use std::{
  fs,
  path::Path,
  process::Command,
  time::{Duration, Instant},
};

use serde_json::{json, Value};

struct Repository {
  directory: tempfile::TempDir,
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

  fn gate(&self, authority_revision: &str, revision: &str) -> std::process::Output {
    self.tenet(&[
      "gate",
      "--authority-revision",
      authority_revision,
      "--revision",
      revision,
      "--json",
    ])
  }

  fn init(&self) -> Value {
    success_json(self.tenet(&["init", "--spec", "SPEC.md", "--json"]))
  }

  fn configure(&self, command: &[&str]) -> Value {
    let argv = command
      .iter()
      .map(|item| format!("\"{}\"", item.replace('"', "\\\"")))
      .collect::<Vec<_>>()
      .join(", ");
    fs::write(
      self.path().join(".tenet/tenet.toml"),
      format!(
        "version = 1\nspec_path = \"SPEC.md\"\n\n[[verifiers]]\nid = \"quality\"\nargv = [{argv}]\ncwd = \".\"\ntimeout_seconds = 10\nmax_output_bytes = 4096\nauthority = \"project\"\n"
      ),
    )
    .expect("write policy");
    success_json(self.tenet(&["status", "--json"]))
  }

  fn propose_and_approve(&self, status: &Value) -> Value {
    let proposal = json!({
      "schemaVersion": 1,
      "specDigest": status["specDigest"],
      "policyDigest": status["policyDigest"],
      "requirements": [{
        "id": "REQ-001",
        "statement": "The required behavior is implemented",
        "obligations": [{
          "id": "REQ-001/VO-001",
          "statement": "The configured project verifier succeeds",
          "evidenceContract": { "verifierId": "quality" }
        }, {
          "id": "REQ-001/VO-002",
          "statement": "The configured project verifier confirms the candidate",
          "evidenceContract": { "verifierId": "quality" }
        }]
      }]
    });
    let proposal_path = self.path().join("proposal.json");
    fs::write(
      &proposal_path,
      serde_json::to_vec_pretty(&proposal).expect("encode proposal"),
    )
    .expect("write proposal");
    let proposed = success_json(self.tenet(&[
      "contract",
      "propose",
      "--file",
      proposal_path.to_str().expect("UTF-8 path"),
      "--json",
    ]));
    success_json(
      self.tenet(&[
        "contract",
        "approve",
        "--proposal",
        proposed["proposalId"].as_str().expect("proposal ID"),
        "--digest",
        proposed["proposalDigest"]
          .as_str()
          .expect("proposal digest"),
        "--json",
      ]),
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

fn success_json(output: std::process::Output) -> Value {
  assert!(
    output.status.success(),
    "command failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  serde_json::from_slice(&output.stdout).expect("JSON response")
}

fn failure_json(output: std::process::Output, expected_code: i32) -> Value {
  assert_eq!(output.status.code(), Some(expected_code));
  serde_json::from_slice(&output.stdout).expect("JSON failure response")
}

#[test]
fn init_is_idempotent_and_generates_only_repository_local_tenet_files() {
  let repository = Repository::new();
  let agents_before = fs::read(repository.path().join("AGENTS.md")).expect("read AGENTS");
  let claude_before = fs::read(repository.path().join("CLAUDE.md")).expect("read CLAUDE");

  let first = repository.init();
  let config_before = fs::read(repository.path().join(".tenet/tenet.toml")).expect("read config");
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
  assert!(!repository.path().join(".mcp.json").exists());
}

#[test]
fn generated_skill_is_portable_concise_and_not_authoritative() {
  let repository = Repository::new();
  repository.init();
  let skill = fs::read_to_string(repository.path().join(".agents/skills/tenet/SKILL.md"))
    .expect("read skill");

  assert!(skill.starts_with("---\nname: tenet\n"));
  assert!(skill.contains("tenet-skill-version: \"1\""));
  assert!(skill.contains("Never invoke `tenet contract approve` yourself"));
  assert!(skill.contains("Never choose or advance A yourself"));
  assert!(skill
    .contains("tenet gate --authority-revision <authority-sha> --revision <candidate-sha> --json"));
  assert!(skill.contains("exact (A, R) pair"));
  assert!(skill.lines().count() < 40);
  for forbidden in [
    "Codex",
    "Claude",
    "Gemini",
    "MCP",
    "ACP",
    "contract.json",
    "verifier_id",
  ] {
    assert!(!skill.contains(forbidden), "skill contains {forbidden}");
  }
}

#[test]
fn proposal_schema_validation_and_operator_admission_are_separate() {
  let repository = Repository::new();
  repository.init();
  let status = repository.configure(&["/usr/bin/true"]);
  let schema = success_json(repository.tenet(&["contract", "schema", "--json"]));
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
    "schemaVersion": 1,
    "specDigest": status["specDigest"],
    "policyDigest": status["policyDigest"],
    "requirements": [{
      "id": "REQ-001",
      "statement": "claim",
      "obligations": [{
        "id": "REQ-001/VO-001",
        "statement": "oracle",
        "evidenceContract": { "verifierId": "agent-selected-shell" }
      }]
    }]
  });
  fs::write(
    repository.path().join("bad.json"),
    serde_json::to_vec(&proposal).unwrap(),
  )
  .unwrap();
  let output = repository.tenet(&[
    "contract",
    "propose",
    "--file",
    repository.path().join("bad.json").to_str().unwrap(),
    "--json",
  ]);
  assert!(!output.status.success());
  let error: Value = serde_json::from_slice(&output.stdout).expect("typed JSON error");
  assert!(error["message"]
    .as_str()
    .unwrap()
    .contains("unknown verifier"));
  assert!(!repository.path().join(".tenet/contract.json").exists());
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
  let evidence = success_json(repository.tenet(&["evidence", "--revision", &revision, "--json"]));
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
  let status = success_json(repository.tenet(&["status", "--json"]));
  assert_eq!(status["lastGatedAuthorityRevision"], revision);
}

#[test]
fn failed_verifier_returns_not_done_with_contradiction() {
  let repository = Repository::new();
  let revision = repository.admitted(&["/usr/bin/false"]);

  let gate = failure_json(repository.gate(&revision, &revision), 2);
  assert_eq!(gate["verdict"], "not_done");
  assert_eq!(gate["obligations"][0]["state"], "contradicted");
  assert!(gate["blockers"]
    .as_array()
    .unwrap()
    .iter()
    .any(|item| item["code"] == "contradiction_observed"));
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
  let gate = success_json(
    Command::new(env!("CARGO_BIN_EXE_tenet"))
      .arg("--cwd")
      .arg(clone.path())
      .args([
        "gate",
        "--authority-revision",
        &authority_revision,
        "--revision",
        &revision,
        "--json",
      ])
      .env_remove("OPENAI_API_KEY")
      .env_remove("ANTHROPIC_API_KEY")
      .output()
      .expect("gate fresh clone"),
  );
  assert_eq!(gate["verdict"], "done");
}

#[test]
fn cli_only_workflow_remains_functional_without_the_generated_skill() {
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
  assert!(gate["blockers"][0]["message"]
    .as_str()
    .unwrap()
    .contains(".tenet/tenet.toml"));
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
  assert!(gate["blockers"][0]["message"]
    .as_str()
    .unwrap()
    .contains(".tenet/contract.json"));
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

  let output = repository.tenet(&["status", "--json"]);
  assert_eq!(output.status.code(), Some(1));
  let error: Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(error["message"]
    .as_str()
    .unwrap()
    .contains("unknown variant"));
}

#[test]
fn symbolic_authority_revision_is_rejected() {
  let repository = Repository::new();
  let revision = repository.admitted(&["/usr/bin/true"]);

  let output = repository.gate("HEAD", &revision);
  assert_eq!(output.status.code(), Some(1));
  let error: Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(error["message"]
    .as_str()
    .unwrap()
    .contains("resolve authority revision"));
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
  let status = success_json(repository.tenet(&["status", "--json"]));
  repository.propose_and_approve(&status);
  let authority_revision = repository.commit("admit verifier working directory");

  fs::remove_dir_all(repository.path().join("checks")).unwrap();
  let external = tempfile::tempdir().unwrap();
  std::os::unix::fs::symlink(external.path(), repository.path().join("checks")).unwrap();
  let revision = repository.commit("redirect verifier working directory");

  let gate = failure_json(repository.gate(&authority_revision, &revision), 4);
  assert_eq!(gate["verdict"], "infrastructure_error");
  assert!(gate["blockers"][0]["message"]
    .as_str()
    .unwrap()
    .contains("inside candidate checkout"));
}

#[test]
fn disposable_legacy_audit_state_does_not_block_a_new_gate() {
  let repository = Repository::new();
  let revision = repository.admitted(&["/usr/bin/true"]);
  let legacy = json!({
    "schemaVersion": 1,
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

  let status = success_json(repository.tenet(&["status", "--json"]));
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
    "schemaVersion": 1,
    "specDigest": status["specDigest"],
    "policyDigest": status["policyDigest"],
    "requirements": [{
      "id": "REQ-001",
      "statement": "claim",
      "obligations": [{
        "id": "REQ-001/VO-001",
        "statement": "oracle",
        "evidenceContract": { "verifierId": "quality" }
      }]
    }]
  });
  let proposal_path = repository.path().join("proposal.json");
  fs::write(&proposal_path, serde_json::to_vec(&proposal).unwrap()).unwrap();
  let proposed = success_json(repository.tenet(&[
    "contract",
    "propose",
    "--file",
    proposal_path.to_str().unwrap(),
    "--json",
  ]));

  let output = repository.tenet(&[
    "contract",
    "approve",
    "--proposal",
    "proposal-wrong",
    "--digest",
    proposed["proposalDigest"].as_str().unwrap(),
    "--json",
  ]);
  assert_eq!(output.status.code(), Some(1));
  let error: Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(error["message"].as_str().unwrap().contains("identity"));
  assert!(!repository.path().join(".tenet/contract.json").exists());
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
    assert!(error["message"]
      .as_str()
      .unwrap()
      .contains("full immutable Git commit ID"));
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
  let status = success_json(repository.tenet(&["status", "--json"]));
  repository.propose_and_approve(&status);
  let revision = repository.commit("admit timeout verifier");

  let started = Instant::now();
  let gate = failure_json(repository.gate(&revision, &revision), 4);
  assert_eq!(gate["verdict"], "infrastructure_error");
  assert!(started.elapsed() < Duration::from_secs(5));
}
