use std::{path::PathBuf, process::Stdio, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::{
  io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
  process::{Child, ChildStdin, ChildStdout, Command},
  time::timeout,
};

use loops_domain::{
  config::AgentConfig,
  events::RunEvent,
  model::{
    ArchitectOutput, CompletedWorkUnit, ReconcileResult, RequirementCatalog, VerificationReport,
    WorkUnit, WorkerEvent, WorkerRole, WorkerSummary,
  },
};

use crate::{
  backend::{AgentBackend, BackendContext},
  prompts::full_role_prompt,
  skills,
};

const YIELD_TOOL: &str = "loops_yield";

#[derive(Default)]
pub struct OmpRpcBackend;

#[async_trait]
impl AgentBackend for OmpRpcBackend {
  async fn architect(&self, ctx: &BackendContext, spec: &str) -> Result<ArchitectOutput> {
    let prompt = format!(
            "Create the authoritative requirement catalog for the product spec below.\n\n--- BEGIN SPEC ---\n{spec}\n--- END SPEC ---"
        );
    self
      .run_typed(ctx, WorkerRole::Architect, &prompt, architect_schema())
      .await
  }

  async fn reconcile(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
    recent_completed: &[CompletedWorkUnit],
  ) -> Result<ReconcileResult> {
    let prompt = format!(
            "Reconcile the current repository against this requirement catalog. Inspect the repository directly before deciding.\n\nRequirement catalog:\n{}\n\nRecent completed work-unit claims (verify them; do not trust them):\n{}",
            serde_json::to_string_pretty(catalog)?,
            serde_json::to_string_pretty(&recent_completed.iter().rev().take(5).collect::<Vec<_>>())?,
        );
    self
      .run_typed(ctx, WorkerRole::Reconcile, &prompt, reconcile_schema())
      .await
  }

  async fn implement(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
    work_unit: &WorkUnit,
  ) -> Result<WorkerSummary> {
    let prompt = format!(
      "Implement this work unit now.\n\nWork unit:\n{}\n\nRequirement catalog:\n{}",
      serde_json::to_string_pretty(work_unit)?,
      serde_json::to_string_pretty(catalog)?,
    );
    self
      .run_typed(ctx, WorkerRole::Implement, &prompt, worker_summary_schema())
      .await
  }

  async fn repair(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
    work_unit: &WorkUnit,
    report: &VerificationReport,
  ) -> Result<WorkerSummary> {
    let prompt = format!(
            "Repair the assigned work unit based on this deterministic verification failure.\n\nWork unit:\n{}\n\nVerification report:\n{}\n\nRequirement catalog:\n{}",
            serde_json::to_string_pretty(work_unit)?,
            serde_json::to_string_pretty(report)?,
            serde_json::to_string_pretty(catalog)?,
        );
    self
      .run_typed(ctx, WorkerRole::Repair, &prompt, worker_summary_schema())
      .await
  }

  async fn assess(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
  ) -> Result<ReconcileResult> {
    let prompt = format!(
            "Perform an independent final assessment of the repository against every requirement.\n\nRequirement catalog:\n{}",
            serde_json::to_string_pretty(catalog)?,
        );
    self
      .run_typed(ctx, WorkerRole::Assess, &prompt, reconcile_schema())
      .await
  }
}

impl OmpRpcBackend {
  async fn run_typed<T: DeserializeOwned>(
    &self,
    ctx: &BackendContext,
    role: WorkerRole,
    prompt: &str,
    schema: Value,
  ) -> Result<T> {
    let limit = Duration::from_secs(ctx.config.agent.turn_timeout_secs);
    match timeout(limit, self.run_typed_inner(ctx, role, prompt, schema)).await {
      Ok(result) => result,
      Err(_) => {
        let message = format!(
          "{} worker timed out after {}s",
          role.as_str(),
          limit.as_secs()
        );
        ctx
          .events
          .worker(WorkerEvent::End {
            role,
            at: now(),
            ok: false,
            message: Some(message.clone()),
          })
          .await;
        Err(anyhow!(message))
      }
    }
  }

  async fn run_typed_inner<T: DeserializeOwned>(
    &self,
    ctx: &BackendContext,
    role: WorkerRole,
    prompt: &str,
    schema: Value,
  ) -> Result<T> {
    let resolved = skills::resolve(&ctx.cwd, &ctx.config.skills, role)?;
    let global_agent_dir = global_omp_agent_dir()?;
    let worker_environment =
      skills::prepare_worker_environment(&ctx.runtime_dir, role, &resolved, &global_agent_dir)
        .await?;
    ctx
      .events
      .worker(WorkerEvent::Start {
        role,
        at: now(),
        skills: resolved.names(),
      })
      .await;

    let mut rpc = RpcProcess::spawn(ctx, role, &worker_environment, &resolved).await?;
    let result = async {
      rpc.initialize_protocol().await?;
            rpc.install_yield_tool(schema).await?;
            rpc.send(&json!({"id":"prompt-1","type":"prompt","message":prompt})).await?;

            let mut yielded: Option<T> = None;
            let mut reminders = 0u8;
            let mut agent_started = false;

            loop {
                let frame = tokio::select! {
                    _ = ctx.cancel.cancelled() => {
                        let _ = rpc.send(&json!({"id":"abort-1","type":"abort"})).await;
                        bail!("run cancelled");
                    }
                    frame = rpc.next_frame() => frame?,
                };

                let kind = frame.get("type").and_then(Value::as_str).unwrap_or("");
                match kind {
                    "response" => {
                        if frame.get("success") == Some(&Value::Bool(false)) {
                            let command = frame.get("command").and_then(Value::as_str).unwrap_or("rpc command");
                            let error = frame.get("error").and_then(Value::as_str).unwrap_or("unknown RPC error");
                            bail!("OMP {command} failed: {error}");
                        }
                        if frame.get("id").and_then(Value::as_str) == Some("prompt-1")
                            && frame.pointer("/data/agentInvoked") == Some(&Value::Bool(false))
                        {
                            bail!("OMP accepted the worker prompt but did not invoke the agent");
                        }
                    }
                    "agent_start" => agent_started = true,
                    "prompt_result" => {
                        if frame.get("id").and_then(Value::as_str) == Some("prompt-1")
                            && frame.get("agentInvoked") == Some(&Value::Bool(false))
                        {
                            bail!("OMP resolved the worker prompt without invoking the agent");
                        }
                    }
                    "message_update" => {
                        if frame.pointer("/assistantMessageEvent/type").and_then(Value::as_str) == Some("text_delta") {
                            if let Some(delta) = frame.pointer("/assistantMessageEvent/delta").and_then(Value::as_str) {
                                ctx.events.worker(WorkerEvent::Text { role, at: now(), delta: delta.into() }).await;
                            }
                        }
                    }
                    "tool_execution_start" => {
                        let tool_name = frame.get("toolName").and_then(Value::as_str).unwrap_or("tool").to_owned();
                        if tool_name != YIELD_TOOL {
                            let args = frame.get("args").cloned().unwrap_or(Value::Null);
                            ctx.events.worker(WorkerEvent::ToolStart { role, at: now(), tool_name, args }).await;
                        }
                    }
                    "tool_execution_end" => {
                        let tool_name = frame.get("toolName").and_then(Value::as_str).unwrap_or("tool").to_owned();
                        if tool_name != YIELD_TOOL {
                            let is_error = frame.get("isError").and_then(Value::as_bool).unwrap_or(false);
                            let output = extract_tool_output(frame.get("result"));
                            ctx.events.worker(WorkerEvent::ToolEnd { role, at: now(), tool_name, is_error, output }).await;
                        }
                    }
                    "host_tool_call" => {
                        if frame.get("toolName").and_then(Value::as_str) != Some(YIELD_TOOL) {
                            rpc.reject_host_tool(&frame, "unknown host-owned tool").await?;
                            continue;
                        }
                        let args = frame.get("arguments").cloned().unwrap_or(Value::Null);
                        if yielded.is_some() {
                            rpc.reject_host_tool(&frame, "loops_yield was already accepted for this worker").await?;
                            continue;
                        }
                        match serde_json::from_value::<T>(args) {
                            Ok(value) => {
                                yielded = Some(value);
                                rpc.accept_host_tool(&frame).await?;
                            }
                            Err(error) => {
                                rpc.reject_host_tool(&frame, &format!("structured output does not match schema: {error}")).await?;
                            }
                        }
                    }
                    "extension_ui_request" => rpc.answer_headless_ui(&frame).await?,
                    "extension_error" => {
                        let message = frame.get("error").and_then(Value::as_str).unwrap_or("extension error");
                        bail!("OMP extension error during headless worker: {message}");
                    }
                    "agent_end" => {
                        let terminal = frame.get("isTerminal").and_then(Value::as_bool).unwrap_or(true);
                        if !terminal { continue; }
                        if let Some(value) = yielded.take() {
                            return Ok(value);
                        }
                        if !agent_started {
                            bail!("OMP worker ended without starting an agent turn");
                        }
                        if reminders >= 2 {
                            bail!("{} worker ended without calling {YIELD_TOOL}", role.as_str());
                        }
                        reminders += 1;
                        let id = format!("reminder-{reminders}");
                        rpc.send(&json!({
                            "id": id,
                            "type": "prompt",
                            "message": format!("Structured completion reminder {reminders}/2: finish by calling `{YIELD_TOOL}` with an object matching its schema. Do not answer with prose only.")
                        })).await?;
                    }
                    _ => {}
                }
            }
        }
        .await;

    let shutdown_error = rpc.shutdown().await.err();
    let (ok, message) = match &result {
      Ok(_) => (true, None),
      Err(error) => (false, Some(error.to_string())),
    };
    ctx
      .events
      .worker(WorkerEvent::End {
        role,
        at: now(),
        ok,
        message,
      })
      .await;
    if result.is_ok() {
      if let Some(error) = shutdown_error {
        ctx
          .events
          .emit(RunEvent::Message(format!(
            "OMP worker shutdown warning: {error:#}"
          )))
          .await;
      }
    }
    result
  }
}

fn apply_role_selection(command: &mut Command, agent: &AgentConfig, role: WorkerRole) {
  command.arg("--thinking").arg(agent.thinking_for(role));
  if let Some(model) = agent.model_for(role) {
    command.arg("--model").arg(model);
  }
}

struct RpcProcess {
  child: Child,
  stdin: Option<ChildStdin>,
  lines: Lines<BufReader<ChildStdout>>,
  stderr_task: tokio::task::JoinHandle<Result<String>>,
  max_reassembled_frame_bytes: usize,
}

impl RpcProcess {
  async fn spawn(
    ctx: &BackendContext,
    role: WorkerRole,
    worker_environment: &std::path::Path,
    resolved_skills: &skills::ResolvedSkills,
  ) -> Result<Self> {
    let tools = match role {
      WorkerRole::Architect | WorkerRole::Reconcile | WorkerRole::Assess => {
        &ctx.config.agent.read_only_tools
      }
      WorkerRole::Implement | WorkerRole::Repair => &ctx.config.agent.coding_tools,
    };
    let mut command = Command::new(&ctx.config.agent.command);
    command.args(["--mode", "rpc", "--no-session"]);
    apply_role_selection(&mut command, &ctx.config.agent, role);
    command
      .arg("--tools")
      .arg(tools.join(","))
      .arg("--no-extensions")
      .arg("--skills")
      .arg(resolved_skills.omp_names()?.join(","))
      .arg("--no-rules")
      .arg("--append-system-prompt")
      .arg(full_role_prompt(role));
    if ctx.config.agent.auto_approve {
      command.arg("--yolo");
    }
    command.args(&ctx.config.agent.extra_args);
    command
      .current_dir(&ctx.cwd)
      .env("PI_RPC_EMIT_TITLE", "0")
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .kill_on_drop(true);
    command.env("PI_CODING_AGENT_DIR", worker_environment);

    let mut child = command
      .spawn()
      .with_context(|| format!("spawn `{}` RPC worker", ctx.config.agent.command))?;
    let stdin = child.stdin.take().context("OMP RPC stdin unavailable")?;
    let stdout = child.stdout.take().context("OMP RPC stdout unavailable")?;
    let stderr = child.stderr.take().context("OMP RPC stderr unavailable")?;
    let stderr_task = tokio::spawn(async move {
      let mut reader = BufReader::new(stderr);
      let mut text = String::new();
      tokio::io::AsyncReadExt::read_to_string(&mut reader, &mut text).await?;
      Ok(text)
    });
    Ok(Self {
      child,
      stdin: Some(stdin),
      lines: BufReader::new(stdout).lines(),
      stderr_task,
      max_reassembled_frame_bytes: 64 * 1024 * 1024,
    })
  }

  async fn initialize_protocol(&mut self) -> Result<()> {
    let ready = self.next_frame().await?;
    if ready.get("type").and_then(Value::as_str) != Some("ready") {
      bail!("expected OMP RPC ready frame, got {ready}");
    }
    if let Some(max) = ready
      .get("maxReassembledFrameBytes")
      .and_then(Value::as_u64)
    {
      self.max_reassembled_frame_bytes =
        usize::try_from(max).unwrap_or(self.max_reassembled_frame_bytes);
    }
    let supports_v2 = ready
      .get("supportedProtocolVersions")
      .and_then(Value::as_array)
      .map(|items| items.iter().any(|v| v.as_u64() == Some(2)))
      .unwrap_or(false);
    if supports_v2 {
      self
        .send(&json!({"id":"protocol-v2","type":"negotiate_protocol","protocolVersion":2}))
        .await?;
      self.wait_success("protocol-v2").await?;
    }
    Ok(())
  }

  async fn install_yield_tool(&mut self, parameters: Value) -> Result<()> {
    self.send(&json!({
            "id":"host-tools-1",
            "type":"set_host_tools",
            "tools":[{
                "name":YIELD_TOOL,
                "label":"Complete loops worker",
                "description":"Return the structured terminal result for the current loops worker. Call exactly once after the assigned work is complete.",
                "parameters":parameters,
                "loadMode":"essential"
            }]
        })).await?;
    self.wait_success("host-tools-1").await
  }

  async fn wait_success(&mut self, id: &str) -> Result<()> {
    loop {
      let frame = self.next_frame().await?;
      if frame.get("type").and_then(Value::as_str) != Some("response") {
        continue;
      }
      let response_id = frame.get("id").and_then(Value::as_str);
      if response_id == Some(id) {
        if frame.get("success") == Some(&Value::Bool(true)) {
          return Ok(());
        }
        bail!(
          "OMP RPC command failed: {}",
          frame
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
        );
      }
      if frame.get("success") == Some(&Value::Bool(false)) && response_id.is_none() {
        bail!(
          "OMP RPC returned an uncorrelated command error while waiting for {id}: {}",
          frame
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
        );
      }
    }
  }

  async fn send(&mut self, value: &Value) -> Result<()> {
    let stdin = self.stdin.as_mut().context("OMP RPC stdin closed")?;
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await?;
    stdin.flush().await?;
    Ok(())
  }

  async fn next_frame(&mut self) -> Result<Value> {
    let line = self
      .lines
      .next_line()
      .await?
      .ok_or_else(|| anyhow!("OMP RPC stdout closed"))?;
    let value: Value =
      serde_json::from_str(&line).with_context(|| format!("parse OMP RPC frame: {line}"))?;
    if value.get("type").and_then(Value::as_str) != Some("rpc_chunk") {
      return Ok(value);
    }
    self.reassemble_chunks(value).await
  }

  async fn reassemble_chunks(&mut self, first: Value) -> Result<Value> {
    let chunk_id = first
      .get("chunkId")
      .and_then(Value::as_str)
      .context("rpc_chunk missing chunkId")?
      .to_owned();
    let count = first
      .get("count")
      .and_then(Value::as_u64)
      .context("rpc_chunk missing count")? as usize;
    let byte_length = first
      .get("byteLength")
      .and_then(Value::as_u64)
      .context("rpc_chunk missing byteLength")? as usize;
    if count == 0 {
      bail!("OMP RPC chunk sequence has count=0");
    }
    if byte_length > self.max_reassembled_frame_bytes {
      bail!(
        "OMP RPC logical frame {byte_length} exceeds configured reassembly limit {}",
        self.max_reassembled_frame_bytes
      );
    }
    let mut encoded = Vec::with_capacity(count);
    encoded.push(chunk_data(&first, &chunk_id, 0, count)?);
    for index in 1..count {
      let line = self
        .lines
        .next_line()
        .await?
        .ok_or_else(|| anyhow!("OMP RPC stdout closed mid-chunk"))?;
      let value: Value = serde_json::from_str(&line)?;
      encoded.push(chunk_data(&value, &chunk_id, index, count)?);
    }
    let mut bytes = Vec::with_capacity(byte_length);
    for part in encoded {
      let decoded = base64::engine::general_purpose::STANDARD.decode(part)?;
      bytes.extend(decoded);
    }
    if bytes.len() != byte_length {
      bail!(
        "OMP RPC reassembled frame length mismatch: expected {byte_length}, got {}",
        bytes.len()
      );
    }
    let text = std::str::from_utf8(&bytes).context("OMP RPC chunk payload is not UTF-8")?;
    serde_json::from_str(text).context("parse reassembled OMP RPC frame")
  }

  async fn accept_host_tool(&mut self, frame: &Value) -> Result<()> {
    let id = frame
      .get("id")
      .and_then(Value::as_str)
      .context("host_tool_call missing id")?;
    self
      .send(&json!({
          "type":"host_tool_result",
          "id":id,
          "result":{"content":[{"type":"text","text":"Structured completion accepted by loops."}]}
      }))
      .await
  }

  async fn reject_host_tool(&mut self, frame: &Value, message: &str) -> Result<()> {
    let id = frame
      .get("id")
      .and_then(Value::as_str)
      .context("host_tool_call missing id")?;
    self
      .send(&json!({
          "type":"host_tool_result",
          "id":id,
          "isError":true,
          "result":{"content":[{"type":"text","text":message}]}
      }))
      .await
  }

  async fn answer_headless_ui(&mut self, frame: &Value) -> Result<()> {
    let Some(id) = frame.get("id").and_then(Value::as_str) else {
      return Ok(());
    };
    let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
    match method {
      "confirm" => {
        self
          .send(&json!({"type":"extension_ui_response","id":id,"confirmed":false}))
          .await?
      }
      "select" | "input" | "editor" | "cancel" | "open_url" => {
        self
          .send(&json!({"type":"extension_ui_response","id":id,"cancelled":true}))
          .await?
      }
      _ => {}
    }
    Ok(())
  }

  async fn shutdown(mut self) -> Result<()> {
    self.stdin.take();
    match timeout(Duration::from_secs(2), self.child.wait()).await {
      Ok(status) => {
        let status = status?;
        let stderr = self.stderr_task.await??;
        if !status.success() && !stderr.trim().is_empty() {
          bail!("OMP RPC exited {status}: {}", stderr.trim());
        }
      }
      Err(_) => {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
      }
    }
    Ok(())
  }
}

fn chunk_data(
  frame: &Value,
  chunk_id: &str,
  expected_index: usize,
  expected_count: usize,
) -> Result<String> {
  if frame.get("type").and_then(Value::as_str) != Some("rpc_chunk") {
    bail!("interleaved non-chunk frame during RPC reassembly");
  }
  if frame.get("chunkId").and_then(Value::as_str) != Some(chunk_id) {
    bail!("interleaved rpc_chunk sequence");
  }
  if frame.get("index").and_then(Value::as_u64) != Some(expected_index as u64) {
    bail!("rpc_chunk index mismatch");
  }
  if frame.get("count").and_then(Value::as_u64) != Some(expected_count as u64) {
    bail!("rpc_chunk count mismatch");
  }
  Ok(
    frame
      .get("data")
      .and_then(Value::as_str)
      .context("rpc_chunk missing data")?
      .to_owned(),
  )
}

fn extract_tool_output(value: Option<&Value>) -> Option<String> {
  let value = value?;
  let content = value.get("content")?;
  let mut text = String::new();
  match content {
    Value::String(v) => text.push_str(v),
    Value::Array(items) => {
      for item in items {
        if let Some(v) = item
          .get("text")
          .and_then(Value::as_str)
          .or_else(|| item.get("content").and_then(Value::as_str))
        {
          if !text.is_empty() {
            text.push('\n');
          }
          text.push_str(v);
        }
      }
    }
    _ => {}
  }
  let text = text.trim();
  if text.is_empty() {
    None
  } else if text.len() > 6000 {
    let end = utf8_floor(text, 6000);
    Some(format!("{}\n… output truncated", &text[..end]))
  } else {
    Some(text.to_owned())
  }
}

fn utf8_floor(text: &str, max_bytes: usize) -> usize {
  let mut end = max_bytes.min(text.len());
  while end > 0 && !text.is_char_boundary(end) {
    end -= 1;
  }
  end
}

fn now() -> String {
  chrono::Utc::now().to_rfc3339()
}

fn global_omp_agent_dir() -> Result<PathBuf> {
  if let Some(path) = std::env::var_os("PI_CODING_AGENT_DIR") {
    return Ok(path.into());
  }
  let home = std::env::var_os("HOME").context("resolve home directory for global OMP state")?;
  Ok(PathBuf::from(home).join(".omp").join("agent"))
}

fn string_array_schema() -> Value {
  json!({"type":"array","items":{"type":"string"}})
}

fn requirement_schema() -> Value {
  json!({
      "type":"object",
      "additionalProperties":false,
      "properties":{
          "id":{"type":"string"},
          "title":{"type":"string"},
          "description":{"type":"string"},
          "acceptanceCriteria":string_array_schema()
      },
      "required":["id","title","description","acceptanceCriteria"]
  })
}

fn architect_schema() -> Value {
  json!({
      "type":"object",
      "additionalProperties":false,
      "properties":{"requirements":{"type":"array","items":requirement_schema()}},
      "required":["requirements"]
  })
}

fn work_unit_schema() -> Value {
  json!({
      "type":"object",
      "additionalProperties":false,
      "properties":{
          "id":{"type":"string"},
          "title":{"type":"string"},
          "objective":{"type":"string"},
          "requirementIds":string_array_schema(),
          "acceptanceCriteria":string_array_schema(),
          "suggestedChecks":string_array_schema()
      },
      "required":["id","title","objective","requirementIds","acceptanceCriteria","suggestedChecks"]
  })
}

fn reconcile_schema() -> Value {
  json!({
      "type":"object",
      "additionalProperties":false,
      "properties":{
          "complete":{"type":"boolean"},
          "summary":{"type":"string"},
          "requirements":{
              "type":"array",
              "items":{
                  "type":"object",
                  "additionalProperties":false,
                  "properties":{
                      "id":{"type":"string"},
                      "status":{"type":"string","enum":["satisfied","partial","missing"]},
                      "evidence":string_array_schema(),
                      "gaps":string_array_schema()
                  },
                  "required":["id","status","evidence","gaps"]
              }
          },
          "nextWorkUnit":{"anyOf":[work_unit_schema(),{"type":"null"}]}
      },
      "required":["complete","summary","requirements","nextWorkUnit"]
  })
}

fn worker_summary_schema() -> Value {
  json!({
      "type":"object",
      "additionalProperties":false,
      "properties":{
          "summary":{"type":"string"},
          "changedFiles":string_array_schema(),
          "testsRun":string_array_schema(),
          "notes":string_array_schema(),
          "decisions":string_array_schema(),
          "discoveries":string_array_schema(),
          "risks":string_array_schema(),
          "followUps":string_array_schema()
      },
      "required":["summary","changedFiles","testsRun","notes"]
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn schema_accepts_deserializable_architect_shape() {
    let value = json!({"requirements":[{"id":"REQ-001","title":"x","description":"y","acceptanceCriteria":["z"]}]});
    let parsed: ArchitectOutput = serde_json::from_value(value).unwrap();
    assert_eq!(parsed.requirements[0].id, "REQ-001");
  }

  #[test]
  fn chunk_validator_rejects_interleaving() {
    let frame = json!({"type":"rpc_chunk","chunkId":"a","index":0,"count":2,"data":"e30="});
    assert!(chunk_data(&frame, "b", 0, 2).is_err());
  }

  fn role_selection_args(agent: &AgentConfig, role: WorkerRole) -> Vec<String> {
    let mut command = Command::new("omp");
    apply_role_selection(&mut command, agent, role);
    command
      .as_std()
      .get_args()
      .map(|argument| argument.to_string_lossy().into_owned())
      .collect()
  }

  #[test]
  fn command_selection_uses_independent_role_model_and_thinking() {
    let mut config = loops_domain::config::Config::default();
    config.agent.roles.architect.model = Some("architect-model".into());
    config.agent.roles.architect.thinking = Some("xhigh".into());
    config.agent.roles.implement.model = Some("implementation-model".into());
    config.agent.roles.implement.thinking = Some("low".into());

    assert_eq!(
      role_selection_args(&config.agent, WorkerRole::Architect),
      ["--thinking", "xhigh", "--model", "architect-model"],
    );
    assert_eq!(
      role_selection_args(&config.agent, WorkerRole::Implement),
      ["--thinking", "low", "--model", "implementation-model"],
    );
  }

  #[test]
  fn command_selection_limits_model_override_to_configured_role() {
    let mut config = loops_domain::config::Config::default();
    config.agent.roles.implement.model = Some("implementation-model".into());

    assert_eq!(
      role_selection_args(&config.agent, WorkerRole::Assess),
      ["--thinking", "high"],
    );
    assert_eq!(
      role_selection_args(&config.agent, WorkerRole::Implement),
      ["--thinking", "high", "--model", "implementation-model"],
    );
  }
}
