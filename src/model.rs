use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
  Idle,
  Running,
  Done,
  Blocked,
  Failed,
  Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
  Initialized,
  Architecting,
  Reconciling,
  Implementing,
  Verifying,
  Repairing,
  Assessing,
  Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequirementStatus {
  Satisfied,
  Partial,
  Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Requirement {
  pub id: String,
  pub title: String,
  pub description: String,
  #[serde(rename = "acceptanceCriteria")]
  pub acceptance_criteria: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequirementCatalog {
  #[serde(rename = "specHash")]
  pub spec_hash: String,
  pub requirements: Vec<Requirement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchitectOutput {
  pub requirements: Vec<Requirement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequirementAssessment {
  pub id: String,
  pub status: RequirementStatus,
  pub evidence: Vec<String>,
  pub gaps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkUnit {
  pub id: String,
  pub title: String,
  pub objective: String,
  #[serde(rename = "requirementIds")]
  pub requirement_ids: Vec<String>,
  #[serde(rename = "acceptanceCriteria")]
  pub acceptance_criteria: Vec<String>,
  #[serde(rename = "suggestedChecks")]
  pub suggested_checks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconcileResult {
  pub complete: bool,
  pub summary: String,
  pub requirements: Vec<RequirementAssessment>,
  #[serde(rename = "nextWorkUnit")]
  pub next_work_unit: Option<WorkUnit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerSummary {
  pub summary: String,
  #[serde(rename = "changedFiles")]
  pub changed_files: Vec<String>,
  #[serde(rename = "testsRun")]
  pub tests_run: Vec<String>,
  pub notes: Vec<String>,
  #[serde(default)]
  pub decisions: Vec<String>,
  #[serde(default)]
  pub discoveries: Vec<String>,
  #[serde(default)]
  pub risks: Vec<String>,
  #[serde(default, rename = "followUps")]
  pub follow_ups: Vec<String>,
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
pub struct VerificationReport {
  pub passed: bool,
  #[serde(rename = "startedAt")]
  pub started_at: String,
  #[serde(rename = "finishedAt")]
  pub finished_at: String,
  pub commands: Vec<CommandResult>,
  pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RequirementCounts {
  pub total: usize,
  pub satisfied: usize,
  pub partial: usize,
  pub missing: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompletedWorkUnit {
  #[serde(rename = "workUnit")]
  pub work_unit: WorkUnit,
  #[serde(rename = "completedAt")]
  pub completed_at: String,
  #[serde(rename = "verificationEvidence")]
  pub verification_evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
  pub version: u32,
  pub status: RunStatus,
  pub phase: Phase,
  #[serde(rename = "runId")]
  pub run_id: Option<String>,
  pub cycle: u32,
  #[serde(rename = "currentWorkUnit")]
  pub current_work_unit: Option<WorkUnit>,
  #[serde(rename = "requirementCounts")]
  pub requirement_counts: RequirementCounts,
  #[serde(rename = "completedWorkUnits")]
  pub completed_work_units: Vec<CompletedWorkUnit>,
  #[serde(rename = "lastSummary")]
  pub last_summary: String,
  #[serde(rename = "blockedReason")]
  pub blocked_reason: Option<String>,
  #[serde(rename = "lastError")]
  pub last_error: Option<String>,
  #[serde(rename = "updatedAt")]
  pub updated_at: String,
}

impl State {
  pub fn fresh() -> Self {
    Self {
      version: 1,
      status: RunStatus::Idle,
      phase: Phase::Initialized,
      run_id: None,
      cycle: 0,
      current_work_unit: None,
      requirement_counts: RequirementCounts::default(),
      completed_work_units: Vec::new(),
      last_summary: "Initialized".into(),
      blocked_reason: None,
      last_error: None,
      updated_at: chrono::Utc::now().to_rfc3339(),
    }
  }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRole {
  Architect,
  Reconcile,
  Implement,
  Repair,
  Assess,
}

impl WorkerRole {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Architect => "architect",
      Self::Reconcile => "reconcile",
      Self::Implement => "implement",
      Self::Repair => "repair",
      Self::Assess => "assess",
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerEvent {
  Start {
    role: WorkerRole,
    at: String,
    skills: Vec<String>,
  },
  Text {
    role: WorkerRole,
    at: String,
    delta: String,
  },
  ToolStart {
    role: WorkerRole,
    at: String,
    tool_name: String,
    args: serde_json::Value,
  },
  ToolEnd {
    role: WorkerRole,
    at: String,
    tool_name: String,
    is_error: bool,
    output: Option<String>,
  },
  End {
    role: WorkerRole,
    at: String,
    ok: bool,
    message: Option<String>,
  },
}
