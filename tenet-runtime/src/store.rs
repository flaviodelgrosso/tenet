#[cfg(unix)]
use std::os::fd::{FromRawFd, RawFd};
use std::{
  fmt::Write as _,
  fs::OpenOptions,
  io::{Read, Write},
  path::{Path, PathBuf},
  process::Command,
};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};
use tokio::fs;

use tenet_domain::{
  config::{read_config, Config, TENET_DIR},
  evidence::EvidenceGraphState,
  model::{
    CatalogApproval, CompletedWorkUnit, IntegrationPhase, IntegrationTransaction, ReconcileResult,
    RequirementCatalog, State,
  },
  trusted_verifier::{TrustedExecutionRecord, TrustedVerificationSpec},
  verification::ProjectVerificationRun,
};
use tenet_storage::Storage;
const AUTHORITY_NAMESPACE_ENV: &str = "TENET_CONTROLLER_AUTHORITY_NAMESPACE";
const AUTHORITY_KEY_FD_ENV: &str = "TENET_CONTROLLER_AUTHORITY_KEY_FD";
const MAX_AUTHORITY_KEY_BYTES: u64 = 1024 * 1024;

struct ControllerAuthorityIdentity {
  namespace: String,
  key_material: Vec<u8>,
}

impl ControllerAuthorityIdentity {
  fn from_environment() -> Result<Self> {
    let namespace = std::env::var(AUTHORITY_NAMESPACE_ENV)
      .with_context(|| format!("{AUTHORITY_NAMESPACE_ENV} is required for trusted authority"))?;
    std::env::remove_var(AUTHORITY_NAMESPACE_ENV);
    if namespace.trim().is_empty() {
      anyhow::bail!("{AUTHORITY_NAMESPACE_ENV} must not be blank");
    }
    let descriptor = std::env::var(AUTHORITY_KEY_FD_ENV)
      .with_context(|| format!("{AUTHORITY_KEY_FD_ENV} is required for trusted authority"))?;
    std::env::remove_var(AUTHORITY_KEY_FD_ENV);
    let descriptor = descriptor.parse::<i64>().with_context(|| {
      format!("{AUTHORITY_KEY_FD_ENV} must contain an inherited file descriptor")
    })?;
    let key_material = read_authority_key(descriptor)?;
    if key_material.is_empty() {
      anyhow::bail!("controller authority key must not be empty");
    }
    Ok(Self {
      namespace,
      key_material,
    })
  }

  fn install(mut self) -> Result<()> {
    let result = install_controller_authority_key(&self.namespace, &self.key_material);
    self.key_material.fill(0);
    result
  }
}

#[cfg(unix)]
fn read_authority_key(descriptor: i64) -> Result<Vec<u8>> {
  let descriptor = RawFd::try_from(descriptor)
    .context("controller authority key file descriptor is outside the supported range")?;
  if descriptor < 0 {
    anyhow::bail!("controller authority key file descriptor must not be negative");
  }
  // SAFETY: ownership of this inherited descriptor is transferred by the launcher contract.
  let mut file = unsafe { std::fs::File::from_raw_fd(descriptor) };
  let mut key = Vec::new();
  Read::by_ref(&mut file)
    .take(MAX_AUTHORITY_KEY_BYTES + 1)
    .read_to_end(&mut key)
    .context("read controller authority key file descriptor")?;
  if key.len() as u64 > MAX_AUTHORITY_KEY_BYTES {
    key.fill(0);
    anyhow::bail!("controller authority key exceeds {MAX_AUTHORITY_KEY_BYTES} bytes");
  }
  Ok(key)
}

#[cfg(not(unix))]
fn read_authority_key(_descriptor: i64) -> Result<Vec<u8>> {
  anyhow::bail!("inherited controller authority key descriptors require a Unix host")
}

pub fn bootstrap_controller_authority_identity() -> Result<()> {
  ControllerAuthorityIdentity::from_environment()?.install()
}

pub async fn trusted_authority_identity_required(cwd: &Path) -> Result<bool> {
  Ok(Storage::open(cwd).await?.has_trusted_authority().await?)
}

