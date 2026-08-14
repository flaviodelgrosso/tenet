use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_client_protocol::mcp_server::{McpConnectionTo, McpServer, McpServerConnect};
use agent_client_protocol::role;
use agent_client_protocol::schema::{
  v1::{
    AuthCapabilities, AuthMethod, AuthenticateRequest, ClientCapabilities, ContentBlock,
    Implementation, InitializeRequest, InitializeResponse, PermissionOptionKind,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOption, SessionConfigSelectOptions, SessionNotification, SessionUpdate,
    SetSessionConfigOptionRequest, ToolCall, ToolCallId, ToolCallStatus,
  },
  ProtocolVersion,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{
  AcpAgent, AcpAgentConfig, Agent, Client, ConnectionTo, Dispatch, DynConnectTo, Handled, NullRun,
  SessionMessage, UntypedMessage,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use loops_domain::events::EventSink;
use loops_domain::model::{
  ArchitectOutput, CompletedWorkUnit, Discovery, ReconcileResult, RequirementCatalog,
  VerificationReport, WorkUnit, WorkerEvent, WorkerRole, WorkerSummary,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::registry::RegistryClient;
use loops_runtime::backend::{
  AgentBackend, AgentRuntime, BackendContext, LaunchMetadata, WorkerOutputValidator, WorkerRequest,
  WorkerResult,
};

use crate::prompts::full_role_prompt;

/// The sole production worker runtime. Agents are external ACP endpoints; role procedures and
/// completion semantics remain owned by Loops.
pub struct AcpRuntime;

/// Safe, displayable authentication mechanism advertised by an ACP agent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcpAuthMechanism {
  /// The agent performs its own authentication flow.
  Agent,
  /// The client would have to supply an environment variable.
  EnvVar,
  /// The client would have to host an interactive terminal flow.
  Terminal,
  /// An authentication mechanism newer than this client supports.
  Unsupported,
}

impl AcpAuthMechanism {
  /// A concise name suitable for CLI output.
  pub fn label(self) -> &'static str {
    match self {
      Self::Agent => "agent",
      Self::EnvVar => "environment variable",
      Self::Terminal => "interactive terminal",
      Self::Unsupported => "an unsupported client mechanism",
    }
  }
}

/// A non-secret description of one authentication method advertised at initialization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcpAuthMethod {
  pub id: String,
  pub name: String,
  pub description: Option<String>,
  pub mechanism: AcpAuthMechanism,
}

/// Agent identity and authentication availability obtained from an ACP initialization handshake.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcpReadiness {
  pub agent_name: Option<String>,
  pub agent_version: Option<String>,
  pub auth_methods: Vec<AcpAuthMethod>,
}

#[async_trait]
impl AgentBackend for AcpRuntime {
  async fn resolve_launch(
    &self,
    cwd: &std::path::Path,
    config: &loops_domain::config::Config,
  ) -> Result<Option<LaunchMetadata>> {
    let Some(id) = config.agent.id.as_deref() else {
      return Ok(None);
    };

    let cache = cwd.join(loops_domain::config::LOOPS_DIR).join("registry");
    Ok(Some(
      RegistryClient::default().resolve(&cache, id).await?.launch,
    ))
  }

  async fn architect(&self, ctx: &BackendContext, spec: &str) -> Result<ArchitectOutput> {
    self
      .run_typed(
        ctx,
        WorkerRole::Architect,
        format!(
          "Create the authoritative requirement catalog for the product spec below.\n\n{spec}"
        ),
        architect_schema(),
      )
      .await
  }

  async fn reconcile(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
    recent: &[CompletedWorkUnit],
    discoveries: &[Discovery],
  ) -> Result<ReconcileResult> {
    self.run_typed(ctx, WorkerRole::Reconcile, format!("Reconcile the repository against this catalog. Inspect it directly. Propose a dependency graph of candidate work units; the controller alone decides concurrency.\n\nCatalog:\n{}\n\nRecent completed work:\n{}\n\nWorker discoveries requiring reconsideration:\n{}", serde_json::to_string_pretty(catalog)?, serde_json::to_string_pretty(recent)?, serde_json::to_string_pretty(discoveries)?), reconcile_schema()).await
  }

  async fn implement(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
    unit: &WorkUnit,
  ) -> Result<WorkerSummary> {
    self
      .run_typed(
        ctx,
        WorkerRole::Implement,
        format!(
          "Implement this work unit now.\n\nWork unit:\n{}\n\nCatalog:\n{}",
          serde_json::to_string_pretty(unit)?,
          serde_json::to_string_pretty(catalog)?
        ),
        worker_summary_schema(),
      )
      .await
  }

  async fn repair(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
    unit: &WorkUnit,
    report: &VerificationReport,
  ) -> Result<WorkerSummary> {
    self.run_typed(ctx, WorkerRole::Repair, format!("Repair the assigned work unit from this deterministic verification report.\n\nWork unit:\n{}\n\nReport:\n{}\n\nCatalog:\n{}", serde_json::to_string_pretty(unit)?, serde_json::to_string_pretty(report)?, serde_json::to_string_pretty(catalog)?), worker_summary_schema()).await
  }

