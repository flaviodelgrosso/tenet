use std::{
  fmt::Write as _,
  fs::OpenOptions,
  io::Write,
  path::{Path, PathBuf},
  process::Command,
};

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use tokio::fs;

use tenet_domain::{
  config::{Config, TENET_DIR},
  evidence::EvidenceGraphState,
  model::{
    CompletedWorkUnit, IntegrationPhase, IntegrationTransaction, ReconcileResult,
    RequirementCatalog, State,
  },
  verification::ProjectVerificationRun,
};

pub const STATE_FILE: &str = "state.json";
pub const REQUIREMENTS_FILE: &str = "requirements.json";
pub const ROADMAP_FILE: &str = "roadmap.json";
pub const INTEGRATION_JOURNAL_FILE: &str = "integration-journal.json";
pub const EVIDENCE_GRAPH_FILE: &str = "evidence/graph.json";

pub async fn ensure_layout(cwd: &Path) -> Result<()> {
  fs::create_dir_all(cwd.join(TENET_DIR).join("evidence")).await?;
  fs::create_dir_all(cwd.join(TENET_DIR).join("runs")).await?;
  Ok(())
}

pub async fn ensure_spec(cwd: &Path, config: &Config) -> Result<()> {
  let path = cwd.join(&config.spec_file);
  if !path.exists() {
    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent).await?;
    }
    fs::write(path, SPEC_TEMPLATE).await?;
  }
  Ok(())
}

pub async fn ensure_gitignore(cwd: &Path) -> Result<()> {
  let path = cwd.join(".gitignore");
  let mut text = match fs::read_to_string(&path).await {
    Ok(v) => v,
    Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
    Err(err) => return Err(err.into()),
  };
  if !text.lines().any(|v| v.trim() == ".tenet/") {
    if !text.is_empty() && !text.ends_with('\n') {
      text.push('\n');
    }
    text.push_str(".tenet/\n");
    fs::write(path, text).await?;
  }
  Ok(())
}

pub async fn read_state(cwd: &Path) -> Result<State> {
  let path = cwd.join(TENET_DIR).join(STATE_FILE);
  if !path.exists() {
    return Ok(State::fresh());
  }
  let value: serde_json::Value = read_json(path).await?;
  let version = value
    .get("version")
    .and_then(serde_json::Value::as_u64)
    .ok_or_else(|| anyhow!("state file has no numeric version"))?;
  if version != u64::from(State::VERSION) {
    return Err(anyhow!(
      "unsupported state version {version}; expected {}",
      State::VERSION
    ));
  }
  let state = serde_json::from_value(value).context("deserialize current state")?;
  validate_state(&state)?;
  Ok(state)
}

fn validate_state(state: &State) -> Result<()> {
  use tenet_domain::model::{Phase, RunStatus};

  if state.status == RunStatus::Done && state.phase != Phase::Complete {
    return Err(anyhow!(
      "invalid persisted state: done status requires complete phase"
    ));
  }
  if state.status == RunStatus::Idle
    && (!state.active_leases.is_empty() || !state.candidate_integrations.is_empty())
  {
    return Err(anyhow!(
      "invalid persisted state: idle state cannot contain active leases or candidate integrations"
    ));
  }
  if state.current_repair.is_some()
    && (state.status != RunStatus::Running || state.phase != Phase::Repairing)
  {
    return Err(anyhow!(
      "invalid persisted state: current repair requires running/repairing state"
    ));
  }
  Ok(())
}

pub async fn write_state(cwd: &Path, state: &State) -> Result<()> {
  write_json_atomic(cwd.join(TENET_DIR).join(STATE_FILE), state).await
}

pub async fn read_catalog(cwd: &Path) -> Result<Option<RequirementCatalog>> {
  let path = cwd.join(TENET_DIR).join(REQUIREMENTS_FILE);
  if !path.exists() {
    return Ok(None);
  }
  Ok(Some(read_json(path).await?))
}

pub async fn write_catalog(cwd: &Path, catalog: &RequirementCatalog) -> Result<()> {
  write_json_atomic(cwd.join(TENET_DIR).join(REQUIREMENTS_FILE), catalog).await
}