pub fn install_controller_authority_key(
  authority_namespace: &str,
  key_material: &[u8],
) -> Result<()> {
  tenet_storage::install_controller_authority_key(authority_namespace, key_material)
    .context("install controller authority identity")
}

pub async fn ensure_layout(cwd: &Path) -> Result<()> {
  fs::create_dir_all(cwd.join(TENET_DIR).join("runs")).await?;
  Storage::open(cwd).await?;
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
    Ok(value) => value,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
    Err(error) => return Err(error.into()),
  };
  if !text.lines().any(|value| value.trim() == ".tenet/") {
    if !text.is_empty() && !text.ends_with('\n') {
      text.push('\n');
    }
    text.push_str(".tenet/\n");
    fs::write(path, text).await?;
  }
  Ok(())
}

pub async fn read_state(cwd: &Path) -> Result<State> {
  Ok(Storage::open(cwd).await?.load_current_state().await?)
}

pub async fn write_state(cwd: &Path, state: &State) -> Result<()> {
  Ok(Storage::open(cwd).await?.persist_state(state).await?)
}

pub async fn read_catalog(cwd: &Path) -> Result<Option<RequirementCatalog>> {
  Ok(Storage::open(cwd).await?.load_active_catalog().await?)
}

pub async fn catalog_is_approved(cwd: &Path, catalog: &RequirementCatalog) -> Result<bool> {
  Ok(
    Storage::open(cwd)
      .await?
      .catalog_is_approved(catalog)
      .await?,
  )
}

pub async fn write_catalog_approval(cwd: &Path, approval: &CatalogApproval) -> Result<()> {
  Ok(
    Storage::open(cwd)
      .await?
      .persist_catalog_approval(approval)
      .await?,
  )
}

pub async fn write_catalog(cwd: &Path, catalog: &RequirementCatalog) -> Result<()> {
  let source_path = read_config(cwd)
    .await
    .map(|config| config.spec_file)
    .unwrap_or_else(|_| "spec.md".into());
  Ok(
    Storage::open(cwd)
      .await?
      .persist_catalog(&source_path, Utc::now(), catalog)
      .await?,
  )
}

pub async fn write_roadmap(
  cwd: &Path,
  run_id: &str,
  cycle: u32,
  repository_revision: &str,
  catalog_hash: &str,
  value: &ReconcileResult,
) -> Result<()> {
  Storage::open(cwd)
    .await?
    .persist_reconcile_round(run_id, cycle, repository_revision, catalog_hash, value)
    .await
    .context("persist reconciliation round")?;
  Ok(())
}

pub async fn read_evidence_graph(
  cwd: &Path,
  expected: &EvidenceGraphState,
) -> Result<EvidenceGraphState> {
  let storage = Storage::open(cwd).await?;
  let catalog = storage
    .load_active_catalog()
    .await?
    .ok_or_else(|| anyhow!("cannot load evidence without an active catalog"))?;
  let config = read_config(cwd).await?;
  let graph = storage
    .load_evidence_graph(&catalog, &config.verification.trusted_checks)
    .await?;
  if graph.specification_hash != expected.specification_hash
    || graph.requirements != expected.requirements
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
  let storage = Storage::open(cwd).await?;
  let state = storage.load_current_state().await?;
  let run_id = state
    .run_id
    .as_deref()
    .ok_or_else(|| anyhow!("cannot persist evidence without an active run"))?;
  storage
    .persist_evidence_graph(run_id, graph)
    .await
    .context("persist evidence graph")?;
  Ok(())
}

pub async fn has_unfinished_integration(cwd: &Path) -> Result<bool> {
  Ok(
    Storage::open(cwd)
      .await?
      .has_unfinished_integration()
      .await?,
  )
}

pub async fn read_integration_journal(cwd: &Path) -> Result<Option<IntegrationTransaction>> {
  let storage = Storage::open(cwd).await?;
  let state = storage.load_current_state().await?;
  match state.run_id {
    Some(run_id) => Ok(storage.load_active_integration(&run_id).await?),
    None => Ok(None),
  }
}