  async fn assess(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
  ) -> Result<ReconcileResult> {
    self
      .run_typed(
        ctx,
        WorkerRole::Assess,
        format!(
          "Perform an independent final assessment against every requirement.\n\nCatalog:\n{}",
          serde_json::to_string_pretty(catalog)?
        ),
        reconcile_schema(),
      )
      .await
  }
}

impl AcpRuntime {
  /// Starts the configured ACP agent and performs only the initialization handshake.
  ///
  /// Custom environment values are passed directly to the child process and are never included in
  /// the returned metadata.
  pub async fn readiness(
    &self,
    launch: Option<LaunchMetadata>,
    custom: Option<loops_domain::config::CustomAgentConfig>,
  ) -> Result<AcpReadiness> {
    let agent = configured_agent(launch, custom)?;
    Client
      .builder()
      .name("loops")
      .connect_with(agent, async move |connection: ConnectionTo<Agent>| {
        let initialize = connection
          .send_request(initialize_request())
          .block_task()
          .await?;
        Ok(readiness_from_initialize(&initialize))
      })
      .await
      .map_err(|error| anyhow!("ACP initialization failed: {error}"))
  }

  /// Invokes an advertised agent-owned authentication method.
  ///
  /// Environment-variable and client-terminal methods are deliberately rejected because Loops
  /// neither accepts credentials nor hosts an authentication terminal.
  pub async fn login(
    &self,
    launch: Option<LaunchMetadata>,
    custom: Option<loops_domain::config::CustomAgentConfig>,
    method_id: &str,
  ) -> Result<AcpReadiness> {
    let agent = configured_agent(launch, custom)?;
    let method_id = method_id.to_owned();
    Client
      .builder()
      .name("loops")
      .connect_with(agent, async move |connection: ConnectionTo<Agent>| {
        let initialize = connection
          .send_request(initialize_request())
          .block_task()
          .await?;
        let readiness = readiness_from_initialize(&initialize);
        let method = readiness
          .auth_methods
          .iter()
          .find(|method| method.id == method_id)
          .ok_or_else(|| {
            agent_client_protocol::Error::new(
              -32000,
              format!("authentication method {method_id:?} was not advertised by this agent"),
            )
          })?;
        if method.mechanism != AcpAuthMechanism::Agent {
          return Err(agent_client_protocol::Error::new(
            -32000,
            format!(
              "authentication method {method_id:?} requires {}; Loops supports only agent-owned authentication",
              method.mechanism.label()
            ),
          ));
        }
        connection
          .send_request(AuthenticateRequest::new(method_id))
          .block_task()
          .await?;
        Ok(readiness)
      })
      .await
      .map_err(|error| anyhow!("ACP authentication failed: {error}"))
  }

  async fn run_typed<T: DeserializeOwned + Send + 'static>(
    &self,
    ctx: &BackendContext,
    role: WorkerRole,
    prompt: String,
    schema: Value,
  ) -> Result<T> {
    let validator = Arc::new(|value: &Value| {
      serde_json::from_value::<T>(value.clone())
        .map(|_| ())
        .map_err(|error| anyhow!("{error}"))
    });

    let request = WorkerRequest {
      role,
      worker_id: ctx.worker_id.clone(),
      lease_id: ctx.lease_id.clone(),
      work_unit_id: ctx.work_unit_id.clone(),
      prompt: build_worker_prompt(role, &schema, &prompt)?,
      cwd: ctx.cwd.clone(),
      runtime_dir: ctx.runtime_dir.clone(),
      schema,
      validate_output: validator,
      preferences: ctx.config.agent.preferences_for(role),
      timeout: Duration::from_secs(ctx.config.agent.turn_timeout_secs),
      launch: ctx.launch.clone(),
      custom: ctx.config.agent.custom.clone(),
      completion_retries: ctx.config.agent.completion_retries,
    };
    ctx
      .events
      .worker(WorkerEvent::Start {
        role,
        worker_id: ctx.worker_id.clone(),
        lease_id: ctx.lease_id.clone(),
        work_unit_id: ctx.work_unit_id.clone(),
        at: chrono::Utc::now().to_rfc3339(),
      })
      .await;
    let timeout = request.timeout;
    let run = self.run_worker_with_events(request, Some(ctx.events.clone()));
    let outcome = tokio::time::timeout(timeout, async {
      tokio::select! {
        result = run => result,
        _ = ctx.cancel.cancelled() => Err(anyhow!("run cancelled")),
      }
    })
    .await
    .map_err(|_| {
      anyhow!(
        "{} worker timed out after {}s",
        role.as_str(),
        timeout.as_secs()
      )
    })?;

    let (ok, message) = match &outcome {
      Ok(result) => match serde_json::from_value::<T>(result.structured_output.clone()) {
        Ok(_) => (true, None),
        Err(error) => (
          false,
          Some(format!(
            "structured output did not match the requested schema: {error}"
          )),
        ),
      },
      Err(error) => (false, Some(error.to_string())),
    };

    ctx
      .events
      .worker(WorkerEvent::End {
        role,
        worker_id: ctx.worker_id.clone(),
        lease_id: ctx.lease_id.clone(),
        work_unit_id: ctx.work_unit_id.clone(),
        at: chrono::Utc::now().to_rfc3339(),
        ok,
        message,
      })
      .await;
    match outcome {
      Ok(result) => serde_json::from_value::<T>(result.structured_output)
        .map_err(|error| anyhow!("worker response did not match the requested schema: {error}")),
      Err(error) => Err(error),
    }
  }
}

