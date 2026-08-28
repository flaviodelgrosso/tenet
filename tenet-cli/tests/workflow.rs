use std::{
  fs,
  io::{BufRead, BufReader, Write},
  path::Path,
  process::{Command, Stdio},
  time::{Duration, Instant},
};

use serde_json::{Value, json};

struct Repository {
  directory: tempfile::TempDir,
}

struct OperationOutput {
  code: i32,
  value: Value,
  status: TestStatus,
  stdout: Vec<u8>,
}

struct TestStatus(i32);

impl TestStatus {
  fn success(&self) -> bool {
    self.0 == 0
  }

  fn code(&self) -> Option<i32> {
    Some(self.0)
  }
}

impl Repository {
  fn new() -> Self {
    let directory = tempfile::tempdir().expect("temporary repository");
    git(directory.path(), &["init", "-q"]);
    git(directory.path(), &["config", "user.name", "Tenet Test"]);
    git(
      directory.path(),
      &["config", "user.email", "tenet-test@localhost"],
    );
    fs::write(directory.path().join("SPEC.md"), "# Required behavior\n")
      .expect("write specification");
    fs::write(directory.path().join("AGENTS.md"), "existing agent rules\n")
      .expect("write AGENTS sentinel");
    fs::write(
      directory.path().join("CLAUDE.md"),
      "existing Claude rules\n",
    )
    .expect("write CLAUDE sentinel");
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
      .expect("run tenet")
  }

  fn mcp_request(&self, method: &str, params: Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tenet"))
      .arg("--cwd")
      .arg(self.path())
      .arg("mcp")
      .env_remove("OPENAI_API_KEY")
      .env_remove("ANTHROPIC_API_KEY")
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
          "clientInfo": { "name": "tenet-workflow-test", "version": "1" }
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
    assert!(child.wait().expect("wait for MCP server").success());
    response
  }

  fn mcp_tool(&self, name: &str, arguments: Value) -> OperationOutput {
    let response = self.mcp_request(
      "tools/call",
      json!({ "name": name, "arguments": arguments }),
    );
    if response["result"]["isError"] == true {
      let value = json!({
        "message": response["result"]["content"][0]["text"]
      });
      return operation_output(1, value);
    }
    let value = response["result"]["structuredContent"].clone();
    let code = match value["verdict"].as_str() {
      Some("not_done") => 2,
      Some("inconclusive") => 3,
      Some("infrastructure_error") => 4,
      _ => 0,
    };
    operation_output(code, value)
  }

  fn status(&self) -> OperationOutput {
    self.mcp_tool("tenet_status", json!({}))
  }

  fn policy_schema(&self) -> OperationOutput {
    self.mcp_tool("tenet_policy_schema", json!({}))
  }

  fn contract_schema(&self) -> OperationOutput {
    self.mcp_tool("tenet_contract_schema", json!({}))
  }

  fn propose_file(&self, path: &Path) -> OperationOutput {
    let proposal =
      serde_json::from_slice(&fs::read(path).expect("read proposal")).expect("parse proposal");
    self.mcp_tool("tenet_contract_propose", proposal)
  }

  fn approve(&self, proposal_id: &str, proposal_digest: &str) -> OperationOutput {
    self.mcp_tool(
      "tenet_contract_approve",
      json!({ "proposalId": proposal_id, "proposalDigest": proposal_digest }),
    )
  }

  fn evidence(&self, revision: &str) -> OperationOutput {
    self.mcp_tool("tenet_evidence", json!({ "revision": revision }))
  }

  fn gate(&self, authority_revision: &str, revision: &str) -> OperationOutput {
    self.mcp_tool(
      "tenet_gate",
      json!({ "authorityRevision": authority_revision, "revision": revision }),
    )
  }

  fn init(&self) -> Value {
    success_json(self.tenet(&["init", "--json"]))
  }

  fn configure(&self, command: &[&str]) -> Value {
    self.configure_with_authority(command, "project", None)
  }

  fn configure_with_authority(
    &self,
    command: &[&str],
    authority: &str,
    oracle_path: Option<&str>,
  ) -> Value {
    let argv = command
      .iter()
      .map(|item| format!("\"{}\"", item.replace('"', "\\\"")))
      .collect::<Vec<_>>()
      .join(", ");
    let oracle_path = oracle_path
      .map(|path| format!("oracle_path = \"{path}\"\n"))
      .unwrap_or_default();
    fs::write(
      self.path().join(".tenet/tenet.toml"),
      format!(
        "version = 1\nspec_path = \"SPEC.md\"\n\n[[verifiers]]\nid = \"quality\"\nargv = [{argv}]\ncwd = \".\"\ntimeout_seconds = 10\nmax_output_bytes = 4096\nauthority = \"{authority}\"\n{oracle_path}"
      ),
    )
    .expect("write policy");
    success_json(self.status())
  }

  fn propose_and_approve(&self, status: &Value) -> Value {
    self.propose_and_approve_with_authority(status, "project")
  }

  fn propose_and_approve_with_authority(&self, status: &Value, authority: &str) -> Value {
    let proposal = json!({
      "schemaVersion": 2,
      "specDigest": status["specDigest"],
      "policyDigest": status["policyDigest"],
      "requirements": [{
        "id": "REQ-001",
        "statement": "The required behavior is implemented",
        "obligations": [{
          "id": "REQ-001/VO-001",
          "statement": "The configured verifier succeeds",
          "evidenceContract": { "claim": { "verifierId": "quality", "authority": authority } }
        }, {
          "id": "REQ-001/VO-002",
          "statement": "The configured verifier confirms the candidate",
          "evidenceContract": { "claim": { "verifierId": "quality", "authority": authority } }
        }]
      }]
    });
    let proposal_path = self.path().join("proposal.json");
    fs::write(
      &proposal_path,
      serde_json::to_vec_pretty(&proposal).expect("encode proposal"),
    )
    .expect("write proposal");
    let proposed = success_json(self.propose_file(&proposal_path));
    success_json(
      self.approve(
        proposed["proposalId"].as_str().expect("proposal ID"),
        proposed["proposalDigest"]
          .as_str()
          .expect("proposal digest"),
      ),
    )
  }

  fn commit(&self, message: &str) -> String {
    git(self.path(), &["add", "."]);
    git(self.path(), &["commit", "-q", "-m", message]);
    git_output(self.path(), &["rev-parse", "HEAD"])
      .trim()
      .to_owned()
  }

  fn admitted(&self, verifier: &[&str]) -> String {
    self.init();
    let status = self.configure(verifier);
    self.propose_and_approve(&status);
    self.commit("admit Tenet contract")
  }
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) {
  use std::os::unix::fs::PermissionsExt;

  fs::create_dir_all(path.parent().expect("executable parent")).expect("create oracle bundle");
  fs::write(path, contents).expect("write executable");
  let mut permissions = fs::metadata(path)
    .expect("read executable metadata")
    .permissions();
  permissions.set_mode(0o755);
  fs::set_permissions(path, permissions).expect("make executable");
}

fn git(cwd: &Path, arguments: &[&str]) {
  let output = Command::new("git")
    .args(arguments)
    .current_dir(cwd)
    .output()
    .expect("run git");
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
    .expect("run git");
  assert!(output.status.success());
  String::from_utf8(output.stdout).expect("UTF-8 Git output")
}