pub async fn write_roadmap(cwd: &Path, value: &ReconcileResult) -> Result<()> {
  write_json_atomic(cwd.join(TENET_DIR).join(ROADMAP_FILE), value).await
}
pub async fn read_evidence_graph(
  cwd: &Path,
  expected: &EvidenceGraphState,
) -> Result<EvidenceGraphState> {
  let path = cwd.join(TENET_DIR).join(EVIDENCE_GRAPH_FILE);
  if !path.exists() {
    return Ok(expected.clone());
  }
  let mut value: serde_json::Value = read_json(path).await?;
  let version = value
    .get("version")
    .and_then(serde_json::Value::as_u64)
    .ok_or_else(|| anyhow!("evidence graph has no numeric version"))?;
  if version == 0 {
    value["version"] = serde_json::Value::from(EvidenceGraphState::VERSION);
  } else if version != u64::from(EvidenceGraphState::VERSION) {
    return Err(anyhow!(
      "unsupported evidence graph version {version}; expected {}",
      EvidenceGraphState::VERSION
    ));
  }
  let mut graph: EvidenceGraphState =
    serde_json::from_value(value).context("deserialize evidence graph")?;
  if graph.requirements.is_empty() {
    graph.requirements.clone_from(&expected.requirements);
  }
  if graph.specification_hash != expected.specification_hash {
    return Ok(expected.clone());
  }
  if graph.requirements != expected.requirements
    || graph.required_requirements != expected.required_requirements
    || graph.criteria != expected.criteria
    || graph.obligations != expected.obligations
  {
    return Err(anyhow!(
      "persisted evidence graph structure does not match the requirement catalog"
    ));
  }
  Ok(graph)
}

pub async fn write_evidence_graph(cwd: &Path, graph: &EvidenceGraphState) -> Result<()> {
  write_json_atomic(cwd.join(TENET_DIR).join(EVIDENCE_GRAPH_FILE), graph).await
}

pub async fn save_evidence<T: serde::Serialize>(
  cwd: &Path,
  name: &str,
  value: &T,
) -> Result<PathBuf> {
  let path = cwd
    .join(TENET_DIR)
    .join("evidence")
    .join(format!("{name}.json"));
  write_json_atomic(path.clone(), value).await?;
  Ok(path)
}
pub async fn read_integration_journal(cwd: &Path) -> Result<Option<IntegrationTransaction>> {
  let path = cwd.join(TENET_DIR).join(INTEGRATION_JOURNAL_FILE);
  if !path.exists() {
    return Ok(None);
  }
  let transaction: IntegrationTransaction = read_json(path).await?;
  if transaction.version != IntegrationTransaction::VERSION {
    return Err(anyhow!(
      "unsupported integration journal version {}; expected {}",
      transaction.version,
      IntegrationTransaction::VERSION
    ));
  }
  Ok(Some(transaction))
}

pub async fn write_integration_journal(
  cwd: &Path,
  transaction: &IntegrationTransaction,
) -> Result<()> {
  write_json_atomic(
    cwd.join(TENET_DIR).join(INTEGRATION_JOURNAL_FILE),
    transaction,
  )
  .await
}

pub async fn remove_integration_journal(cwd: &Path) -> Result<()> {
  let path = cwd.join(TENET_DIR).join(INTEGRATION_JOURNAL_FILE);
  match fs::remove_file(path).await {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error.into()),
  }
}

pub async fn recover_integration(cwd: &Path, state: &mut State, config: &Config) -> Result<()> {
  let Some(transaction) = read_integration_journal(cwd).await? else {
    return Ok(());
  };
  let head = crate::git::head(cwd).await?;
  if head == transaction.old_head && transaction.phase == IntegrationPhase::Prepared {
    remove_integration_journal(cwd).await?;
    return Ok(());
  }
  if head != transaction.new_head {
    return Err(anyhow!(
      "integration recovery failed closed: canonical HEAD {head} matches neither expected old {} nor intended new {} for transaction {}",
      transaction.old_head,
      transaction.new_head,
      transaction.id
    ));
  }
  verify_transaction_evidence(cwd, &transaction, &config.verification.suite_hash()?).await?;
  if !state.completed_work_units.iter().any(|completed| {
    completed.work_unit == transaction.work_unit
      && completed.verification_evidence == transaction.verification_evidence
  }) {
    state.completed_work_units.push(CompletedWorkUnit {
      work_unit: transaction.work_unit.clone(),
      completed_at: transaction.updated_at.clone(),
      verification_evidence: transaction.verification_evidence.clone(),
    });
  }
  state.candidate_integrations.clear();
  write_state(cwd, state).await?;
  remove_integration_journal(cwd).await
}