fn configured_agent(
  launch: Option<LaunchMetadata>,
  custom: Option<loops_domain::config::CustomAgentConfig>,
) -> Result<AcpAgent> {
  let (command, args, env) = match (launch, custom) {
    (Some(launch), None) => (launch.command, launch.args, launch.env),
    (None, Some(custom)) => (custom.command, custom.args, custom.env),
    (Some(_), Some(_)) => {
      return Err(anyhow!(
        "ambiguous ACP launch source: choose either a Registry launch or agent.custom"
      ));
    }
    (None, None) => {
      return Err(anyhow!(
        "no ACP launch source resolved; select a Registry agent or configure agent.custom.command"
      ));
    }
  };

  let mut config = AcpAgentConfig::new(command).args(args);
  for (name, value) in env {
    config = config.env(name, value);
  }

  Ok(AcpAgent::new(config))
}

fn initialize_request() -> InitializeRequest {
  InitializeRequest::new(ProtocolVersion::V1)
    .client_info(Implementation::new("loops", env!("CARGO_PKG_VERSION")))
    .client_capabilities(ClientCapabilities::new().auth(AuthCapabilities::new().terminal(false)))
}

fn readiness_from_initialize(initialize: &InitializeResponse) -> AcpReadiness {
  AcpReadiness {
    agent_name: initialize
      .agent_info
      .as_ref()
      .map(|info| info.title.clone().unwrap_or_else(|| info.name.clone())),
    agent_version: initialize
      .agent_info
      .as_ref()
      .map(|info| info.version.clone()),
    auth_methods: initialize
      .auth_methods
      .iter()
      .map(|method| AcpAuthMethod {
        id: method.id().to_string(),
        name: method.name().to_owned(),
        description: method.description().map(ToOwned::to_owned),
        mechanism: match method {
          AuthMethod::Agent(_) => AcpAuthMechanism::Agent,
          AuthMethod::EnvVar(_) => AcpAuthMechanism::EnvVar,
          AuthMethod::Terminal(_) => AcpAuthMechanism::Terminal,
          _ => AcpAuthMechanism::Unsupported,
        },
      })
      .collect(),
  }
}

fn validate_json_schema(value: &Value, schema: &Value) -> Result<()> {
  if let Some(variants) = schema.get("anyOf").and_then(Value::as_array) {
    let mut errors = Vec::with_capacity(variants.len());
    for variant in variants {
      match validate_json_schema(value, variant) {
        Ok(()) => return Ok(()),
        Err(error) => errors.push(error.to_string()),
      }
    }
    return Err(anyhow!(
      "did not match any allowed schema: {}",
      errors.join("; ")
    ));
  }

  if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
    if !allowed.contains(value) {
      return Err(anyhow!("value is not one of the allowed values"));
    }
  }

  match schema.get("type").and_then(Value::as_str) {
    Some("object") => {
      let object = value
        .as_object()
        .ok_or_else(|| anyhow!("expected object"))?;
      if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
          if !object.contains_key(name) {
            return Err(anyhow!("missing required property {name:?}"));
          }
        }
      }
      if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
          if let Some(name) = object.keys().find(|name| !properties.contains_key(*name)) {
            return Err(anyhow!("unexpected property {name:?}"));
          }
        }
      }
      if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, property_schema) in properties {
          if let Some(property) = object.get(name) {
            validate_json_schema(property, property_schema)
              .map_err(|error| anyhow!("property {name:?}: {error}"))?;
          }
        }
      }
    }
    Some("array") => {
      let array = value.as_array().ok_or_else(|| anyhow!("expected array"))?;
      if let Some(item_schema) = schema.get("items") {
        for item in array {
          validate_json_schema(item, item_schema)?;
        }
      }
    }
    Some("string") if !value.is_string() => return Err(anyhow!("expected string")),
    Some("boolean") if !value.is_boolean() => return Err(anyhow!("expected boolean")),
    Some("number") if !value.is_number() => return Err(anyhow!("expected number")),
    Some("null") if !value.is_null() => return Err(anyhow!("expected null")),
    _ => {}
  }
  Ok(())
}