fn operation_output(code: i32, value: Value) -> OperationOutput {
  let stdout = serde_json::to_vec(&value).expect("encode operation output");
  OperationOutput {
    code,
    value,
    status: TestStatus(code),
    stdout,
  }
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

trait TestOutput {
  fn code(&self) -> i32;
  fn value(&self) -> Value;
}

impl TestOutput for std::process::Output {
  fn code(&self) -> i32 {
    self.status.code().unwrap_or(-1)
  }

  fn value(&self) -> Value {
    serde_json::from_slice(&self.stdout).expect("JSON response")
  }
}

impl TestOutput for OperationOutput {
  fn code(&self) -> i32 {
    self.code
  }

  fn value(&self) -> Value {
    self.value.clone()
  }
}

fn success_json(output: impl TestOutput) -> Value {
  assert_eq!(output.code(), 0, "operation failed");
  output.value()
}

fn failure_json(output: impl TestOutput, expected_code: i32) -> Value {
  assert_eq!(output.code(), expected_code);
  output.value()
}

#[test]
fn init_creates_valid_portable_mcp_configuration() {
  let repository = Repository::new();

  repository.init();

  let configuration: Value = serde_json::from_slice(
    &fs::read(repository.path().join(".mcp.json")).expect("read MCP configuration"),
  )
  .expect("parse MCP configuration");
  assert_eq!(
    configuration,
    json!({
      "mcpServers": {
        "tenet": {
          "command": "tenet",
          "args": ["mcp"]
        }
      }
    })
  );
  for vendor_configuration in [".omp/mcp.json", ".cursor/mcp.json"] {
    assert!(
      !repository.path().join(vendor_configuration).exists(),
      "created vendor-specific configuration {vendor_configuration}"
    );
  }
}

#[test]
fn init_merges_tenet_mcp_server_without_discarding_existing_configuration() {
  let repository = Repository::new();
  fs::write(
    repository.path().join(".mcp.json"),
    r#"{
  "metadata": { "preserve": true },
  "mcpServers": {
    "other": { "command": "other", "args": ["serve"], "env": { "MODE": "test" } }
  }
}"#,
  )
  .expect("write MCP configuration");

  repository.init();

  let configuration: Value = serde_json::from_slice(
    &fs::read(repository.path().join(".mcp.json")).expect("read MCP configuration"),
  )
  .expect("parse merged MCP configuration");
  assert_eq!(
    configuration,
    json!({
      "metadata": { "preserve": true },
      "mcpServers": {
        "other": { "command": "other", "args": ["serve"], "env": { "MODE": "test" } },
        "tenet": { "command": "tenet", "args": ["mcp"] }
      }
    })
  );
}

#[test]
fn init_is_idempotent_when_tenet_mcp_server_is_already_registered() {
  let repository = Repository::new();
  let agents_before = fs::read(repository.path().join("AGENTS.md")).expect("read AGENTS");
  let claude_before = fs::read(repository.path().join("CLAUDE.md")).expect("read CLAUDE");

  let first = repository.init();
  let config_before = fs::read(repository.path().join(".tenet/tenet.toml")).expect("read config");
  let mcp_before = fs::read(repository.path().join(".mcp.json")).expect("read MCP configuration");
  let skill_before =
    fs::read(repository.path().join(".agents/skills/tenet/SKILL.md")).expect("read skill");
  let second = repository.init();

  assert_eq!(first["created"], true);
  assert_eq!(first["contractState"], "missing");
  assert_eq!(second["created"], false);
  assert_eq!(
    config_before,
    fs::read(repository.path().join(".tenet/tenet.toml")).unwrap()
  );
  assert_eq!(
    mcp_before,
    fs::read(repository.path().join(".mcp.json")).unwrap()
  );
  assert_eq!(
    skill_before,
    fs::read(repository.path().join(".agents/skills/tenet/SKILL.md")).unwrap()
  );
  assert_eq!(
    agents_before,
    fs::read(repository.path().join("AGENTS.md")).unwrap()
  );
  assert_eq!(
    claude_before,
    fs::read(repository.path().join("CLAUDE.md")).unwrap()
  );
}

#[test]
fn init_rejects_conflicting_tenet_mcp_server_without_overwriting_it() {
  let repository = Repository::new();
  let mcp_path = repository.path().join(".mcp.json");
  fs::write(
    &mcp_path,
    r#"{ "mcpServers": { "tenet": { "command": "custom-tenet", "args": ["mcp"] } } }"#,
  )
  .expect("write conflicting MCP configuration");
  let before = fs::read(&mcp_path).expect("read conflicting MCP configuration");

  let output = repository.tenet(&["init"]);

  assert!(!output.status.success());
  assert!(
    String::from_utf8(output.stderr)
      .expect("UTF-8 error output")
      .contains("conflicting `tenet` MCP server entry")
  );
  assert_eq!(
    before,
    fs::read(mcp_path).expect("read retained MCP configuration")
  );
}

#[test]
fn init_creates_a_missing_default_specification_inside_the_repository() {
  let repository = Repository::new();
  fs::remove_file(repository.path().join("SPEC.md")).expect("remove specification");

  let first = success_json(repository.tenet(&["init", "--spec", "docs/TENET.md", "--json"]));
  assert_eq!(first["created"], true);
  assert_eq!(first["specPath"], "docs/TENET.md");
  assert_eq!(
    fs::read_to_string(repository.path().join("docs/TENET.md")).unwrap(),
    "# Tenet completion specification\n\nDescribe the required behavior and acceptance criteria for this repository.\n"
  );

  let second = success_json(repository.tenet(&["init", "--spec", "docs/TENET.md", "--json"]));
  assert_eq!(second["created"], false);
}

#[test]
fn init_defaults_to_the_repository_root_specification_from_a_subdirectory() {
  let repository = Repository::new();
  let nested = repository.path().join("nested");
  fs::create_dir(&nested).expect("create nested directory");

  let initialized = success_json(
    Command::new(env!("CARGO_BIN_EXE_tenet"))
      .arg("--cwd")
      .arg(&nested)
      .args(["init", "--json"])
      .output()
      .expect("initialize from nested directory"),
  );

  assert_eq!(initialized["specPath"], "SPEC.md");
  assert!(!nested.join("SPEC.md").exists());
}

#[test]
fn public_cli_exposes_only_repository_initialization() {
  let repository = Repository::new();
  let initialized = repository.tenet(&["init"]);
  assert!(initialized.status.success());

  let help = repository.tenet(&["--help"]);
  assert!(help.status.success());
  let help = String::from_utf8(help.stdout).expect("UTF-8 help");
  assert!(help.contains("init"));
  for hidden in ["status", "contract", "policy", "gate", "evidence", "mcp"] {
    assert!(
      !help.contains(&format!("\n  {hidden}")),
      "public help exposes {hidden}"
    );
  }

  for removed in ["status", "contract", "policy", "gate", "evidence"] {
    let output = repository.tenet(&[removed]);
    assert!(
      !output.status.success(),
      "removed command {removed} succeeded"
    );
  }
}