pub async fn record_manual_verification(
  cwd: &Path,
  run_id: &str,
  report: &ProjectVerificationRun,
) -> Result<()> {
  let storage = Storage::open(cwd).await?;
  storage
    .create_detached_run(run_id)
    .await
    .context("create detached verification run")?;
  storage
    .record_project_verification(run_id, report)
    .await
    .context("record project verification")?;
  Ok(())
}
pub async fn record_trusted_execution(
  cwd: &Path,
  record: &TrustedExecutionRecord,
  spec: &TrustedVerificationSpec,
) -> Result<()> {
  let storage = Storage::open(cwd).await?;
  let state = storage.load_current_state().await?;
  let run_id = state
    .run_id
    .as_deref()
    .ok_or_else(|| anyhow!("cannot persist trusted execution without an active run"))?;
  storage
    .record_trusted_execution(run_id, record, spec)
    .await
    .context("record controller-owned trusted execution")?;
  Ok(())
}

pub async fn write_integration_journal(
  cwd: &Path,
  transaction: &IntegrationTransaction,
) -> Result<()> {
  let storage = Storage::open(cwd).await?;
  match transaction.phase {
    IntegrationPhase::Prepared => storage
      .prepare_integration(transaction)
      .await
      .context("prepare integration transaction")?,
    IntegrationPhase::GitCommitted => {
      storage
        .mark_integration_git_committed(transaction)
        .await
        .context("mark integration Git committed")?;
    }
    IntegrationPhase::StateCommitted => {
      storage
        .complete_integration(transaction, &transaction.updated_at)
        .await
        .context("complete integration transaction")?;
    }
  }
  Ok(())
}

pub async fn remove_integration_journal(cwd: &Path) -> Result<()> {
  let storage = Storage::open(cwd).await?;
  let unfinished_count = storage.unfinished_integration_count().await?;
  if unfinished_count > 1 {
    return Err(anyhow!(
      "cannot remove integration state: {unfinished_count} unfinished transactions are ambiguous"
    ));
  }
  let state = storage.load_current_state().await?;
  let Some(run_id) = state.run_id else {
    if storage.has_unfinished_integration().await? {
      return Err(anyhow!(
        "cannot remove unfinished integration without a current run"
      ));
    }
    return Ok(());
  };
  let Some(transaction) = storage.load_active_integration(&run_id).await? else {
    if storage.has_unfinished_integration().await? {
      return Err(anyhow!(
        "cannot remove unfinished integration owned by a non-current run"
      ));
    }
    return Ok(());
  };
  if transaction.phase != IntegrationPhase::Prepared {
    return Err(anyhow!(
      "cannot abandon integration {} after canonical Git mutation",
      transaction.id
    ));
  }
  storage
    .abandon_prepared_integration(&transaction.id)
    .await?;
  Ok(())
}

