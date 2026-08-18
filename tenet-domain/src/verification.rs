use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{ObligationId, VerificationRunId};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct VerificationSpec {
  pub program: String,
  #[serde(default)]
  pub args: Vec<String>,
  #[serde(rename = "workingDirectory", default = "default_working_directory")]
  pub working_directory: String,
  #[serde(default)]
  pub environment: BTreeMap<String, String>,
}

impl VerificationSpec {
  pub fn identity(&self) -> String {
    serde_json::to_string(self).unwrap_or_default()
  }
}

fn default_working_directory() -> String {
  ".".into()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationAuthority {
  ProjectConfigured,
  ControllerDerived,
  AgentProposed,
}

impl VerificationAuthority {
  pub fn is_trusted(self) -> bool {
    matches!(self, Self::ProjectConfigured | Self::ControllerDerived)
  }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyScopeAuthority {
  ProjectConfigured,
  ControllerDerived,
  AgentProposed,
  Unknown,
}

impl DependencyScopeAuthority {
  pub fn is_trusted(self) -> bool {
    matches!(self, Self::ProjectConfigured | Self::ControllerDerived)
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationExecutionRequest {
  #[serde(rename = "verificationRunId")]
  pub run_id: VerificationRunId,
  #[serde(rename = "obligationId")]
  pub obligation_id: ObligationId,
  pub spec: VerificationSpec,
  pub authority: VerificationAuthority,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationExecutionResult {
  #[serde(rename = "verificationRunId")]
  pub run_id: VerificationRunId,
  #[serde(rename = "obligationId")]
  pub obligation_id: ObligationId,
  pub spec: VerificationSpec,
  pub authority: VerificationAuthority,
  pub result: CommandResult,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectCheckResult {
  pub name: String,
  pub spec: VerificationSpec,
  #[serde(rename = "timeoutSecs")]
  pub timeout_secs: u64,
  pub result: CommandResult,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectVerificationRun {
  #[serde(rename = "verificationRunId")]
  pub run_id: VerificationRunId,
  pub revision: String,
  #[serde(rename = "suiteHash")]
  pub suite_hash: String,
  pub checks: Vec<ProjectCheckResult>,
  pub passed: bool,
  #[serde(rename = "startedAt", with = "rfc3339")]
  pub started_at: DateTime<Utc>,
  #[serde(rename = "finishedAt", with = "rfc3339")]
  pub finished_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandResult {
  pub command: String,
  #[serde(rename = "exitCode")]
  pub exit_code: Option<i32>,
  pub timed_out: bool,
  #[serde(rename = "durationMs")]
  pub duration_ms: u128,
  pub stdout: String,
  pub stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryChange {
  pub path: String,
  pub status: char,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationReport {
  pub passed: bool,
  #[serde(rename = "startedAt", with = "rfc3339")]
  pub started_at: DateTime<Utc>,
  #[serde(rename = "finishedAt", with = "rfc3339")]
  pub finished_at: DateTime<Utc>,
  pub commands: Vec<CommandResult>,
  #[serde(default)]
  pub executions: Vec<VerificationExecutionResult>,
  pub warnings: Vec<String>,
}

mod rfc3339 {
  use chrono::{DateTime, Utc};
  use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

  pub fn serialize<S>(value: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    serializer.serialize_str(&value.to_rfc3339())
  }

  pub fn deserialize<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
  where
    D: Deserializer<'de>,
  {
    let value = String::deserialize(deserializer)?;
    DateTime::parse_from_rfc3339(&value)
      .map(|timestamp| timestamp.with_timezone(&Utc))
      .map_err(D::Error::custom)
  }
}

#[cfg(test)]
mod tests {
  use chrono::TimeZone;

  use super::*;

  #[test]
  fn verification_report_round_trip_preserves_rfc3339_wire_format() {
    let report = VerificationReport {
      passed: true,
      started_at: Utc.with_ymd_and_hms(2026, 8, 16, 10, 0, 0).unwrap(),
      finished_at: Utc.with_ymd_and_hms(2026, 8, 16, 10, 0, 1).unwrap(),
      commands: Vec::new(),
      executions: Vec::new(),
      warnings: Vec::new(),
    };

    let value = serde_json::to_value(&report).expect("serialize report");
    let decoded: VerificationReport =
      serde_json::from_value(value.clone()).expect("deserialize report");

    assert_eq!(decoded, report);
    assert_eq!(value["startedAt"], "2026-08-16T10:00:00+00:00");
  }

  #[test]
  fn project_verification_run_round_trip_preserves_diagnostics() {
    let spec = VerificationSpec {
      program: "./verify".into(),
      args: vec!["--all".into()],
      working_directory: ".".into(),
      environment: [("CI".into(), "true".into())].into_iter().collect(),
    };
    let run = ProjectVerificationRun {
      run_id: VerificationRunId::new(),
      revision: "abc123".into(),
      suite_hash: "suite-hash".into(),
      checks: vec![ProjectCheckResult {
        name: "quality".into(),
        spec: spec.clone(),
        timeout_secs: 600,
        result: CommandResult {
          command: spec.identity(),
          exit_code: Some(0),
          timed_out: false,
          duration_ms: 12,
          stdout: "ok".into(),
          stderr: String::new(),
        },
      }],
      passed: true,
      started_at: Utc.with_ymd_and_hms(2026, 8, 18, 10, 0, 0).unwrap(),
      finished_at: Utc.with_ymd_and_hms(2026, 8, 18, 10, 0, 1).unwrap(),
    };

    let encoded = serde_json::to_vec(&run).expect("serialize project run");
    let decoded: ProjectVerificationRun =
      serde_json::from_slice(&encoded).expect("deserialize project run");

    assert_eq!(decoded, run);
  }
}