#[test]
fn generated_skill_is_portable_concise_and_not_authoritative() {
  let repository = Repository::new();
  repository.init();
  let skill = fs::read_to_string(repository.path().join(".agents/skills/tenet/SKILL.md"))
    .expect("read skill");

  assert!(skill.starts_with("---\nname: tenet\n"));
  assert!(skill.contains("tenet-skill-version: \"6\""));
  assert!(skill.contains("`tenet init` is the only user-facing CLI workflow"));
  assert!(skill.contains("interact with Tenet only through its semantic MCP tools"));
  assert!(skill.contains("present the exact proposal ID and digest"));
  assert!(skill.contains("every requirement and obligation ID and statement"));
  assert!(skill.contains("every primary verifier and authority mapping"));
  assert!(skill.contains("every oracle-assurance ID, criterion, verifier, and authority mapping"));
  assert!(skill.contains("Never self-approve or infer approval from silence"));
  assert!(skill.contains("Only after explicit approval, call `tenet_contract_approve`"));
  assert!(skill.contains("content, specification, policy, ID, or digest changed"));
  assert!(skill.contains("Never choose or advance A yourself"));
  assert!(skill.contains("Call `tenet_gate` with both exact full revisions"));
  assert!(skill.contains("Claim completion only when `tenet_gate` returns `done`"));
  assert!(skill.contains("exact reported (A, R) pair"));
  assert!(skill.contains("Inspect `tenet_policy_schema`"));
  assert!(skill.contains("never infer hidden capabilities or reverse-engineer the binary"));
  assert!(skill.lines().count() < 35);
  assert_eq!(skill, include_str!("../../.agents/skills/tenet/SKILL.md"));
  for forbidden in [
    "Codex",
    "Claude",
    "Gemini",
    "ACP",
    "contract.json",
    "verifier_id",
  ] {
    assert!(!skill.contains(forbidden), "skill contains {forbidden}");
  }
}

#[test]
fn policy_schema_exposes_every_supported_verifier_field() {
  let repository = Repository::new();
  let schema = success_json(repository.policy_schema());
  let verifier = &schema["$defs"]["VerifierSpec"];
  let properties = verifier["properties"]
    .as_object()
    .expect("verifier properties");

  for field in [
    "id",
    "argv",
    "cwd",
    "timeout_seconds",
    "max_output_bytes",
    "env",
    "environment_mode",
    "authority",
    "oracle_path",
  ] {
    assert!(
      properties.contains_key(field),
      "missing verifier field {field}"
    );
  }
  assert_eq!(
    schema["$defs"]["VerifierAuthority"]["enum"],
    json!(["project", "authority_snapshot"])
  );
  assert_eq!(
    schema["$defs"]["EnvironmentMode"]["enum"],
    json!(["ambient", "declared"])
  );
}

#[test]
fn proposal_schema_validation_and_operator_admission_are_separate() {
  let repository = Repository::new();
  repository.init();
  let status = repository.configure(&["/usr/bin/true"]);
  let schema = success_json(repository.contract_schema());
  assert_eq!(schema["properties"]["requirements"]["type"], "array");

  let approval = repository.propose_and_approve(&status);
  assert_eq!(approval["contractPath"], ".tenet/contract.json");
  assert!(repository.path().join(".tenet/contract.json").exists());
}

#[test]
fn proposal_cannot_introduce_an_unknown_verifier() {
  let repository = Repository::new();
  repository.init();
  let status = repository.configure(&["/usr/bin/true"]);
  let proposal = json!({
    "schemaVersion": 2,
    "specDigest": status["specDigest"],
    "policyDigest": status["policyDigest"],
    "requirements": [{
      "id": "REQ-001",
      "statement": "claim",
      "obligations": [{
        "id": "REQ-001/VO-001",
        "statement": "oracle",
        "evidenceContract": { "claim": { "verifierId": "agent-selected-shell", "authority": "project" } }
      }]
    }]
  });
  fs::write(
    repository.path().join("bad.json"),
    serde_json::to_vec(&proposal).unwrap(),
  )
  .unwrap();
  let output = repository.propose_file(Path::new(
    repository.path().join("bad.json").to_str().unwrap(),
  ));
  assert!(!output.status.success());
  let error: Value = serde_json::from_slice(&output.stdout).expect("typed JSON error");
  assert!(
    error["message"]
      .as_str()
      .unwrap()
      .contains("unknown verifier")
  );
  assert!(!repository.path().join(".tenet/contract.json").exists());
}

#[test]
fn proposal_rejects_verifier_authority_mismatch() {
  let repository = Repository::new();
  repository.init();
  let status = repository.configure(&["/usr/bin/true"]);
  let proposal = json!({
    "schemaVersion": 2,
    "specDigest": status["specDigest"],
    "policyDigest": status["policyDigest"],
    "requirements": [{
      "id": "REQ-001",
      "statement": "claim",
      "obligations": [{
        "id": "REQ-001/VO-001",
        "statement": "oracle",
        "evidenceContract": { "claim": { "verifierId": "quality", "authority": "authority_snapshot" } }
      }]
    }]
  });
  let proposal_path = repository.path().join("authority-mismatch.json");
  fs::write(&proposal_path, serde_json::to_vec(&proposal).unwrap()).unwrap();

  let output = repository.propose_file(Path::new(proposal_path.to_str().unwrap()));
  assert!(!output.status.success());
  let error: Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(
    error["message"]
      .as_str()
      .unwrap()
      .contains("requires verifier authority")
  );
}

#[test]
fn exact_revision_gate_returns_done_and_explains_persisted_evidence() {
  let repository = Repository::new();
  let revision = repository.admitted(&["/usr/bin/true"]);

  let gate = success_json(repository.gate(&revision, &revision));
  assert_eq!(gate["verdict"], "done");
  assert_eq!(gate["revision"], revision);
  assert_eq!(gate["authorityRevision"], revision);
  assert_eq!(gate["obligations"][0]["state"], "contract_satisfied");
  let evidence = success_json(repository.evidence(&revision));
  assert_eq!(evidence["artifacts"][0]["revision"], revision);
  assert_eq!(evidence["artifacts"][0]["authorityRevision"], revision);
  assert_eq!(
    evidence["artifacts"][0]["authority"],
    "tenet_observed_project_verifier"
  );
  assert_eq!(
    evidence["artifacts"][0]["provenance"],
    "tenet_local_verifier"
  );
  assert_eq!(evidence["artifacts"][0]["evidenceKind"], "claim");
  assert_eq!(
    evidence["artifacts"][0]["execution"]["environmentMode"],
    "ambient"
  );
  assert_eq!(
    evidence["artifacts"][0]["execution"]["runnerIdentity"],
    "tenet.local_process_runner.v1"
  );
  for field in [
    "tenetVersion",
    "os",
    "architecture",
    "executionEnvironmentIdentity",
  ] {
    assert!(
      !evidence["artifacts"][0]["execution"][field]
        .as_str()
        .unwrap()
        .is_empty()
    );
  }
  let status = success_json(repository.status());
  assert_eq!(status["lastGatedAuthorityRevision"], revision);
}

