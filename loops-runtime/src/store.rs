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

use loops_domain::{
  config::{Config, LOOPS_DIR},
  model::{ReconcileResult, RequirementCatalog, State},
};

pub const STATE_FILE: &str = "state.json";
pub const REQUIREMENTS_FILE: &str = "requirements.json";
pub const ROADMAP_FILE: &str = "roadmap.json";

pub async fn ensure_layout(cwd: &Path) -> Result<()> {
  fs::create_dir_all(cwd.join(LOOPS_DIR).join("evidence")).await?;
  fs::create_dir_all(cwd.join(LOOPS_DIR).join("runs")).await?;
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
  if !text.lines().any(|v| v.trim() == ".loops/") {
    if !text.is_empty() && !text.ends_with('\n') {
      text.push('\n');
    }
    text.push_str(".loops/\n");
    fs::write(path, text).await?;
  }
  Ok(())
}

pub async fn read_state(cwd: &Path) -> Result<State> {
  let path = cwd.join(LOOPS_DIR).join(STATE_FILE);
  if !path.exists() {
    return Ok(State::fresh());
  }
  let mut value: serde_json::Value = read_json(path).await?;
  let version = value
    .get("version")
    .and_then(serde_json::Value::as_u64)
    .ok_or_else(|| anyhow!("state file has no numeric version"))?;
  match version {
    1 => migrate_v1(&mut value),
    current if current == u64::from(State::VERSION) => {
      serde_json::from_value(value).context("deserialize current state")
    }
    _ => Err(anyhow!(
      "unsupported state version {version}; expected {}",
      State::VERSION
    )),
  }
}

pub async fn write_state(cwd: &Path, state: &State) -> Result<()> {
  write_json_atomic(cwd.join(LOOPS_DIR).join(STATE_FILE), state).await
}

pub async fn read_catalog(cwd: &Path) -> Result<Option<RequirementCatalog>> {
  let path = cwd.join(LOOPS_DIR).join(REQUIREMENTS_FILE);
  if !path.exists() {
    return Ok(None);
  }
  Ok(Some(read_json(path).await?))
}

pub async fn write_catalog(cwd: &Path, catalog: &RequirementCatalog) -> Result<()> {
  write_json_atomic(cwd.join(LOOPS_DIR).join(REQUIREMENTS_FILE), catalog).await
}

pub async fn write_roadmap(cwd: &Path, value: &ReconcileResult) -> Result<()> {
  write_json_atomic(cwd.join(LOOPS_DIR).join(ROADMAP_FILE), value).await
}

pub async fn save_evidence<T: serde::Serialize>(
  cwd: &Path,
  name: &str,
  value: &T,
) -> Result<PathBuf> {
  let path = cwd
    .join(LOOPS_DIR)
    .join("evidence")
    .join(format!("{name}.json"));
  write_json_atomic(path.clone(), value).await?;
  Ok(path)
}

pub async fn spec_text_and_hash(cwd: &Path, config: &Config) -> Result<(String, String)> {
  let path = cwd.join(&config.spec_file);
  let text = fs::read_to_string(&path)
    .await
    .with_context(|| format!("read authoritative spec {}", path.display()))?;
  let mut hasher = Sha256::new();
  hasher.update(text.as_bytes());
  let digest = hasher.finalize();
  let mut hash = String::with_capacity(digest.len() * 2);
  for byte in digest {
    write!(hash, "{byte:02x}")?;
  }
  Ok((text, hash))
}

pub struct RunLock {
  path: PathBuf,
}

impl RunLock {
  pub fn acquire(cwd: &Path) -> Result<Self> {
    let path = cwd.join(LOOPS_DIR).join("run.lock");
    if path.exists() {
      if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
          if let Some(pid) = value.get("pid").and_then(|v| v.as_u64()) {
            if process_alive(pid as u32) {
              return Err(anyhow!("another loops run is active (pid {pid})"));
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

fn migrate_v1(value: &mut serde_json::Value) -> Result<State> {
  let object = value
    .as_object_mut()
    .ok_or_else(|| anyhow!("state version 1 must be a JSON object"))?;
  object.insert("version".into(), State::VERSION.into());
  object.remove("currentWorkUnit");
  object.insert("activeLeases".into(), serde_json::json!({}));
  object.insert("candidateIntegrations".into(), serde_json::json!([]));
  object.insert("workStatuses".into(), serde_json::json!({}));
  object.insert("discoveries".into(), serde_json::json!([]));
  if let Some(completed) = object
    .get_mut("completedWorkUnits")
    .and_then(serde_json::Value::as_array_mut)
  {
    for item in completed {
      if let Some(unit) = item
        .get_mut("workUnit")
        .and_then(serde_json::Value::as_object_mut)
      {
        unit.insert("dependsOn".into(), serde_json::json!([]));
        unit.insert("scope".into(), serde_json::json!({"paths":["**"]}));
      }
    }
  }
  serde_json::from_value(value.clone()).context("migrate state version 1 to version 2")
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
  fn state_v1_migration_adds_coordination_fields_explicitly() {
    let mut value = serde_json::json!({
      "version": 1,
      "status": "idle",
      "phase": "initialized",
      "runId": null,
      "cycle": 0,
      "currentWorkUnit": null,
      "requirementCounts": {"total":0,"satisfied":0,"partial":0,"missing":0},
      "completedWorkUnits": [],
      "lastSummary": "Initialized",
      "blockedReason": null,
      "lastError": null,
      "updatedAt": "now"
    });

    let state = migrate_v1(&mut value).expect("version 1 migrates");

    assert_eq!(state.version, State::VERSION);
    assert!(state.active_leases.is_empty());
    assert!(state.candidate_integrations.is_empty());
  }
}
