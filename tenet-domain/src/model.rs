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
  Scheduling,
  Implementing,
  Verifying,
  Repairing,
  Integrating,
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct ArchitectOutput {
  pub requirements: Vec<Requirement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequirementAssessment {
  pub id: String,
  pub status: RequirementStatus,
  pub evidence: Vec<String>,
  pub gaps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkScope {
  pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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
  #[serde(rename = "dependsOn")]
  pub depends_on: Vec<String>,
  pub scope: WorkScope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReconcileResult {
  pub complete: bool,
  pub summary: String,
  pub requirements: Vec<RequirementAssessment>,
  #[serde(rename = "workUnits")]
  pub work_units: Vec<WorkUnit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Discovery {
  Dependency {
    #[serde(rename = "workUnitId")]
    work_unit_id: String,
    #[serde(rename = "dependsOn")]
    depends_on: String,
    reason: String,
  },
  Blocker {
    description: String,
  },
  ScopeExpansion {
    paths: Vec<String>,
    reason: String,
  },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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
  pub discoveries: Vec<Discovery>,
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
pub struct RepositoryChange {
  pub path: String,
  pub status: char,
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
#[serde(rename_all = "snake_case")]
pub enum WorkStatus {
  Pending,
  Ready,
  Running,
  Candidate,
  Integrating,
  Completed,
  Failed,
  Blocked,
  Invalidated,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkLease {
  pub id: String,
  #[serde(rename = "workerId")]
  pub worker_id: String,
  #[serde(rename = "workUnit")]
  pub work_unit: WorkUnit,
  #[serde(rename = "baseRevision")]
  pub base_revision: String,
  pub workspace: std::path::PathBuf,
  #[serde(rename = "issuedAt")]
  pub issued_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkExecution {
  pub lease: WorkLease,
  #[serde(rename = "workerSummary")]
  pub worker_summary: WorkerSummary,
  pub verification: VerificationReport,
  #[serde(rename = "baseRevision")]
  pub base_revision: String,
  #[serde(rename = "candidateRevision")]
  pub candidate_revision: String,
  #[serde(rename = "changedPaths")]
  pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct State {
  pub version: u32,
  pub status: RunStatus,
  pub phase: Phase,
  #[serde(rename = "runId")]
  pub run_id: Option<String>,
  pub cycle: u32,
  #[serde(rename = "activeLeases")]
  pub active_leases: std::collections::BTreeMap<String, WorkLease>,
  #[serde(rename = "candidateIntegrations")]
  pub candidate_integrations: Vec<WorkExecution>,
  #[serde(rename = "workStatuses")]
  pub work_statuses: std::collections::BTreeMap<String, WorkStatus>,
  #[serde(rename = "requirementCounts")]
  pub requirement_counts: RequirementCounts,
  #[serde(rename = "completedWorkUnits")]
  pub completed_work_units: Vec<CompletedWorkUnit>,
  pub discoveries: Vec<Discovery>,
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
  pub const VERSION: u32 = 2;

  pub fn fresh() -> Self {
    Self {
      version: Self::VERSION,
      status: RunStatus::Idle,
      phase: Phase::Initialized,
      run_id: None,
      cycle: 0,
      active_leases: std::collections::BTreeMap::new(),
      candidate_integrations: Vec::new(),
      work_statuses: std::collections::BTreeMap::new(),
      requirement_counts: RequirementCounts::default(),
      completed_work_units: Vec::new(),
      discoveries: Vec::new(),
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
    #[serde(rename = "workerId")]
    worker_id: String,
    #[serde(rename = "leaseId")]
    lease_id: Option<String>,
    #[serde(rename = "workUnitId")]
    work_unit_id: Option<String>,
    at: String,
  },
  Text {
    role: WorkerRole,
    #[serde(rename = "workerId")]
    worker_id: String,
    #[serde(rename = "leaseId")]
    lease_id: Option<String>,
    #[serde(rename = "workUnitId")]
    work_unit_id: Option<String>,
    at: String,
    delta: String,
  },
  ToolStart {
    role: WorkerRole,
    #[serde(rename = "workerId")]
    worker_id: String,
    #[serde(rename = "leaseId")]
    lease_id: Option<String>,
    #[serde(rename = "workUnitId")]
    work_unit_id: Option<String>,
    at: String,
    tool_name: String,
    args: serde_json::Value,
  },
  ToolEnd {
    role: WorkerRole,
    #[serde(rename = "workerId")]
    worker_id: String,
    #[serde(rename = "leaseId")]
    lease_id: Option<String>,
    #[serde(rename = "workUnitId")]
    work_unit_id: Option<String>,
    at: String,
    tool_name: String,
    is_error: bool,
    output: Option<String>,
  },
  End {
    role: WorkerRole,
    #[serde(rename = "workerId")]
    worker_id: String,
    #[serde(rename = "leaseId")]
    lease_id: Option<String>,
    #[serde(rename = "workUnitId")]
    work_unit_id: Option<String>,
    at: String,
    ok: bool,
    message: Option<String>,
  },
}