#[test]
fn failed_verifier_returns_not_done_with_contradiction() {
  let repository = Repository::new();
  let revision = repository.admitted(&["/usr/bin/false"]);

  let gate = failure_json(repository.gate(&revision, &revision), 2);
  assert_eq!(gate["verdict"], "not_done");
  assert_eq!(gate["obligations"][0]["state"], "contradicted");
  assert!(
    gate["blockers"]
      .as_array()
      .unwrap()
      .iter()
      .any(|item| item["code"] == "contradiction_observed")
  );
}

#[test]
fn verifier_with_no_evidence_returns_typed_missing_evidence() {
  let repository = Repository::new();
  let revision = repository.admitted(&["/bin/sh", "-c", "exit 125"]);

  let gate = failure_json(repository.gate(&revision, &revision), 2);
  assert_eq!(gate["verdict"], "not_done");
  assert_eq!(gate["obligations"][0]["state"], "missing_evidence");
  assert_eq!(gate["blockers"][0]["code"], "missing_evidence");
}

#[test]
fn changed_specification_invalidates_the_admitted_contract() {
  let repository = Repository::new();
  let authority_revision = repository.admitted(&["/usr/bin/true"]);
  fs::write(
    repository.path().join("SPEC.md"),
    "# Changed normative behavior\n",
  )
  .unwrap();
  let revision = repository.commit("change specification");

  let gate = failure_json(repository.gate(&authority_revision, &revision), 2);
  assert_eq!(gate["verdict"], "not_done");
  assert_eq!(gate["blockers"][0]["code"], "authority_surface_changed");
}

#[test]
fn fresh_clone_gates_without_historical_local_state_or_credentials() {
  let repository = Repository::new();
  let authority_revision = repository.admitted(&["/usr/bin/true"]);
  fs::write(repository.path().join("implementation.txt"), "candidate\n").unwrap();
  let revision = repository.commit("candidate implementation");
  success_json(repository.gate(&authority_revision, &revision));

  let clone = tempfile::tempdir().unwrap();
  let output = Command::new("git")
    .args(["clone", "-q"])
    .arg(repository.path())
    .arg(clone.path())
    .output()
    .expect("clone repository");
  assert!(output.status.success());
  assert!(!clone.path().join(".tenet/state.json").exists());
  let clone_repository = Repository { directory: clone };
  let gate = success_json(clone_repository.gate(&authority_revision, &revision));
  assert_eq!(gate["verdict"], "done");
}

#[test]
fn mcp_workflow_remains_functional_without_the_generated_skill() {
  let repository = Repository::new();
  let authority_revision = repository.admitted(&["/usr/bin/true"]);
  fs::remove_file(repository.path().join(".agents/skills/tenet/SKILL.md")).unwrap();
  let revision = repository.commit("remove optional skill");

  let gate = success_json(repository.gate(&authority_revision, &revision));
  assert_eq!(gate["verdict"], "done");
}

#[test]
fn changed_policy_invalidates_the_admitted_contract() {
  let repository = Repository::new();
  let authority_revision = repository.admitted(&["/usr/bin/true"]);
  let path = repository.path().join(".tenet/tenet.toml");
  let policy = fs::read_to_string(&path)
    .unwrap()
    .replace("timeout_seconds = 10", "timeout_seconds = 11");
  fs::write(path, policy).unwrap();
  let revision = repository.commit("change verification policy");

  let gate = failure_json(repository.gate(&authority_revision, &revision), 2);
  assert_eq!(gate["verdict"], "not_done");
  assert_eq!(gate["blockers"][0]["code"], "authority_surface_changed");
}

#[test]
fn candidate_cannot_replace_authority_policy_and_contract_with_trivial_verifier() {
  let repository = Repository::new();
  let authority_revision = repository.admitted(&["/usr/bin/false"]);
  let status = repository.configure(&["/usr/bin/true"]);
  repository.propose_and_approve(&status);
  let revision = repository.commit("forge candidate control plane");

  let gate = failure_json(repository.gate(&authority_revision, &revision), 2);
  assert_eq!(gate["blockers"][0]["code"], "authority_surface_changed");
  assert!(
    gate["blockers"][0]["message"]
      .as_str()
      .unwrap()
      .contains(".tenet/tenet.toml")
  );
}

#[test]
fn candidate_cannot_replace_authority_contract_with_one_trivial_obligation() {
  let repository = Repository::new();
  let authority_revision = repository.admitted(&["/usr/bin/true"]);
  let contract_path = repository.path().join(".tenet/contract.json");
  let mut contract: Value =
    serde_json::from_slice(&fs::read(&contract_path).unwrap()).expect("parse contract");
  contract["requirements"][0]["obligations"]
    .as_array_mut()
    .unwrap()
    .truncate(1);
  contract["requirements"][0]["obligations"][0]["statement"] =
    Value::String("trivial candidate obligation".into());
  fs::write(
    &contract_path,
    serde_json::to_vec_pretty(&contract).unwrap(),
  )
  .unwrap();
  let revision = repository.commit("forge candidate contract");

  let gate = failure_json(repository.gate(&authority_revision, &revision), 2);
  assert_eq!(gate["blockers"][0]["code"], "authority_surface_changed");
  assert!(
    gate["blockers"][0]["message"]
      .as_str()
      .unwrap()
      .contains(".tenet/contract.json")
  );
}

#[test]
fn implementation_candidate_runs_authority_verifier_in_candidate_tree() {
  let repository = Repository::new();
  let authority_revision = repository.admitted(&["/bin/sh", "-c", "test -f implemented.txt"]);
  fs::write(repository.path().join("implemented.txt"), "implemented\n").unwrap();
  let revision = repository.commit("implement required behavior");

  let gate = success_json(repository.gate(&authority_revision, &revision));
  assert_eq!(gate["verdict"], "done");
  assert_eq!(gate["authorityRevision"], authority_revision);
  assert_eq!(gate["revision"], revision);
}

#[cfg(unix)]
#[test]
fn authority_snapshot_oracle_runs_from_authority_and_records_git_identity() {
  let repository = Repository::new();
  repository.init();
  write_executable(
    &repository.path().join("oracles/quality/verify"),
    "#!/bin/sh\ntest -f \"$TENET_CANDIDATE_ROOT/implemented.txt\"\n",
  );
  let status =
    repository.configure_with_authority(&["verify"], "authority_snapshot", Some("oracles/quality"));
  repository.propose_and_approve_with_authority(&status, "authority_snapshot");
  let authority_revision = repository.commit("admit authority snapshot oracle");
  fs::write(repository.path().join("implemented.txt"), "implemented\n").unwrap();
  let revision = repository.commit("implement required behavior");

  let gate = success_json(repository.gate(&authority_revision, &revision));
  assert_eq!(gate["verdict"], "done");
  let evidence = success_json(repository.evidence(&revision));
  let artifact = &evidence["artifacts"][0];
  assert_eq!(
    artifact["authority"],
    "tenet_observed_authority_snapshot_verifier"
  );
  assert_eq!(
    artifact["oracleIdentity"]["authority"],
    "authority_snapshot"
  );
  assert_eq!(
    artifact["oracleIdentity"]["authorityRevision"],
    authority_revision
  );
  assert_eq!(artifact["oracleIdentity"]["bundlePath"], "oracles/quality");
  assert_eq!(artifact["oracleIdentity"]["verifierId"], "quality");
  assert!(
    artifact["oracleIdentity"]["definitionDigest"]
      .as_str()
      .unwrap()
      .starts_with("sha256:")
  );
  let object = git_output(
    repository.path(),
    &[
      "rev-parse",
      &format!("{authority_revision}:oracles/quality"),
    ],
  );
  assert_eq!(artifact["oracleIdentity"]["bundleObjectId"], object.trim());
  let executable = git_output(
    repository.path(),
    &[
      "rev-parse",
      &format!("{authority_revision}:oracles/quality/verify"),
    ],
  );
  assert_eq!(
    artifact["oracleIdentity"]["executableObjectId"],
    executable.trim()
  );
}