struct LoopsYieldServer {
  schema: Value,
  validate_output: WorkerOutputValidator,
  yield_sender: Arc<Mutex<Option<oneshot::Sender<Value>>>>,
  yield_completion_sender: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl McpServerConnect<Agent> for LoopsYieldServer {
  fn name(&self) -> String {
    "loops_yield".into()
  }

  fn connect(&self, _cx: McpConnectionTo<Agent>) -> DynConnectTo<role::mcp::Client> {
    let schema = self.schema.clone();
    let validate_output = self.validate_output.clone();
    let yield_sender = self.yield_sender.clone();
    let yield_completion_sender = self.yield_completion_sender.clone();

    DynConnectTo::new(
      role::mcp::Server.builder().on_receive_dispatch(
        async move |dispatch: Dispatch<UntypedMessage, UntypedMessage>, _connection| {
          match dispatch {
            Dispatch::Request(request, responder) => {
              let mut completion_sender = None;
              let result = match request.method.as_str() {
                "initialize" => Ok(json!({
                  "protocolVersion": "2025-06-18",
                  "capabilities": {"tools": {}},
                  "serverInfo": {"name": "loops_yield", "version": env!("CARGO_PKG_VERSION")},
                })),
                "tools/list" => Ok(json!({
                  "tools": [{
                    "name": "loops_yield",
                    "description": "Submit the one schema-valid structured result for this Loops worker.",
                    "inputSchema": schema,
                  }],
                })),
                "tools/call" => {
                  let sender_completion = yield_completion_sender.lock().map_err(|_| {
                    agent_client_protocol::Error::internal_error()
                      .data("loops_yield completion state lock poisoned")
                  })?.take();
                  let arguments = request.params.get("arguments").cloned().ok_or_else(|| {
                    agent_client_protocol::Error::invalid_params()
                      .data("loops_yield requires an arguments object")
                  })?;
                  let name = request.params.get("name").and_then(Value::as_str);
                  if name != Some("loops_yield") {
                    Err(agent_client_protocol::Error::invalid_params().data("unknown MCP tool"))
                  } else if !arguments.is_object() {
                    Err(agent_client_protocol::Error::invalid_params()
                      .data("loops_yield arguments must be an object"))
                  } else if let Err(error) = validate_json_schema(&arguments, &schema) {
                    Err(agent_client_protocol::Error::invalid_params().data(error.to_string()))
                  } else if let Err(error) = validate_output(&arguments) {
                    Err(agent_client_protocol::Error::invalid_params().data(error.to_string()))
                  } else {
                    let sender = yield_sender.lock().map_err(|_| {
                      agent_client_protocol::Error::internal_error()
                        .data("loops_yield state lock poisoned")
                    })?.take().ok_or_else(|| {
                      agent_client_protocol::Error::invalid_params()
                        .data("loops_yield accepts exactly one result")
                    })?;
                    let sender_completion = sender_completion.ok_or_else(|| {
                      agent_client_protocol::Error::invalid_params()
                        .data("loops_yield accepts exactly one result")
                    })?;
                    sender.send(arguments.clone()).map_err(|_| {
                      agent_client_protocol::Error::internal_error()
                        .data("loops_yield receiver closed before completion")
                    })?;
                    completion_sender = Some(sender_completion);
                    Ok(json!({
                      "content": [{"type": "text", "text": "Loops accepted the structured result."}],
                      "structuredContent": arguments,
                      "isError": false,
                    }))
                  }
                }
                _ => Err(agent_client_protocol::Error::method_not_found()),
              };
              let response_result = responder.respond_with_result(result);
              if let Some(sender) = completion_sender {
                let _ = sender.send(());
              }
              response_result?;
              Ok(Handled::Yes)
            }
            Dispatch::Notification(_) => Ok(Handled::Yes),
            Dispatch::Response(result, router) => {
              router.route_with_result(result)?;
              Ok(Handled::Yes)
            }
          }
        },
        agent_client_protocol::on_receive_dispatch!(),
      ),
    )
  }
}

fn client_mcp_attachable(response: &InitializeResponse) -> bool {
  response.agent_capabilities.mcp_capabilities.acp
}

#[async_trait]
impl AgentRuntime for AcpRuntime {
  async fn run_worker(&self, request: WorkerRequest) -> Result<WorkerResult> {
    self.run_worker_with_events(request, None).await
  }
}

impl AcpRuntime {
  async fn run_worker_with_events(
    &self,
    request: WorkerRequest,
    events: Option<EventSink>,
  ) -> Result<WorkerResult> {
    let identity = WorkerIdentity {
      worker_id: request.worker_id.clone(),
      lease_id: request.lease_id.clone(),
      work_unit_id: request.work_unit_id.clone(),
    };
    let (command, args, env) = if let Some(launch) = request.launch {
      (launch.command, launch.args, launch.env)
    } else if let Some(custom) = request.custom {
      (custom.command, custom.args, custom.env)
    } else {
      return Err(anyhow!(
        "no ACP launch source resolved; select a Registry agent or configure agent.custom.command"
      ));
    };

    let mut agent_config = AcpAgentConfig::new(command).args(args);
    for (name, value) in env {
      agent_config = agent_config.env(name, value);
    }

    let agent = AcpAgent::new(agent_config);
    let prompt = request.prompt;
    let cwd = request.cwd;
    let role = request.role;
    let preferences = request.preferences;
    let retries = request.completion_retries;
    let schema = request.schema;
    let validate_output = request.validate_output;

    Client
      .builder()
      .name("loops")
      .on_receive_request(
        async move |request: RequestPermissionRequest, responder, _connection| {
          let outcome = request
            .options
            .iter()
            .find(|option| option.kind == PermissionOptionKind::AllowOnce)
            .map(|option| {
              RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                option.option_id.clone(),
              ))
            })
            .unwrap_or(RequestPermissionOutcome::Cancelled);
          responder.respond(RequestPermissionResponse::new(outcome))
        },
        agent_client_protocol::on_receive_request!(),
      )
      .connect_with(agent, move |connection: ConnectionTo<Agent>| {
        let prompt = prompt.clone();
        let preferences = preferences.clone();
        let schema = schema.clone();
        let validate_output = validate_output.clone();
        let events = events.clone();
        let identity = identity.clone();
        async move {
          let initialize = connection
            .send_request(initialize_request())
            .block_task()
            .await?;
          if client_mcp_attachable(&initialize) {
            let (yield_sender, yield_receiver) = oneshot::channel();
            let (yield_completion_sender, yield_completion_receiver) = oneshot::channel();
            let server = McpServer::new(
              LoopsYieldServer {
                schema: schema.clone(),
                validate_output: validate_output.clone(),
                yield_sender: Arc::new(Mutex::new(Some(yield_sender))),
                yield_completion_sender: Arc::new(Mutex::new(Some(yield_completion_sender))),
              },
              NullRun,
            );
            let mcp_prompt = prompt.clone();
            let mcp_preferences = preferences.clone();
            let mcp_schema = schema.clone();
            let mcp_validate_output = validate_output.clone();
            let mcp_events = events.clone();
            let mcp_identity = identity.clone();
            match connection.build_session(&cwd).with_mcp_server(server) {
              Ok(builder) => {
                builder
                  .block_task()
                  .run_until(async move |mut session| {
                    complete_worker_session(
                      &mut session,
                      WorkerSessionInput {
                        prompt: mcp_prompt,
                        role,
                        preferences: mcp_preferences,
                        schema: mcp_schema,
                        validate_output: mcp_validate_output,
                        retries,
                        events: mcp_events,
                        identity: mcp_identity,
                        yielded: Some((yield_receiver, yield_completion_receiver)),
                      },
                    )
                    .await
                  })
                  .await
              }
              Err(_) => {
                connection
                  .build_session(&cwd)
                  .block_task()
                  .run_until(async move |mut session| {
                    complete_worker_session(
                      &mut session,
                      WorkerSessionInput {
                        prompt,
                        role,
                        preferences,
                        schema,
                        validate_output,
                        retries,
                        events,
                        identity,
                        yielded: None,
                      },
                    )
                    .await
                  })
                  .await
              }
            }
          } else {
            connection
              .build_session(&cwd)
              .block_task()
              .run_until(async move |mut session| {
                complete_worker_session(
                  &mut session,
                  WorkerSessionInput {
                    prompt,
                    role,
                    preferences,
                    schema,
                    validate_output,
                    retries,
                    events,
                    identity,
                    yielded: None,
                  },
                )
                .await
              })
              .await
          }
        }
      })
      .await
      .map_err(|error| anyhow!("ACP worker failed: {error}"))
  }
}