pub fn project_verification_hash(report: &ProjectVerificationRun) -> Result<String> {
  let bytes = serde_json::to_vec(report)?;
  Ok(sha256_hex(&bytes))
}

async fn verify_transaction_evidence(
  cwd: &Path,
  transaction: &IntegrationTransaction,
  expected_suite_hash: &str,
) -> Result<()> {
  let report: ProjectVerificationRun =
    read_json(cwd.join(&transaction.verification_evidence)).await?;
  if report.suite_hash != expected_suite_hash {
    return Err(anyhow!(
      "integration project verification evidence uses stale suite {}; current suite is {}",
      report.suite_hash,
      expected_suite_hash
    ));
  }
  let actual = project_verification_hash(&report)?;
  if actual != transaction.verification_hash {
    return Err(anyhow!(
      "integration evidence hash mismatch for transaction {}",
      transaction.id
    ));
  }
  Ok(())
}

pub async fn spec_text_and_hash(cwd: &Path, config: &Config) -> Result<(String, String)> {
  let path = cwd.join(&config.spec_file);
  let text = fs::read_to_string(&path)
    .await
    .with_context(|| format!("read authoritative spec {}", path.display()))?;
  let hash = sha256_hex(text.as_bytes());
  Ok((text, hash))
}

fn sha256_hex(bytes: &[u8]) -> String {
  let digest = Sha256::digest(bytes);
  let mut hash = String::with_capacity(digest.len() * 2);
  for byte in digest {
    write!(hash, "{byte:02x}").expect("writing to String cannot fail");
  }
  hash
}

pub struct RunLock {
  path: PathBuf,
}

impl RunLock {
  pub fn acquire(cwd: &Path) -> Result<Self> {
    let path = cwd.join(TENET_DIR).join("run.lock");
    if path.exists() {
      if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
          if let Some(pid) = value.get("pid").and_then(|v| v.as_u64()) {
            if process_alive(pid as u32) {
              return Err(anyhow!("another tenet run is active (pid {pid})"));
            }
          }
        }
      }
      let _ = std::fs::remove_file(&path);
    }
    let mut file = OpenOptions::new()
      .create_new(true)
      .write(true)
      .open(&path)?;
    let payload =
      serde_json::json!({"pid":std::process::id(),"startedAt":chrono::Utc::now().to_rfc3339()});
    writeln!(file, "{}", serde_json::to_string(&payload)?)?;
    Ok(Self { path })
  }
}

impl Drop for RunLock {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.path);
  }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
  Command::new("kill")
    .args(["-0", &pid.to_string()])
    .status()
    .map(|s| s.success())
    .unwrap_or(false)
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
  let pid = pid.to_string();
  let output = Command::new("tasklist")
    .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
    .output();

  output
    .map(|output| {
      String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.split("\",\"").nth(1).is_some_and(|value| value == pid))
    })
    .unwrap_or(false)
}

#[cfg(all(not(unix), not(windows)))]
fn process_alive(_pid: u32) -> bool {
  false
}

async fn read_json<T: serde::de::DeserializeOwned>(path: PathBuf) -> Result<T> {
  let bytes = fs::read(&path)
    .await
    .with_context(|| format!("read {}", path.display()))?;
  serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

async fn write_json_atomic<T: serde::Serialize>(path: PathBuf, value: &T) -> Result<()> {
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent).await?;
  }
  let tmp = path.with_extension("tmp");
  let bytes = serde_json::to_vec_pretty(value)?;
  fs::write(&tmp, bytes).await?;
  fs::rename(&tmp, &path).await?;
  Ok(())
}