#[cfg(unix)]
#[test]
fn candidate_cannot_alter_authority_snapshot_oracle_that_authorizes_done() {
  let repository = Repository::new();
  repository.init();
  let executable = repository.path().join("oracles/quality/verify");
  write_executable(&executable, "#!/bin/sh\nexit 1\n");
  let status =
    repository.configure_with_authority(&["verify"], "authority_snapshot", Some("oracles/quality"));
  repository.propose_and_approve_with_authority(&status, "authority_snapshot");
  let authority_revision = repository.commit("admit rejecting authority oracle");
  write_executable(&executable, "#!/bin/sh\nexit 0\n");
  let revision = repository.commit("candidate replaces oracle");

  let gate = failure_json(repository.gate(&authority_revision, &revision), 2);
  assert_eq!(gate["verdict"], "not_done");
  assert_eq!(gate["blockers"][0]["code"], "authority_surface_changed");
  assert!(
    gate["blockers"][0]["message"]
      .as_str()
      .unwrap()
      .contains("oracles/quality")
  );
}

#[test]
fn distinct_verifiers_receive_independent_candidate_materializations() {
  let repository = Repository::new();
  repository.init();
  fs::write(
    repository.path().join(".tenet/tenet.toml"),
    r#"version = 1
spec_path = "SPEC.md"

[[verifiers]]
id = "contaminator"
argv = ["/bin/sh", "-c", "touch verifier-contamination"]
authority = "project"

[[verifiers]]
id = "observer"
argv = ["/bin/sh", "-c", "test ! -e verifier-contamination"]
authority = "project"
"#,
  )
  .unwrap();
  let status = success_json(repository.status());
  let proposal = json!({
    "schemaVersion": 2,
    "specDigest": status["specDigest"],
    "policyDigest": status["policyDigest"],
    "requirements": [{
      "id": "REQ-001",
      "statement": "verifiers are isolated",
      "obligations": [{
        "id": "REQ-001/VO-001",
        "statement": "first verifier runs",
        "evidenceContract": { "claim": { "verifierId": "contaminator", "authority": "project" } }
      }, {
        "id": "REQ-001/VO-002",
        "statement": "second verifier sees a fresh tree",
        "evidenceContract": { "claim": { "verifierId": "observer", "authority": "project" } }
      }]
    }]
  });
  let proposal_path = repository.path().join("isolation-proposal.json");
  fs::write(&proposal_path, serde_json::to_vec(&proposal).unwrap()).unwrap();
  let proposed = success_json(repository.propose_file(Path::new(proposal_path.to_str().unwrap())));
  success_json(repository.approve(
    proposed["proposalId"].as_str().unwrap(),
    proposed["proposalDigest"].as_str().unwrap(),
  ));
  let revision = repository.commit("admit isolated verifiers");

  let gate = success_json(repository.gate(&revision, &revision));
  assert_eq!(gate["verdict"], "done");
}

#[test]
fn non_ancestor_authority_revision_fails_closed() {
  let repository = Repository::new();
  let base = repository.admitted(&["/usr/bin/true"]);
  fs::write(repository.path().join("candidate.txt"), "candidate\n").unwrap();
  let revision = repository.commit("candidate branch");
  git(
    repository.path(),
    &["checkout", "-q", "-b", "authority-branch", &base],
  );
  fs::write(repository.path().join("authority.txt"), "authority\n").unwrap();
  let authority_revision = repository.commit("divergent authority branch");

  let gate = failure_json(repository.gate(&authority_revision, &revision), 2);
  assert_eq!(
    gate["blockers"][0]["code"],
    "authority_revision_not_ancestor"
  );
}

#[test]
fn protected_verifier_authority_is_not_a_valid_local_policy() {
  let repository = Repository::new();
  repository.init();
  repository.configure(&["/usr/bin/true"]);
  let path = repository.path().join(".tenet/tenet.toml");
  let policy = fs::read_to_string(&path)
    .unwrap()
    .replace("authority = \"project\"", "authority = \"protected\"");
  fs::write(path, policy).unwrap();

  let error = failure_json(repository.status(), 1);
  assert!(
    error["message"]
      .as_str()
      .unwrap()
      .contains("unknown variant")
  );
}

#[test]
fn symbolic_authority_revision_is_rejected() {
  let repository = Repository::new();
  let revision = repository.admitted(&["/usr/bin/true"]);

  let output = repository.gate("HEAD", &revision);
  assert_eq!(output.status.code(), Some(1));
  let error: Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(
    error["message"]
      .as_str()
      .unwrap()
      .contains("resolve authority revision")
  );
}

#[test]
fn git_replacement_objects_cannot_substitute_authority_content() {
  let repository = Repository::new();
  let authority_revision = repository.admitted(&["/usr/bin/false"]);
  git(
    repository.path(),
    &["checkout", "-q", "--orphan", "forged-authority"],
  );
  git(repository.path(), &["rm", "-q", "-r", "--cached", "."]);
  let status = repository.configure(&["/usr/bin/true"]);
  repository.propose_and_approve(&status);
  let replacement_revision = repository.commit("forged replacement authority");
  git(
    repository.path(),
    &["replace", &authority_revision, &replacement_revision],
  );

  let gate = failure_json(repository.gate(&authority_revision, &authority_revision), 2);
  assert_eq!(gate["verdict"], "not_done");
  assert_eq!(gate["obligations"][0]["state"], "contradicted");
}

#[cfg(unix)]
#[test]
fn local_checkout_hook_and_filter_cannot_forge_candidate_files() {
  use std::os::unix::fs::PermissionsExt;

  let repository = Repository::new();
  repository.init();
  fs::write(repository.path().join("result.txt"), "bad\n").unwrap();
  let status = repository.configure(&["/bin/sh", "-c", "test \"$(cat result.txt)\" = honest"]);
  repository.propose_and_approve(&status);
  let authority_revision = repository.commit("admit candidate content verifier");
  fs::write(
    repository.path().join(".gitattributes"),
    "result.txt filter=forge\n",
  )
  .unwrap();
  let revision = repository.commit("request candidate checkout filter");

  git(
    repository.path(),
    &["config", "filter.forge.smudge", "sed s/bad/honest/"],
  );
  git(repository.path(), &["config", "filter.forge.clean", "cat"]);
  git(
    repository.path(),
    &["config", "filter.forge.required", "true"],
  );
  let hook = repository.path().join(".git/hooks/post-checkout");
  fs::write(&hook, "#!/bin/sh\nprintf 'honest\\n' > result.txt\n").unwrap();
  fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();

  let gate = failure_json(repository.gate(&authority_revision, &revision), 2);
  assert_eq!(gate["verdict"], "not_done");
  assert_eq!(gate["obligations"][0]["state"], "contradicted");
}