#[derive(Clone)]
struct WorkerIdentity {
  worker_id: String,
  lease_id: Option<String>,
  work_unit_id: Option<String>,
}

struct WorkerSessionInput {
  prompt: String,
  role: WorkerRole,
  preferences: loops_domain::config::RolePreference,
  schema: Value,
  validate_output: WorkerOutputValidator,
  retries: u32,
  events: Option<EventSink>,
  identity: WorkerIdentity,
  yielded: Option<(oneshot::Receiver<Value>, oneshot::Receiver<()>)>,
}

async fn complete_worker_session<Link>(
  session: &mut agent_client_protocol::ActiveSession<'_, Link>,
  input: WorkerSessionInput,
) -> std::result::Result<WorkerResult, agent_client_protocol::Error>
where
  Link: agent_client_protocol::role::HasPeer<Agent>,
{
  let WorkerSessionInput {
    prompt,
    role,
    preferences,
    schema,
    validate_output,
    retries,
    events,
    identity,
    mut yielded,
  } = input;
  let response = session.response();
  let mut options = response.config_options.unwrap_or_default();
  let has_selector_preference = preferences.model.is_some()
    || preferences.thought_level.is_some()
    || preferences.mode.is_some();
  let mut text = String::new();
  let mut tool_calls = HashMap::new();
  if has_selector_preference {
    if preferences.model.is_some() && options.is_empty() {
      options = read_config_options(
        session,
        role,
        &identity,
        events.clone(),
        &mut text,
        &mut tool_calls,
      )
      .await?
      .unwrap_or_default();
    }
    if !options.is_empty() || preferences.model.is_some() {
      apply_preferences(session, options, &preferences).await?;
    }
  }
  session.send_prompt(prompt)?;
  read_response(
    session,
    role,
    &identity,
    events.clone(),
    &mut text,
    &mut tool_calls,
  )
  .await?;

  if let Some(structured_output) = take_yielded(&mut yielded).await {
    return Ok(WorkerResult { structured_output });
  }

  let mut attempts = 0u32;
  loop {
    match serde_json::from_str::<Value>(&text) {
      Ok(structured_output) => match validate_json_schema(&structured_output, &schema) {
        Ok(()) => match validate_output(&structured_output) {
          Ok(()) => return Ok(WorkerResult { structured_output }),
          Err(error) if attempts < retries => {
            attempts += 1;
            text.clear();
            session.send_prompt(format!("The previous response was valid schema JSON but could not be deserialized for the requested role type ({error}). Reply with only one schema-valid JSON value matching the requested role type; no markdown or prose."))?;
            read_response(
              session,
              role,
              &identity,
              events.clone(),
              &mut text,
              &mut tool_calls,
            )
            .await?;
            if let Some(structured_output) = take_yielded(&mut yielded).await {
              return Ok(WorkerResult { structured_output });
            }
          }
          Err(error) => {
            return Err(agent_client_protocol::Error::internal_error().data(format!(
              "typed structured completion failed after {attempts} retries: {error}"
            )))
          }
        },
        Err(error) if attempts < retries => {
          attempts += 1;
          text.clear();
          session.send_prompt(format!("The previous response was not schema-valid JSON ({error}). Reply with only one schema-valid JSON value; no markdown or prose."))?;
          read_response(
            session,
            role,
            &identity,
            events.clone(),
            &mut text,
            &mut tool_calls,
          )
          .await?;
          if let Some(structured_output) = take_yielded(&mut yielded).await {
            return Ok(WorkerResult { structured_output });
          }
        }
        Err(error) => {
          return Err(agent_client_protocol::Error::internal_error().data(format!(
            "invalid structured completion after {attempts} retries: {error}"
          )))
        }
      },
      Err(error) if attempts < retries => {
        attempts += 1;
        text.clear();
        session.send_prompt(format!("The previous response was not valid whole-response JSON ({error}). Reply with only one schema-valid JSON value; no markdown or prose."))?;
        read_response(
          session,
          role,
          &identity,
          events.clone(),
          &mut text,
          &mut tool_calls,
        )
        .await?;
        if let Some(structured_output) = take_yielded(&mut yielded).await {
          return Ok(WorkerResult { structured_output });
        }
      }
      Err(error) => {
        return Err(agent_client_protocol::Error::internal_error().data(format!(
          "invalid structured completion after {attempts} retries: {error}"
        )))
      }
    }
  }
}