const SPEC_TEMPLATE: &str = r#"# Product specification

## Goal
Describe the product and the outcome the autonomous coding loop must deliver.

## Requirements
- Add concrete, observable product requirements here.

## Constraints
- This file is authoritative and must not be modified by autonomous workers.

## Acceptance
Describe commands and observable behavior that prove the product is complete.
"#;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn state_validation_rejects_done_outside_complete_phase() {
    let mut state = State::fresh();
    state.status = tenet_domain::model::RunStatus::Done;
    state.phase = tenet_domain::model::Phase::Reconciling;

    assert!(validate_state(&state).is_err());
  }

  #[tokio::test]
  async fn ensure_spec_uses_the_configured_nested_path() {
    let project = tempfile::tempdir().expect("temporary project");
    let config = Config {
      spec_file: "requirements/product.md".into(),
      ..Config::default()
    };

    ensure_spec(project.path(), &config)
      .await
      .expect("create configured spec");

    assert!(project.path().join("requirements/product.md").exists());
    assert!(!project.path().join(TENET_DIR).join("spec.md").exists());
  }

  #[tokio::test]
  async fn read_state_accepts_existing_version_one_json() {
    let project = tempfile::tempdir().expect("temporary project");
    let directory = project.path().join(TENET_DIR);
    fs::create_dir_all(&directory)
      .await
      .expect("create state directory");
    let fixture = serde_json::json!({
      "version": 1,
      "status": "running",
      "phase": "reconciling",
      "runId": "run-existing",
      "cycle": 3,
      "activeLeases": {},
      "candidateIntegrations": [],
      "workStatuses": {},
      "requirementCounts": {"total": 1, "satisfied": 0, "partial": 0, "missing": 1},
      "completedWorkUnits": [],
      "discoveries": [],
      "lastSummary": "Reconcile",
      "blockedReason": null,
      "lastError": null,
      "updatedAt": "2026-08-16T10:00:00+00:00"
    });
    fs::write(
      directory.join(STATE_FILE),
      serde_json::to_vec_pretty(&fixture).expect("serialize fixture"),
    )
    .await
    .expect("write fixture");

    let state = read_state(project.path())
      .await
      .expect("read existing state");

    assert_eq!(state.run_id.as_deref(), Some("run-existing"));
    assert_eq!(state.cycle, 3);
  }

  fn evidence_catalog() -> RequirementCatalog {
    use tenet_domain::{
      evidence::{AcceptanceCriterion, VerificationObligation},
      ids::{CriterionId, ObligationId, RequirementId},
      model::Requirement,
      worker::CatalogCoverage,
    };

    RequirementCatalog {
      spec_hash: "spec".into(),
      requirements: vec![Requirement {
        id: RequirementId::from("REQ-001"),
        title: "Requirement".into(),
        description: "Description".into(),
        required: true,
        source_refs: Vec::new(),
      }],
      acceptance_criteria: vec![AcceptanceCriterion {
        id: CriterionId::from("REQ-001/AC-01"),
        requirement_id: RequirementId::from("REQ-001"),
        description: "Observable".into(),
        mandatory: true,
      }],
      verification_obligations: vec![VerificationObligation {
        id: ObligationId::from("REQ-001/AC-01/VO-01"),
        criterion_id: CriterionId::from("REQ-001/AC-01"),
        description: "Required behavior is observable".into(),
        required: true,
      }],
      coverage: CatalogCoverage {
        normative_fragments: Vec::new(),
        uncovered_fragment_ids: Vec::new(),
      },
    }
  }

  fn graph_from_catalog(catalog: &RequirementCatalog) -> Result<EvidenceGraphState> {
    let mut graph = EvidenceGraphState::new(&catalog.spec_hash);
    for requirement in &catalog.requirements {
      graph.register_requirement(requirement.id.clone(), requirement.required);
    }
    for criterion in &catalog.acceptance_criteria {
      graph.add_criterion(criterion.clone())?;
    }
    for obligation in &catalog.verification_obligations {
      graph.add_obligation(obligation.clone())?;
    }
    Ok(graph)
  }

  #[tokio::test]
  async fn evidence_graph_persists_and_reloads_semantics() {
    use chrono::Utc;
    use tenet_domain::{
      evidence::{
        EvidencePolicy, ObligationAssessment, ObligationAssessmentResult, SemanticAssessmentReport,
        VerificationState,
      },
      ids::{ObligationId, RequirementId, VerificationRunId},
      verification::{CommandResult, ProjectCheckResult, ProjectVerificationRun, VerificationSpec},
    };

    let project = tempfile::tempdir().expect("temporary project");
    ensure_layout(project.path()).await.expect("layout");
    let catalog = evidence_catalog();
    let mut graph = graph_from_catalog(&catalog).expect("graph");
    let now = Utc::now();
    let project_run = ProjectVerificationRun {
      run_id: VerificationRunId::new(),
      revision: "abc123".into(),
      suite_hash: "suite".into(),
      checks: vec![ProjectCheckResult {
        name: "quality".into(),
        spec: VerificationSpec {
          program: "true".into(),
          args: Vec::new(),
          working_directory: ".".into(),
          environment: Default::default(),
        },
        timeout_secs: 10,
        result: CommandResult {
          command: "true".into(),
          exit_code: Some(0),
          timed_out: false,
          duration_ms: 1,
          stdout: String::new(),
          stderr: String::new(),
        },
      }],
      passed: true,
      started_at: now,
      finished_at: now,
    };
    graph.record_project_verification(&project_run);
    graph
      .record_semantic_assessment(
        "abc123",
        now,
        "assess",
        &SemanticAssessmentReport {
          summary: "satisfied".into(),
          assessments: vec![ObligationAssessmentResult {
            obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
            assessment: ObligationAssessment::Satisfied {
              rationale: "Observed behavior".into(),
              evidence_refs: Vec::new(),
            },
          }],
        },
      )
      .expect("semantic evidence");
    write_evidence_graph(project.path(), &graph)
      .await
      .expect("write graph");

    let reloaded = read_evidence_graph(project.path(), &graph)
      .await
      .expect("reload graph");

    assert_eq!(
      reloaded.requirement_verification_state(
        &RequirementId::from("REQ-001"),
        EvidencePolicy::new("abc123", "suite")
      ),
      Ok(VerificationState::Verified)
    );
  }

  #[tokio::test]
  async fn evidence_graph_version_zero_migrates_to_current_version() {
    let project = tempfile::tempdir().expect("temporary project");
    ensure_layout(project.path()).await.expect("layout");
    let catalog = evidence_catalog();
    let graph = graph_from_catalog(&catalog).expect("graph");
    let mut value = serde_json::to_value(&graph).expect("serialize graph");
    value["version"] = serde_json::json!(0);
    value
      .as_object_mut()
      .expect("graph object")
      .remove("requirements");
    fs::write(
      project.path().join(TENET_DIR).join(EVIDENCE_GRAPH_FILE),
      serde_json::to_vec_pretty(&value).expect("serialize fixture"),
    )
    .await
    .expect("write fixture");

    let migrated = read_evidence_graph(project.path(), &graph)
      .await
      .expect("migrate graph");

    assert_eq!(migrated.version, EvidenceGraphState::VERSION);
    assert_eq!(migrated.requirements, migrated.required_requirements);
  }

  #[tokio::test]
  async fn evidence_graph_rejects_unknown_future_version() {
    let project = tempfile::tempdir().expect("temporary project");
    ensure_layout(project.path()).await.expect("layout");
    let catalog = evidence_catalog();
    let mut value =
      serde_json::to_value(graph_from_catalog(&catalog).expect("graph")).expect("serialize graph");
    value["version"] = serde_json::json!(99);
    fs::write(
      project.path().join(TENET_DIR).join(EVIDENCE_GRAPH_FILE),
      serde_json::to_vec_pretty(&value).expect("serialize fixture"),
    )
    .await
    .expect("write fixture");

    let error = read_evidence_graph(
      project.path(),
      &graph_from_catalog(&catalog).expect("graph"),
    )
    .await
    .expect_err("future version rejected");

    assert!(error
      .to_string()
      .contains("unsupported evidence graph version"));
  }
}