#[cfg(unix)]
#[test]
fn candidate_symlink_cannot_escape_verifier_working_directory() {
  let repository = Repository::new();
  repository.init();
  fs::create_dir(repository.path().join("checks")).unwrap();
  fs::write(repository.path().join("checks/tracked.txt"), "authority\n").unwrap();
  repository.configure(&["/usr/bin/true"]);
  let policy_path = repository.path().join(".tenet/tenet.toml");
  let policy = fs::read_to_string(&policy_path)
    .unwrap()
    .replace("cwd = \".\"", "cwd = \"checks\"");
  fs::write(policy_path, policy).unwrap();
  let status = success_json(repository.status());
  repository.propose_and_approve(&status);
  let authority_revision = repository.commit("admit verifier working directory");

  fs::remove_dir_all(repository.path().join("checks")).unwrap();
  let external = tempfile::tempdir().unwrap();
  std::os::unix::fs::symlink(external.path(), repository.path().join("checks")).unwrap();
  let revision = repository.commit("redirect verifier working directory");

  let gate = failure_json(repository.gate(&authority_revision, &revision), 4);
  assert_eq!(gate["verdict"], "infrastructure_error");
  assert!(
    gate["blockers"][0]["message"]
      .as_str()
      .unwrap()
      .contains("inside candidate checkout")
  );
}

#[test]
fn disposable_legacy_audit_state_does_not_block_a_new_gate() {
  let repository = Repository::new();
  let revision = repository.admitted(&["/usr/bin/true"]);
  let legacy = json!({
    "schemaVersion": 3,
    "evidence": [],
    "gates": [{
      "schemaVersion": 1,
      "revision": revision,
      "specDigest": "legacy",
      "contractDigest": "legacy",
      "policyDigest": "legacy",
      "verdict": "done",
      "obligations": [],
      "blockers": []
    }]
  });
  fs::write(
    repository.path().join(".tenet/state.json"),
    serde_json::to_vec_pretty(&legacy).unwrap(),
  )
  .unwrap();

  let gate = success_json(repository.gate(&revision, &revision));
  assert_eq!(gate["verdict"], "done");
  assert_eq!(gate["authorityRevision"], revision);
}

#[test]
fn verifier_launch_failure_is_an_infrastructure_error() {
  let repository = Repository::new();
  let revision = repository.admitted(&["/definitely/not/a/tenet-verifier"]);

  let gate = failure_json(repository.gate(&revision, &revision), 4);
  assert_eq!(gate["verdict"], "infrastructure_error");
  assert_eq!(gate["obligations"][0]["state"], "infrastructure_error");
  assert_eq!(gate["blockers"][0]["code"], "verifier_infrastructure_error");
}

#[test]
fn status_never_executes_configured_verifiers() {
  let repository = Repository::new();
  repository.init();
  repository.configure(&["/bin/sh", "-c", "touch verifier-ran"]);

  let status = success_json(repository.status());
  assert_eq!(status["initialized"], true);
  assert!(!repository.path().join("verifier-ran").exists());
}

#[test]
fn exact_revision_gate_ignores_uncommitted_working_tree_changes() {
  let repository = Repository::new();
  let revision = repository.admitted(&["/usr/bin/true"]);
  fs::write(
    repository.path().join("uncommitted.txt"),
    "not part of candidate\n",
  )
  .unwrap();

  let gate = success_json(repository.gate(&revision, &revision));
  assert_eq!(gate["verdict"], "done");
  assert_eq!(gate["revision"], revision);
}

#[test]
fn approval_requires_the_exact_proposal_identity_and_digest() {
  let repository = Repository::new();
  repository.init();
  let status = repository.configure(&["/usr/bin/true"]);
  let proposal = json!({
    "schemaVersion": 2,
    "specDigest": status["specDigest"],
    "policyDigest": status["policyDigest"],
    "requirements": [{
      "id": "REQ-001",
      "statement": "claim",
      "obligations": [{
        "id": "REQ-001/VO-001",
        "statement": "oracle",
        "evidenceContract": { "claim": { "verifierId": "quality", "authority": "project" } }
      }]
    }]
  });
  let proposal_path = repository.path().join("proposal.json");
  fs::write(&proposal_path, serde_json::to_vec(&proposal).unwrap()).unwrap();
  let proposed = success_json(repository.propose_file(Path::new(proposal_path.to_str().unwrap())));

  let output = repository.approve(
    "proposal-wrong",
    proposed["proposalDigest"].as_str().unwrap(),
  );
  assert_eq!(output.status.code(), Some(1));
  let error: Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(error["message"].as_str().unwrap().contains("identity"));

  let output = repository.approve(proposed["proposalId"].as_str().unwrap(), "sha256:wrong");
  assert_eq!(output.status.code(), Some(1));
  let error: Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(error["message"].as_str().unwrap().contains("not found"));
  assert!(!repository.path().join(".tenet/contract.json").exists());
}

#[test]
fn approval_revalidates_pending_proposals_after_specification_or_policy_changes() {
  for mutation in ["specification", "policy"] {
    let repository = Repository::new();
    repository.init();
    let status = repository.configure(&["/usr/bin/true"]);
    let proposal = json!({
      "schemaVersion": 2,
      "specDigest": status["specDigest"],
      "policyDigest": status["policyDigest"],
      "requirements": [{
        "id": "REQ-001",
        "statement": "claim",
        "obligations": [{
          "id": "REQ-001/VO-001",
          "statement": "oracle",
          "evidenceContract": { "claim": { "verifierId": "quality", "authority": "project" } }
        }]
      }]
    });
    let proposal_path = repository.path().join("proposal.json");
    fs::write(&proposal_path, serde_json::to_vec(&proposal).unwrap()).unwrap();
    let proposed =
      success_json(repository.propose_file(Path::new(proposal_path.to_str().unwrap())));

    match mutation {
      "specification" => {
        fs::write(repository.path().join("SPEC.md"), "# Changed behavior\n").unwrap()
      }
      "policy" => {
        let policy_path = repository.path().join(".tenet/tenet.toml");
        let policy = fs::read_to_string(&policy_path).unwrap();
        fs::write(
          &policy_path,
          policy.replace("timeout_seconds = 10", "timeout_seconds = 11"),
        )
        .unwrap();
      }
      _ => unreachable!(),
    }

    let output = repository.approve(
      proposed["proposalId"].as_str().unwrap(),
      proposed["proposalDigest"].as_str().unwrap(),
    );
    assert_eq!(
      output.status.code(),
      Some(1),
      "{mutation} change admitted proposal"
    );
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(error["message"].as_str().unwrap().contains("stale"));
    assert!(!repository.path().join(".tenet/contract.json").exists());
  }
}