pub async fn recover_integration(cwd: &Path, state: &mut State, config: &Config) -> Result<()> {
  let storage = Storage::open(cwd).await?;
  let unfinished_count = storage.unfinished_integration_count().await?;
  if unfinished_count > 1 {
    return Err(anyhow!("integration recovery failed closed: {unfinished_count} unfinished transactions are ambiguous"));
  }
  *state = storage.load_current_state().await?;
  let Some(run_id) = state.run_id.as_deref() else {
    if storage.has_unfinished_integration().await? {
      return Err(anyhow!(
        "integration recovery found an unfinished transaction without a current run"
      ));
    }
    return Ok(());
  };
  let Some(mut transaction) = storage.load_active_integration(run_id).await? else {
    if storage.has_unfinished_integration().await? {
      return Err(anyhow!(
        "integration recovery found an unfinished transaction owned by a non-current run"
      ));
    }
    return Ok(());
  };
  let head = crate::git::head(cwd).await?;
  if head == transaction.old_head && transaction.phase == IntegrationPhase::Prepared {
    storage
      .abandon_prepared_integration(&transaction.id)
      .await?;
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
  let report = storage
    .load_project_verification(transaction.verification_run_id)
    .await?
    .ok_or_else(|| {
      anyhow!(
        "integration recovery is missing project verification {}",
        transaction.verification_run_id
      )
    })?;
  verify_transaction_evidence(&transaction, &report, &config.verification.suite_hash()?)?;
  if transaction.phase == IntegrationPhase::Prepared {
    transaction.phase = IntegrationPhase::GitCommitted;
    transaction.updated_at = Utc::now().to_rfc3339();
    storage.mark_integration_git_committed(&transaction).await?;
  }
  if !state.completed_work_units.iter().any(|completed| {
    completed.work_unit == transaction.work_unit
      && completed.verification_run_id == transaction.verification_run_id
  }) {
    state.completed_work_units.push(CompletedWorkUnit {
      work_unit: transaction.work_unit.clone(),
      completed_at: transaction.updated_at.clone(),
      verification_run_id: transaction.verification_run_id,
    });
  }
  state.candidate_integrations.clear();
  storage.persist_state(state).await?;
  storage
    .complete_integration(&transaction, &transaction.updated_at)
    .await?;
  Ok(())
}

pub fn project_verification_hash(report: &ProjectVerificationRun) -> Result<String> {
  let bytes = serde_json::to_vec(report)?;
  Ok(sha256_hex(&bytes))
}

fn verify_transaction_evidence(
  transaction: &IntegrationTransaction,
  report: &ProjectVerificationRun,
  expected_suite_hash: &str,
) -> Result<()> {
  if !report.passed {
    return Err(anyhow!(
      "integration recovery requires passing project verification evidence"
    ));
  }
  if report.revision != transaction.new_head {
    return Err(anyhow!(
      "integration recovery evidence revision {} does not match intended revision {}",
      report.revision,
      transaction.new_head
    ));
  }
  if report.suite_hash != expected_suite_hash {
    return Err(anyhow!(
      "integration project verification evidence uses stale suite {}; current suite is {}",
      report.suite_hash,
      expected_suite_hash
    ));
  }
  let actual = project_verification_hash(report)?;
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
          if let Some(pid) = value.get("pid").and_then(serde_json::Value::as_u64) {
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
    let payload = serde_json::json!({"pid":std::process::id(),"startedAt":Utc::now().to_rfc3339()});
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
    .map(|status| status.success())
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
  use tenet_domain::{
    ids::{CriterionId, ObligationId, RequirementId, VerificationRunId},
    model::{WorkScope, WorkUnit},
  };

  fn recovery_evidence_fixture() -> (IntegrationTransaction, ProjectVerificationRun) {
    let verification_run_id = VerificationRunId::new();
    let report = ProjectVerificationRun {
      run_id: verification_run_id,
      revision: "wrong".into(),
      suite_hash: "suite".into(),
      checks: Vec::new(),
      passed: true,
      started_at: Utc::now(),
      finished_at: Utc::now(),
    };
    let transaction = IntegrationTransaction {
      version: IntegrationTransaction::VERSION,
      id: "integration".into(),
      run_id: "run".into(),
      work_unit: WorkUnit {
        id: "WU-001".into(),
        title: "Work".into(),
        objective: "Work".into(),
        requirement_ids: vec![RequirementId::from("REQ-001")],
        criterion_ids: vec![CriterionId::from("REQ-001/AC-01")],
        verification_obligation_ids: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
        suggested_checks: Vec::new(),
        depends_on: Vec::new(),
        scope: WorkScope {
          paths: vec!["src/**".into()],
        },
      },
      candidate_revision: "new".into(),
      old_head: "old".into(),
      new_head: "new".into(),
      phase: IntegrationPhase::GitCommitted,
      verification_run_id,
      verification_hash: project_verification_hash(&report).expect("verification hash"),
      created_at: "created".into(),
      updated_at: "updated".into(),
    };
    (transaction, report)
  }

  #[test]
  fn recovery_rejects_wrong_revision_and_failed_evidence() {
    let (transaction, mut report) = recovery_evidence_fixture();
    assert!(verify_transaction_evidence(&transaction, &report, "suite")
      .expect_err("wrong revision")
      .to_string()
      .contains("does not match intended revision"));

    report.revision = transaction.new_head.clone();
    report.passed = false;
    assert!(verify_transaction_evidence(&transaction, &report, "suite")
      .expect_err("failed evidence")
      .to_string()
      .contains("requires passing"));
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
  }
}