async fn take_yielded(
  yielded: &mut Option<(oneshot::Receiver<Value>, oneshot::Receiver<()>)>,
) -> Option<Value> {
  let (receiver, completion_receiver) = yielded.as_mut()?;
  let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
  let structured_output = match receiver.try_recv() {
    Ok(value) => Some(value),
    Err(_) => match tokio::time::timeout_at(deadline, &mut *receiver).await {
      Ok(Ok(value)) => Some(value),
      _ => None,
    },
  }?;

  let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
  let _ = tokio::time::timeout(remaining, &mut *completion_receiver).await;
  Some(structured_output)
}

async fn read_response<Link>(
  session: &mut agent_client_protocol::ActiveSession<'_, Link>,
  role: WorkerRole,
  identity: &WorkerIdentity,
  events: Option<EventSink>,
  text: &mut String,
  tool_calls: &mut HashMap<ToolCallId, ToolCall>,
) -> std::result::Result<(), agent_client_protocol::Error>
where
  Link: agent_client_protocol::role::HasPeer<Agent>,
{
  loop {
    match session.read_update().await? {
      SessionMessage::StopReason(_) => return Ok(()),
      SessionMessage::SessionMessage(dispatch) => {
        let sink = events.clone();
        MatchDispatch::new(dispatch)
          .if_notification(async |notification: SessionNotification| {
            map_update(notification.update, role, identity, sink, text, tool_calls).await;
            Ok(())
          })
          .await
          .otherwise_ignore()?;
      }
      _ => return Ok(()),
    }
  }
}

async fn map_update(
  update: SessionUpdate,
  role: WorkerRole,
  identity: &WorkerIdentity,
  events: Option<EventSink>,
  text: &mut String,
  tool_calls: &mut HashMap<ToolCallId, ToolCall>,
) {
  match update {
    SessionUpdate::AgentMessageChunk(chunk) => {
      if let ContentBlock::Text(content) = chunk.content {
        text.push_str(&content.text);
        if let Some(events) = events {
          events
            .worker(WorkerEvent::Text {
              role,
              worker_id: identity.worker_id.clone(),
              lease_id: identity.lease_id.clone(),
              work_unit_id: identity.work_unit_id.clone(),
              at: chrono::Utc::now().to_rfc3339(),
              delta: content.text,
            })
            .await;
        }
      }
    }
    SessionUpdate::ToolCall(call) => {
      let tool_call_id = call.tool_call_id.clone();
      let title = call.title.clone();
      let status = call.status;
      let output = call.raw_output.clone();
      let args = call.raw_input.clone().unwrap_or(Value::Null);
      tool_calls.insert(tool_call_id, call);
      if let Some(events) = events {
        events
          .worker(WorkerEvent::ToolStart {
            role,
            worker_id: identity.worker_id.clone(),
            lease_id: identity.lease_id.clone(),
            work_unit_id: identity.work_unit_id.clone(),
            at: chrono::Utc::now().to_rfc3339(),
            tool_name: title.clone(),
            args,
          })
          .await;
        if matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed) {
          events
            .worker(WorkerEvent::ToolEnd {
              role,
              worker_id: identity.worker_id.clone(),
              lease_id: identity.lease_id.clone(),
              work_unit_id: identity.work_unit_id.clone(),
              at: chrono::Utc::now().to_rfc3339(),
              tool_name: title,
              is_error: matches!(status, ToolCallStatus::Failed),
              output: output.map(|value| value.to_string()),
            })
            .await;
        }
      }
    }
    SessionUpdate::ToolCallUpdate(update) => {
      let tool_call_id = update.tool_call_id.clone();
      let (tool_name, status, output) = {
        let call = tool_calls.entry(tool_call_id).or_insert_with(|| {
          ToolCall::new(update.tool_call_id.clone(), update.tool_call_id.to_string())
        });
        call.update(update.fields);
        (call.title.clone(), call.status, call.raw_output.clone())
      };
      if matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed) {
        if let Some(events) = events {
          events
            .worker(WorkerEvent::ToolEnd {
              role,
              worker_id: identity.worker_id.clone(),
              lease_id: identity.lease_id.clone(),
              work_unit_id: identity.work_unit_id.clone(),
              at: chrono::Utc::now().to_rfc3339(),
              tool_name,
              is_error: matches!(status, ToolCallStatus::Failed),
              output: output.map(|value| value.to_string()),
            })
            .await;
        }
      }
    }
    _ => {}
  }
}

