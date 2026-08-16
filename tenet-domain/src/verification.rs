use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
      warnings: Vec::new(),
    };

    let value = serde_json::to_value(&report).expect("serialize report");
    let decoded: VerificationReport =
      serde_json::from_value(value.clone()).expect("deserialize report");

    assert_eq!(decoded, report);
    assert_eq!(value["startedAt"], "2026-08-16T10:00:00+00:00");
  }
}
