use std::{
  fs,
  io::{BufRead, BufReader, Write},
  path::Path,
  process::{Command, Stdio},
};

use serde::Deserialize;
use serde_json::{json, Value};

struct Repository {
  directory: tempfile::TempDir,
}

impl Repository {
  fn new() -> Self {
    let directory = tempfile::tempdir().expect("temporary repository");
    git(directory.path(), &["init", "-q"]);
    git(directory.path(), &["config", "user.name", "Tenet MCP Test"]);
    git(
      directory.path(),
      &["config", "user.email", "tenet-mcp-test@localhost"],
    );
    fs::write(directory.path().join("SPEC.md"), "# Required behavior\n")
      .expect("write specification");
    Self { directory }
  }

  fn path(&self) -> &Path {
    self.directory.path()
  }

  fn tenet(&self, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tenet"))
      .arg("--cwd")
      .arg(self.path())
      .args(arguments)
      .output()
      .expect("run Tenet CLI")
  }

  fn mcp_request(&self, method: &str, params: Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tenet"))
      .arg("--cwd")
      .arg(self.path())
      .arg("mcp")
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()
      .expect("start Tenet MCP server");
    let mut stdin = child.stdin.take().expect("MCP stdin");
    let stdout = child.stdout.take().expect("MCP stdout");
    let mut stdout = BufReader::new(stdout);

    write_message(
      &mut stdin,
      &json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
          "protocolVersion": "2025-11-25",
          "capabilities": {},
          "clientInfo": { "name": "tenet-integration-test", "version": "1" }
        }
      }),
    );
    let initialized = read_message(&mut stdout);
    assert_eq!(initialized["result"]["serverInfo"]["name"], "tenet");
    write_message(
      &mut stdin,
      &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    );
    write_message(
      &mut stdin,
      &json!({ "jsonrpc": "2.0", "id": 2, "method": method, "params": params }),
    );
    let response = read_message(&mut stdout);
    drop(stdin);
    let status = child.wait().expect("wait for MCP server");
    assert!(status.success(), "MCP server exited unsuccessfully");
    response
  }

  fn mcp_tool(&self, name: &str, arguments: Value) -> Value {
    self.mcp_request(
      "tools/call",
      json!({ "name": name, "arguments": arguments }),
    )
  }

  fn initialize(&self) -> Value {
    success_json(self.tenet(&["init", "--spec", "SPEC.md", "--json"]))
  }

  fn configure(&self, verifier_exit: i32) -> Value {
    fs::write(
      self.path().join(".tenet/tenet.toml"),
      format!(
        "version = 1\nspec_path = \"SPEC.md\"\n\n[[verifiers]]\nid = \"quality\"\nargv = [\"sh\", \"-c\", \"exit {verifier_exit}\"]\ncwd = \".\"\ntimeout_seconds = 10\nmax_output_bytes = 4096\nauthority = \"project\"\n"
      ),
    )
    .expect("write policy");
    structured(&self.mcp_tool("tenet_status", json!({}))).clone()
  }

  fn proposal(&self, status: &Value) -> Value {
    json!({
      "schemaVersion": 2,
      "specDigest": status["specDigest"],
      "policyDigest": status["policyDigest"],
      "requirements": [{
        "id": "REQ-001",
        "statement": "The required behavior is implemented",
        "obligations": [{
          "id": "REQ-001/VO-001",
          "statement": "The configured verifier succeeds",
          "evidenceContract": {
            "claim": { "verifierId": "quality", "authority": "project" }
          }
        }]
      }]
    })
  }

  fn propose(&self, proposal: Value) -> Value {
    structured(&self.mcp_tool("tenet_contract_propose", proposal)).clone()
  }

  fn approve(&self, proposed: &Value) -> Value {
    structured(&self.mcp_tool(
      "tenet_contract_approve",
      json!({
        "proposalId": proposed["proposalId"],
        "proposalDigest": proposed["proposalDigest"]
      }),
    ))
    .clone()
  }

  fn commit(&self, message: &str) -> String {
    git(self.path(), &["add", "."]);
    git(self.path(), &["commit", "-q", "-m", message]);
    git_output(self.path(), &["rev-parse", "HEAD"])
      .trim()
      .to_owned()
  }

  fn admitted(&self, verifier_exit: i32) -> String {
    self.initialize();
    let status = self.configure(verifier_exit);
    let proposed = self.propose(self.proposal(&status));
    self.approve(&proposed);
    self.commit("admit Tenet contract")
  }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusView {
  initialized: bool,
  contract_state: String,
  unresolved_obligations: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GateView {
  authority_revision: String,
  revision: String,
  verdict: String,
  blockers: Vec<Value>,
}

fn write_message(writer: &mut impl Write, message: &Value) {
  serde_json::to_writer(&mut *writer, message).expect("encode MCP message");
  writer.write_all(b"\n").expect("write MCP newline");
  writer.flush().expect("flush MCP message");
}

fn read_message(reader: &mut impl BufRead) -> Value {
  let mut line = String::new();
  reader.read_line(&mut line).expect("read MCP response");
  assert!(!line.is_empty(), "MCP server closed without a response");
  serde_json::from_str(&line).expect("decode MCP response")
}

fn structured(response: &Value) -> &Value {
  assert_eq!(response["result"]["isError"], false, "{response:#}");
  &response["result"]["structuredContent"]
}

fn success_json(output: std::process::Output) -> Value {
  assert!(
    output.status.success(),
    "command failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  serde_json::from_slice(&output.stdout).expect("decode CLI JSON")
}

fn git(cwd: &Path, arguments: &[&str]) {
  let output = Command::new("git")
    .args(arguments)
    .current_dir(cwd)
    .output()
    .expect("run Git");
  assert!(
    output.status.success(),
    "git {} failed: {}",
    arguments.join(" "),
    String::from_utf8_lossy(&output.stderr)
  );
}

fn git_output(cwd: &Path, arguments: &[&str]) -> String {
  let output = Command::new("git")
    .args(arguments)
    .current_dir(cwd)
    .output()
    .expect("run Git");
  assert!(output.status.success());
  String::from_utf8(output.stdout).expect("UTF-8 Git output")
}

#[test]
fn stdio_server_discovers_all_tenet_tools_with_required_fields() {
  let repository = Repository::new();
  let response = repository.mcp_request("tools/list", json!({}));
  let tools = response["result"]["tools"]
    .as_array()
    .expect("discovered tools");
  let names = tools
    .iter()
    .map(|tool| tool["name"].as_str().expect("tool name"))
    .collect::<std::collections::BTreeSet<_>>();
  assert_eq!(
    names,
    std::collections::BTreeSet::from([
      "tenet_status",
      "tenet_contract_schema",
      "tenet_policy_schema",
      "tenet_contract_propose",
      "tenet_contract_approve",
      "tenet_gate",
      "tenet_evidence",
    ])
  );

  let gate = tools
    .iter()
    .find(|tool| tool["name"] == "tenet_gate")
    .expect("gate tool");
  assert_eq!(
    gate["inputSchema"]["required"],
    json!(["authorityRevision", "revision"])
  );
  let evidence = tools
    .iter()
    .find(|tool| tool["name"] == "tenet_evidence")
    .expect("evidence tool");
  assert_eq!(evidence["inputSchema"]["required"], json!(["revision"]));
  let approve = tools
    .iter()
    .find(|tool| tool["name"] == "tenet_contract_approve")
    .expect("approval tool");
  assert!(approve["description"]
    .as_str()
    .expect("approval description")
    .contains("MUST NOT be called unless the human explicitly approved"));
}

#[test]
fn schema_tools_return_canonical_rust_derived_shapes() {
  let repository = Repository::new();
  let contract = repository.mcp_tool("tenet_contract_schema", json!({}));
  assert_eq!(
    structured(&contract)["properties"]["requirements"]["type"],
    "array"
  );
  let policy = repository.mcp_tool("tenet_policy_schema", json!({}));
  assert!(structured(&policy)["$defs"]["VerifierSpec"]["properties"]
    .as_object()
    .expect("verifier properties")
    .contains_key("environment_mode"));
}

#[test]
fn status_is_structured_before_and_after_cli_initialization() {
  let repository = Repository::new();
  let before = repository.mcp_tool("tenet_status", json!({}));
  let decoded: StatusView =
    serde_json::from_value(structured(&before).clone()).expect("typed status result");
  assert!(!decoded.initialized);
  assert_eq!(decoded.contract_state, "missing");
  assert!(decoded.unresolved_obligations.is_empty());

  let initialized = repository.initialize();
  assert_eq!(initialized["initialized"], true);
  assert_eq!(initialized["specPath"], "SPEC.md");
  let after = repository.mcp_tool("tenet_status", json!({}));
  assert_eq!(structured(&after)["initialized"], true);
}

#[test]
fn contract_proposal_is_deterministic_through_mcp() {
  let repository = Repository::new();
  repository.initialize();
  let status = repository.configure(0);
  let proposal = repository.proposal(&status);
  let first = repository.mcp_tool("tenet_contract_propose", proposal.clone());
  let second = repository.mcp_tool("tenet_contract_propose", proposal);
  assert_eq!(structured(&first), structured(&second));
  assert_eq!(structured(&first)["approvalRequired"], true);
}

#[test]
fn post_initialization_workflow_runs_through_mcp() {
  let repository = Repository::new();
  repository.initialize();
  fs::write(
    repository.path().join(".tenet/tenet.toml"),
    "version = 1\nspec_path = \"SPEC.md\"\n\n[[verifiers]]\nid = \"quality\"\nargv = [\"sh\", \"-c\", \"exit 0\"]\ncwd = \".\"\ntimeout_seconds = 10\nmax_output_bytes = 4096\nauthority = \"project\"\n",
  )
  .expect("write verification policy");
  let status = repository.mcp_tool("tenet_status", json!({}));
  let proposal = repository.proposal(structured(&status));
  let proposed = repository.mcp_tool("tenet_contract_propose", proposal);
  let proposal_id = structured(&proposed)["proposalId"].clone();
  let proposal_digest = structured(&proposed)["proposalDigest"].clone();
  let approved = repository.mcp_tool(
    "tenet_contract_approve",
    json!({ "proposalId": proposal_id, "proposalDigest": proposal_digest }),
  );
  assert_eq!(structured(&approved)["proposalId"], proposal_id);
  let authority = repository.commit("admit contract through MCP");
  fs::write(repository.path().join("implementation.txt"), "complete\n")
    .expect("write implementation");
  let revision = repository.commit("implement contract");
  let gate = repository.mcp_tool(
    "tenet_gate",
    json!({ "authorityRevision": authority, "revision": revision }),
  );
  assert_eq!(structured(&gate)["verdict"], "done");
  let evidence = repository.mcp_tool("tenet_evidence", json!({ "revision": revision }));
  assert!(!structured(&evidence)["artifacts"]
    .as_array()
    .expect("evidence artifacts")
    .is_empty());
}

#[test]
fn gate_and_evidence_preserve_typed_domain_results() {
  let repository = Repository::new();
  let authority = repository.admitted(0);
  fs::write(repository.path().join("implementation.txt"), "complete\n")
    .expect("write candidate implementation");
  let revision = repository.commit("implement required behavior");

  let gate = repository.mcp_tool(
    "tenet_gate",
    json!({ "authorityRevision": authority, "revision": revision }),
  );
  let decoded: GateView =
    serde_json::from_value(structured(&gate).clone()).expect("typed gate result");
  assert_eq!(decoded.verdict, "done");
  assert_eq!(decoded.authority_revision, authority);
  assert_eq!(decoded.revision, revision);
  assert!(decoded.blockers.is_empty());

  let evidence = repository.mcp_tool("tenet_evidence", json!({ "revision": revision }));
  assert!(!structured(&evidence)["artifacts"]
    .as_array()
    .expect("evidence artifacts")
    .is_empty());
}

#[test]
fn gate_returns_each_non_done_domain_outcome_as_a_structured_result() {
  for (exit_code, expected) in [(1, "not_done"), (126, "inconclusive")] {
    let repository = Repository::new();
    let revision = repository.admitted(exit_code);
    let response = repository.mcp_tool(
      "tenet_gate",
      json!({ "authorityRevision": revision, "revision": revision }),
    );
    assert_eq!(structured(&response)["verdict"], expected);
  }

  let repository = Repository::new();
  repository.initialize();
  fs::write(
    repository.path().join(".tenet/tenet.toml"),
    "version = 1\nspec_path = \"SPEC.md\"\n\n[[verifiers]]\nid = \"quality\"\nargv = [\"tenet-missing-verifier-command\"]\ncwd = \".\"\ntimeout_seconds = 10\nmax_output_bytes = 4096\nauthority = \"project\"\n",
  )
  .expect("write unavailable verifier policy");
  let status = structured(&repository.mcp_tool("tenet_status", json!({}))).clone();
  let proposed = repository.propose(repository.proposal(&status));
  repository.approve(&proposed);
  let revision = repository.commit("admit unavailable verifier");
  let response = repository.mcp_tool(
    "tenet_gate",
    json!({ "authorityRevision": revision, "revision": revision }),
  );
  assert_eq!(structured(&response)["verdict"], "infrastructure_error");
}

#[test]
fn gate_rejects_missing_authority_or_candidate_arguments_at_protocol_boundary() {
  let repository = Repository::new();
  let missing_authority = repository.mcp_tool("tenet_gate", json!({ "revision": "abc" }));
  assert!(
    missing_authority.get("error").is_some() || missing_authority["result"]["isError"] == true
  );
  let missing_candidate = repository.mcp_tool("tenet_gate", json!({ "authorityRevision": "abc" }));
  assert!(
    missing_candidate.get("error").is_some() || missing_candidate["result"]["isError"] == true
  );
}

#[test]
fn evidence_rejects_symbolic_candidate_revisions() {
  let repository = Repository::new();
  repository.admitted(0);
  let response = repository.mcp_tool("tenet_evidence", json!({ "revision": "HEAD" }));
  assert_eq!(response["result"]["isError"], true);
}

#[test]
fn mcp_gate_preserves_authority_surface_protection() {
  let repository = Repository::new();
  let authority = repository.admitted(0);
  fs::write(
    repository.path().join("SPEC.md"),
    "# Weakened candidate specification\n",
  )
  .expect("change authority surface");
  let revision = repository.commit("change candidate specification");
  let response = repository.mcp_tool(
    "tenet_gate",
    json!({ "authorityRevision": authority, "revision": revision }),
  );
  assert_eq!(structured(&response)["verdict"], "not_done");
  assert_eq!(
    structured(&response)["blockers"][0]["code"],
    "authority_surface_changed"
  );
}

#[test]
fn mcp_approval_revalidates_exact_identity_digest_and_staleness() {
  let repository = Repository::new();
  repository.initialize();
  let status = repository.configure(0);
  let proposed = repository.mcp_tool("tenet_contract_propose", repository.proposal(&status));
  let proposed = structured(&proposed);

  let wrong_identity = repository.mcp_tool(
    "tenet_contract_approve",
    json!({
      "proposalId": "proposal-wrong",
      "proposalDigest": proposed["proposalDigest"]
    }),
  );
  assert_eq!(wrong_identity["result"]["isError"], true);

  fs::write(
    repository.path().join("SPEC.md"),
    "# Changed after proposal\n",
  )
  .expect("change specification");
  let stale = repository.mcp_tool(
    "tenet_contract_approve",
    json!({
      "proposalId": proposed["proposalId"],
      "proposalDigest": proposed["proposalDigest"]
    }),
  );
  assert_eq!(stale["result"]["isError"], true);
}

#[test]
fn mcp_contract_proposal_cannot_bypass_domain_validation() {
  let repository = Repository::new();
  repository.initialize();
  let status = repository.configure(0);
  let mut proposal = repository.proposal(&status);
  proposal["requirements"][0]["obligations"][0]["evidenceContract"]["claim"]["verifierId"] =
    json!("unknown");
  let response = repository.mcp_tool("tenet_contract_propose", proposal);
  assert_eq!(response["result"]["isError"], true);
}