async fn apply_preferences<Link>(
  session: &mut agent_client_protocol::ActiveSession<'_, Link>,
  options: Vec<SessionConfigOption>,
  preferences: &loops_domain::config::RolePreference,
) -> std::result::Result<(), agent_client_protocol::Error>
where
  Link: agent_client_protocol::role::HasPeer<Agent>,
{
  for (category, configured) in [
    (
      SessionConfigOptionCategory::Model,
      preferences.model.as_deref(),
    ),
    (
      SessionConfigOptionCategory::ThoughtLevel,
      preferences.thought_level.as_deref(),
    ),
    (
      SessionConfigOptionCategory::Mode,
      preferences.mode.as_deref(),
    ),
  ] {
    let Some(value) = configured else { continue };

    let selected = resolve_preference_option(&options, &category, value)
      .map_err(|message| agent_client_protocol::Error::internal_error().data(message))?;
    let Some((option, selected)) = selected else {
      eprintln!(
        "requested {category:?} preference {value:?} is not exposed by this ACP session; continuing with agent default"
      );
      continue;
    };

    session
      .connection()
      .send_request_to(
        Agent,
        SetSessionConfigOptionRequest::new(
          session.session_id().clone(),
          option.id.clone(),
          selected.value.clone(),
        ),
      )
      .block_task()
      .await?;
  }
  Ok(())
}

fn resolve_preference_option<'a>(
  options: &'a [SessionConfigOption],
  category: &SessionConfigOptionCategory,
  value: &str,
) -> std::result::Result<Option<(&'a SessionConfigOption, &'a SessionConfigSelectOption)>, String> {
  let option = options
    .iter()
    .find(|option| option.category.as_ref() == Some(category));
  let selected = option.and_then(|option| match &option.kind {
    SessionConfigKind::Select(select) => match &select.options {
      SessionConfigSelectOptions::Ungrouped(values) => values
        .iter()
        .find(|candidate| candidate.value.to_string() == value),
      SessionConfigSelectOptions::Grouped(groups) => groups
        .iter()
        .flat_map(|group| group.options.iter())
        .find(|candidate| candidate.value.to_string() == value),
      _ => None,
    },
    _ => None,
  });

  match option.zip(selected) {
    Some(selection) => Ok(Some(selection)),
    None if matches!(category, SessionConfigOptionCategory::Model) => Err(format!(
      "requested {category:?} preference {value:?} is not exposed by this ACP session"
    )),
    None => Ok(None),
  }
}

async fn read_config_options<Link>(
  session: &mut agent_client_protocol::ActiveSession<'_, Link>,
  role: WorkerRole,
  identity: &WorkerIdentity,
  events: Option<EventSink>,
  text: &mut String,
  tool_calls: &mut HashMap<ToolCallId, ToolCall>,
) -> std::result::Result<Option<Vec<SessionConfigOption>>, agent_client_protocol::Error>
where
  Link: agent_client_protocol::role::HasPeer<Agent>,
{
  let deadline = Instant::now() + Duration::from_millis(100);
  loop {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
      return Ok(None);
    }

    let message = match tokio::time::timeout(remaining, session.read_update()).await {
      Ok(message) => message?,
      Err(_) => return Ok(None),
    };

    let SessionMessage::SessionMessage(dispatch) = message else {
      if matches!(message, SessionMessage::StopReason(_)) {
        return Ok(None);
      }
      continue;
    };

    let mut options = None;
    let sink = events.clone();
    MatchDispatch::new(dispatch)
      .if_notification(async |notification: SessionNotification| {
        match notification.update {
          SessionUpdate::ConfigOptionUpdate(update) => options = Some(update.config_options),
          update => map_update(update, role, identity, sink, text, tool_calls).await,
        }
        Ok(())
      })
      .await
      .otherwise_ignore()?;
    if options.is_some() {
      return Ok(options);
    }
  }
}

fn build_worker_prompt(role: WorkerRole, schema: &Value, work_context: &str) -> Result<String> {
  Ok(format!(
    "{}\n\nROLE OUTPUT SCHEMA (return exactly one JSON value and no surrounding prose):\n{}\n\nWORK CONTEXT:\n{}",
    full_role_prompt(role),
    serde_json::to_string_pretty(schema)?,
    work_context
  ))
}