#[test]
fn inconclusive_verifier_observation_returns_inconclusive_verdict() {
  let repository = Repository::new();
  let revision = repository.admitted(&["/bin/sh", "-c", "exit 126"]);

  let gate = failure_json(repository.gate(&revision, &revision), 3);
  assert_eq!(gate["verdict"], "inconclusive");
  assert_eq!(gate["obligations"][0]["state"], "inconclusive");
  assert_eq!(gate["blockers"][0]["code"], "verifier_inconclusive");
}

#[test]
fn gate_rejects_symbolic_or_abbreviated_revisions() {
  let repository = Repository::new();
  let authority_revision = repository.admitted(&["/usr/bin/true"]);

  for revision in ["HEAD", "deadbeef"] {
    let output = repository.gate(&authority_revision, revision);
    assert_eq!(output.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
      error["message"]
        .as_str()
        .unwrap()
        .contains("full immutable Git commit ID")
    );
  }
}

#[test]
fn verifier_timeout_terminates_the_process_group_and_returns_infrastructure_error() {
  let repository = Repository::new();
  repository.init();
  repository.configure(&["/bin/sh", "-c", "sleep 30 & wait"]);
  let path = repository.path().join(".tenet/tenet.toml");
  let policy = fs::read_to_string(&path)
    .unwrap()
    .replace("timeout_seconds = 10", "timeout_seconds = 1");
  fs::write(path, policy).unwrap();
  let status = success_json(repository.status());
  repository.propose_and_approve(&status);
  let revision = repository.commit("admit timeout verifier");

  let started = Instant::now();
  let gate = failure_json(repository.gate(&revision, &revision), 4);
  assert_eq!(gate["verdict"], "infrastructure_error");
  assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn project_verifier_cannot_be_admitted_as_oracle_assurance() {
  let repository = Repository::new();
  repository.init();
  fs::write(
    repository.path().join(".tenet/tenet.toml"),
    r#"version = 1
spec_path = "SPEC.md"

[[verifiers]]
id = "quality"
argv = ["/usr/bin/true"]
authority = "project"

[[verifiers]]
id = "candidate-assurance"
argv = ["/usr/bin/true"]
authority = "project"
"#,
  )
  .unwrap();
  let status = success_json(repository.status());
  let proposal = json!({
    "schemaVersion": 2,
    "specDigest": status["specDigest"],
    "policyDigest": status["policyDigest"],
    "requirements": [{
      "id": "REQ-001",
      "statement": "claim",
      "obligations": [{
        "id": "REQ-001/VO-001",
        "statement": "quality passes",
        "evidenceContract": {
          "claim": { "verifierId": "quality", "authority": "project" },
          "assurances": [{
            "id": "ASSURE-001",
            "criterion": "the primary oracle rejects a seeded defect",
            "verifierId": "candidate-assurance",
            "authority": "project"
          }]
        }
      }]
    }]
  });
  let path = repository.path().join("project-assurance.json");
  fs::write(&path, serde_json::to_vec(&proposal).unwrap()).unwrap();

  let output = repository.propose_file(Path::new(path.to_str().unwrap()));
  assert!(!output.status.success());
  let error: Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(
    error["message"]
      .as_str()
      .unwrap()
      .contains("authority_snapshot")
  );
}

#[test]
fn oracle_cannot_admit_itself_as_assurance() {
  let repository = Repository::new();
  repository.init();
  fs::write(
    repository.path().join(".tenet/tenet.toml"),
    r#"version = 1
spec_path = "SPEC.md"

[[verifiers]]
id = "snapshot-quality"
argv = ["quality/verify"]
authority = "authority_snapshot"
oracle_path = "./oracles"

[[verifiers]]
id = "same-oracle-assurance"
argv = ["verify"]
authority = "authority_snapshot"
oracle_path = "oracles/quality/"
"#,
  )
  .unwrap();
  let status = success_json(repository.status());
  let proposal = json!({
    "schemaVersion": 2,
    "specDigest": status["specDigest"],
    "policyDigest": status["policyDigest"],
    "requirements": [{
      "id": "REQ-001",
      "statement": "claim",
      "obligations": [{
        "id": "REQ-001/VO-001",
        "statement": "quality passes",
        "evidenceContract": {
          "claim": { "verifierId": "snapshot-quality", "authority": "authority_snapshot" },
          "assurances": [{
            "id": "ASSURE-001",
            "criterion": "the primary oracle rejects a seeded defect",
            "verifierId": "same-oracle-assurance",
            "authority": "authority_snapshot"
          }]
        }
      }]
    }]
  });
  let path = repository.path().join("self-assurance.json");
  fs::write(&path, serde_json::to_vec(&proposal).unwrap()).unwrap();

  let output = repository.propose_file(Path::new(path.to_str().unwrap()));
  assert!(!output.status.success());
  let error: Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(
    error["message"]
      .as_str()
      .unwrap()
      .contains("cannot use the primary oracle")
  );
}

#[test]
fn nested_assurance_contract_is_rejected() {
  let repository = Repository::new();
  repository.init();
  let status = repository.configure(&["/usr/bin/true"]);
  let proposal = json!({
    "schemaVersion": 2,
    "specDigest": status["specDigest"],
    "policyDigest": status["policyDigest"],
    "requirements": [{
      "id": "REQ-001",
      "statement": "claim",
      "obligations": [{
        "id": "REQ-001/VO-001",
        "statement": "quality passes",
        "evidenceContract": {
          "claim": { "verifierId": "quality", "authority": "project" },
          "assurances": [{
            "id": "ASSURE-001",
            "criterion": "criterion",
            "verifierId": "quality",
            "authority": "project",
            "assurances": []
          }]
        }
      }]
    }]
  });
  let path = repository.path().join("nested-assurance.json");
  fs::write(&path, serde_json::to_vec(&proposal).unwrap()).unwrap();

  let output = repository.propose_file(Path::new(path.to_str().unwrap()));
  assert!(!output.status.success());
  let error: Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(error["message"].as_str().unwrap().contains("unknown field"));
}

#[cfg(unix)]
#[test]
fn failed_required_assurance_blocks_done_and_records_distinct_evidence() {
  let repository = Repository::new();
  repository.init();
  write_executable(
    &repository.path().join("oracles/assurance/verify"),
    "#!/bin/sh\nexit 1\n",
  );
  fs::write(
    repository.path().join(".tenet/tenet.toml"),
    r#"version = 1
spec_path = "SPEC.md"

[[verifiers]]
id = "quality"
argv = ["/usr/bin/true"]
authority = "project"

[[verifiers]]
id = "mutation-assurance"
argv = ["verify"]
authority = "authority_snapshot"
oracle_path = "oracles/assurance"
"#,
  )
  .unwrap();
  let status = success_json(repository.status());
  let proposal = json!({
    "schemaVersion": 2,
    "specDigest": status["specDigest"],
    "policyDigest": status["policyDigest"],
    "requirements": [{
      "id": "REQ-001",
      "statement": "claim",
      "obligations": [{
        "id": "REQ-001/VO-001",
        "statement": "quality passes",
        "evidenceContract": {
          "claim": { "verifierId": "quality", "authority": "project" },
          "assurances": [{
            "id": "ASSURE-001",
            "criterion": "the primary oracle rejects a seeded defect",
            "verifierId": "mutation-assurance",
            "authority": "authority_snapshot"
          }]
        }
      }]
    }]
  });
  let path = repository.path().join("assurance.json");
  fs::write(&path, serde_json::to_vec(&proposal).unwrap()).unwrap();
  let proposed = success_json(repository.propose_file(Path::new(path.to_str().unwrap())));
  success_json(repository.approve(
    proposed["proposalId"].as_str().unwrap(),
    proposed["proposalDigest"].as_str().unwrap(),
  ));
  let revision = repository.commit("admit assured contract");

  let gate = failure_json(repository.gate(&revision, &revision), 2);
  assert_eq!(gate["verdict"], "not_done");
  assert_eq!(gate["obligations"][0]["state"], "unverifiable");
  assert_eq!(gate["blockers"][0]["code"], "oracle_assurance_failed");
  let evidence = success_json(repository.evidence(&revision));
  assert_eq!(evidence["artifacts"][0]["evidenceKind"], "claim");
  assert_eq!(evidence["artifacts"][1]["evidenceKind"], "oracle_assurance");
  assert_eq!(
    evidence["artifacts"][1]["qualifiedOracleIdentity"]["authority"],
    "project"
  );
  assert_eq!(
    evidence["artifacts"][1]["qualifiedOracleIdentity"]["verifierId"],
    "quality"
  );
  assert_eq!(
    evidence["artifacts"][1]["qualifiedOracleIdentity"]["candidateRevision"],
    revision
  );
}

#[test]
fn declared_environment_clears_inherited_variables_and_has_stable_identity() {
  let repository = Repository::new();
  repository.init();
  fs::write(
    repository.path().join(".tenet/tenet.toml"),
    r#"version = 1
spec_path = "SPEC.md"

[[verifiers]]
id = "quality"
argv = ["/bin/sh", "-c", "test -z \"$HOME\" && test \"$ONLY_DECLARED\" = yes && test -n \"$TENET_AUTHORITY_REVISION\" && test -n \"$TENET_CANDIDATE_REVISION\""]
authority = "project"
environment_mode = "declared"
env = { ONLY_DECLARED = "yes" }
"#,
  )
  .unwrap();
  let status = success_json(repository.status());
  repository.propose_and_approve(&status);
  let revision = repository.commit("admit declared environment verifier");

  success_json(repository.gate(&revision, &revision));
  success_json(repository.gate(&revision, &revision));
  let evidence = success_json(repository.evidence(&revision));
  assert_eq!(
    evidence["artifacts"][0]["execution"]["environmentMode"],
    "declared"
  );
  assert_eq!(
    evidence["artifacts"][0]["execution"]["executionEnvironmentIdentity"],
    evidence["artifacts"][2]["execution"]["executionEnvironmentIdentity"]
  );
}

#[test]
fn missing_assurance_bundle_returns_typed_infrastructure_verdict() {
  let repository = Repository::new();
  repository.init();
  fs::write(
    repository.path().join(".tenet/tenet.toml"),
    r#"version = 1
spec_path = "SPEC.md"

[[verifiers]]
id = "quality"
argv = ["/usr/bin/true"]
authority = "project"

[[verifiers]]
id = "missing-assurance"
argv = ["verify"]
authority = "authority_snapshot"
oracle_path = "oracles/missing"
"#,
  )
  .unwrap();
  let status = success_json(repository.status());
  let proposal = json!({
    "schemaVersion": 2,
    "specDigest": status["specDigest"],
    "policyDigest": status["policyDigest"],
    "requirements": [{
      "id": "REQ-001",
      "statement": "claim",
      "obligations": [{
        "id": "REQ-001/VO-001",
        "statement": "quality passes",
        "evidenceContract": {
          "claim": { "verifierId": "quality", "authority": "project" },
          "assurances": [{
            "id": "ASSURE-001",
            "criterion": "the primary oracle rejects a seeded defect",
            "verifierId": "missing-assurance",
            "authority": "authority_snapshot"
          }]
        }
      }]
    }]
  });
  let path = repository.path().join("missing-assurance.json");
  fs::write(&path, serde_json::to_vec(&proposal).unwrap()).unwrap();
  let proposed = success_json(repository.propose_file(Path::new(path.to_str().unwrap())));
  success_json(repository.approve(
    proposed["proposalId"].as_str().unwrap(),
    proposed["proposalDigest"].as_str().unwrap(),
  ));
  let revision = repository.commit("admit missing assurance bundle");

  let gate = failure_json(repository.gate(&revision, &revision), 4);
  assert_eq!(gate["verdict"], "infrastructure_error");
  assert_eq!(gate["obligations"][0]["state"], "infrastructure_error");
  assert_eq!(gate["blockers"][0]["code"], "verifier_infrastructure_error");
  assert_eq!(gate["blockers"][0]["verifierId"], "missing-assurance");
}

#[cfg(unix)]
#[test]
fn symlinked_primary_executable_cannot_self_assure_done() {
  use std::os::unix::fs::symlink;

  let repository = Repository::new();
  repository.init();
  write_executable(
    &repository.path().join("oracles/quality/verify"),
    "#!/bin/sh\nexit 0\n",
  );
  symlink("quality", repository.path().join("oracles/quality-link")).unwrap();
  fs::write(
    repository.path().join(".tenet/tenet.toml"),
    r#"version = 1
spec_path = "SPEC.md"

[[verifiers]]
id = "snapshot-quality"
argv = ["quality-link/verify"]
authority = "authority_snapshot"
oracle_path = "oracles"

[[verifiers]]
id = "same-executable-assurance"
argv = ["verify"]
authority = "authority_snapshot"
oracle_path = "oracles/quality"
"#,
  )
  .unwrap();
  let status = success_json(repository.status());
  let proposal = json!({
    "schemaVersion": 2,
    "specDigest": status["specDigest"],
    "policyDigest": status["policyDigest"],
    "requirements": [{
      "id": "REQ-001",
      "statement": "claim",
      "obligations": [{
        "id": "REQ-001/VO-001",
        "statement": "quality passes",
        "evidenceContract": {
          "claim": {
            "verifierId": "snapshot-quality",
            "authority": "authority_snapshot"
          },
          "assurances": [{
            "id": "ASSURE-001",
            "criterion": "the primary oracle rejects a seeded defect",
            "verifierId": "same-executable-assurance",
            "authority": "authority_snapshot"
          }]
        }
      }]
    }]
  });
  let path = repository.path().join("symlink-assurance.json");
  fs::write(&path, serde_json::to_vec(&proposal).unwrap()).unwrap();
  let proposed = success_json(repository.propose_file(Path::new(path.to_str().unwrap())));
  success_json(repository.approve(
    proposed["proposalId"].as_str().unwrap(),
    proposed["proposalDigest"].as_str().unwrap(),
  ));
  let revision = repository.commit("admit symlinked primary oracle");

  let gate = failure_json(repository.gate(&revision, &revision), 4);
  assert_eq!(gate["verdict"], "infrastructure_error");
  assert_eq!(gate["obligations"][0]["state"], "infrastructure_error");
  assert_eq!(gate["blockers"][0]["verifierId"], "snapshot-quality");
}
