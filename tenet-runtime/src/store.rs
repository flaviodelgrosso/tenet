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
  model::{
    CompletedWorkUnit, IntegrationPhase, IntegrationTransaction, ReconcileResult,
    RequirementCatalog, State, VerificationReport,
  },
};

pub const STATE_FILE: &str = "state.json";
pub const REQUIREMENTS_FILE: &str = "requirements.json";
pub const ROADMAP_FILE: &str = "roadmap.json";
pub const INTEGRATION_JOURNAL_FILE: &str = "integration-journal.json";

pub async fn ensure_layout(cwd: &Path) -> Result<()> {
  fs::create_dir_all(cwd.join(TENET_DIR).join("evidence")).await?;
  fs::create_dir_all(cwd.join(TENET_DIR).join("runs")).await?;
  Ok(())
}

pub async fn ensure_spec(cwd: &Path, config: &Config) -> Result<()> {
  let path = cwd.join(&config.spec_file);
  if !path.exists() {
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

pub async fn recover_integration(cwd: &Path, state: &mut State) -> Result<()> {
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
  verify_transaction_evidence(cwd, &transaction).await?;
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

pub fn verification_hash(report: &VerificationReport) -> Result<String> {
  let bytes = serde_json::to_vec(report)?;
  Ok(sha256_hex(&bytes))
}

async fn verify_transaction_evidence(
  cwd: &Path,
  transaction: &IntegrationTransaction,
) -> Result<()> {
  let report: VerificationReport = read_json(cwd.join(&transaction.verification_evidence)).await?;
  let actual = verification_hash(&report)?;
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
}