fn string_array_schema() -> Value {
  json!({"type":"array","items":{"type":"string"}})
}

fn requirement_schema() -> Value {
  json!({"type":"object","additionalProperties":false,"required":["id","title","description","acceptanceCriteria"],"properties":{"id":{"type":"string"},"title":{"type":"string"},"description":{"type":"string"},"acceptanceCriteria":string_array_schema()}})
}

fn requirement_assessment_schema() -> Value {
  json!({"type":"object","additionalProperties":false,"required":["id","status","evidence","gaps"],"properties":{"id":{"type":"string"},"status":{"type":"string","enum":["satisfied","partial","missing"]},"evidence":string_array_schema(),"gaps":string_array_schema()}})
}

fn work_scope_schema() -> Value {
  json!({"type":"object","additionalProperties":false,"required":["paths"],"properties":{"paths":string_array_schema()}})
}

fn work_unit_schema() -> Value {
  json!({"type":"object","additionalProperties":false,"required":["id","title","objective","requirementIds","acceptanceCriteria","suggestedChecks","dependsOn","scope"],"properties":{"id":{"type":"string"},"title":{"type":"string"},"objective":{"type":"string"},"requirementIds":string_array_schema(),"acceptanceCriteria":string_array_schema(),"suggestedChecks":{"type":"array","description":"Executable, non-interactive shell commands only; no instructions, prose, or Markdown backticks. Each command must perform its own assertion.","items":{"type":"string","description":"A single executable shell command."}},"dependsOn":string_array_schema(),"scope":work_scope_schema()}})
}

fn architect_schema() -> Value {
  json!({"type":"object","additionalProperties":false,"required":["requirements"],"properties":{"requirements":{"type":"array","items":requirement_schema()}}})
}

fn reconcile_schema() -> Value {
  json!({"type":"object","additionalProperties":false,"required":["complete","summary","requirements","workUnits"],"properties":{"complete":{"type":"boolean"},"summary":{"type":"string"},"requirements":{"type":"array","items":requirement_assessment_schema()},"workUnits":{"type":"array","items":work_unit_schema()}}})
}

fn discovery_schema() -> Value {
  json!({"oneOf":[{"type":"object","additionalProperties":false,"required":["type","workUnitId","dependsOn","reason"],"properties":{"type":{"const":"dependency"},"workUnitId":{"type":"string"},"dependsOn":{"type":"string"},"reason":{"type":"string"}}},{"type":"object","additionalProperties":false,"required":["type","description"],"properties":{"type":{"const":"blocker"},"description":{"type":"string"}}},{"type":"object","additionalProperties":false,"required":["type","paths","reason"],"properties":{"type":{"const":"scope_expansion"},"paths":string_array_schema(),"reason":{"type":"string"}}}]})
}

fn worker_summary_schema() -> Value {
  json!({"type":"object","additionalProperties":false,"required":["summary","changedFiles","testsRun","notes"],"properties":{"summary":{"type":"string"},"changedFiles":string_array_schema(),"testsRun":string_array_schema(),"notes":string_array_schema(),"decisions":string_array_schema(),"discoveries":{"type":"array","items":discovery_schema()},"risks":string_array_schema(),"followUps":string_array_schema()}})
}

#[cfg(test)]
mod tests {
  use super::*;

  fn valid_reconcile_output() -> Value {
    json!({
      "complete": false,
      "summary": "One requirement remains",
      "requirements": [{
        "id": "REQ-001",
        "status": "missing",
        "evidence": [],
        "gaps": ["Current output is static"]
      }],
      "workUnits": [{
        "id": "WU-001",
        "title": "Print current datetime",
        "objective": "Replace the static greeting",
        "requirementIds": ["REQ-001"],
        "acceptanceCriteria": ["Output contains the current datetime"],
        "suggestedChecks": ["cargo run --quiet"],
        "dependsOn": [],
        "scope": {"paths": ["src/**"]}
      }]
    })
  }

  #[test]
  fn reconcile_schema_accepts_the_typed_shape() {
    let output = valid_reconcile_output();

    validate_json_schema(&output, &reconcile_schema()).expect("typed reconcile shape is valid");
    serde_json::from_value::<ReconcileResult>(output).expect("schema matches the domain type");
  }

  #[test]
  fn reconcile_schema_rejects_unknown_nested_work_unit_fields() {
    let mut output = valid_reconcile_output();
    output["workUnits"][0]["description"] = Value::String("not part of WorkUnit".into());
    let error = validate_json_schema(&output, &reconcile_schema())
      .expect_err("unknown nested fields must fail schema validation");

    assert!(error
      .to_string()
      .contains(r#"unexpected property "description""#));
  }

  #[test]
  fn configured_model_is_rejected_when_not_discovered() {
    let options = vec![SessionConfigOption::select(
      "model",
      "Model",
      "available-model",
      vec![SessionConfigSelectOption::new(
        "available-model",
        "Available model",
      )],
    )
    .category(SessionConfigOptionCategory::Model)];

    let error = resolve_preference_option(
      &options,
      &SessionConfigOptionCategory::Model,
      "missing-model",
    )
    .unwrap_err();

    assert!(error.contains(r#"Model preference "missing-model" is not exposed"#));
  }
}
