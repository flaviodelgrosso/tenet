pub use crate::verification::{CommandResult, RepositoryChange, VerificationReport};
pub use crate::worker::{
  AgentReconciliationProposal, AgentRequirementAssessment, AgentWorkUnit, ArchitectOutput,
  ArchitectRequirement, CandidateCheck, Discovery, DiscoveryRecord, DiscoveryStatus,
  ReconcileResult, Requirement, RequirementAssessment, RequirementCatalog, WorkScope, WorkUnit,
  WorkerDiscovery, WorkerEvent, WorkerRole, WorkerSummary,
};
use serde::{Deserialize, Serialize};

use crate::ids::VerificationRunId;

/// Exact controller-state payload supplied to Implement and Repair workers.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationWorkerContext<'a> {
  pub work_unit: &'a WorkUnit,
  pub catalog: &'a RequirementCatalog,
  pub discoveries: &'a [Discovery],
  #[serde(skip_serializing_if = "Option::is_none")]
  pub previous_verification: Option<&'a VerificationReport>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct RequirementCounts {
  pub total: usize,
  #[serde(alias = "satisfied")]
  pub verified: usize,
  #[serde(rename = "partiallyVerified", alias = "partial")]
  pub partially_verified: usize,
  #[serde(alias = "missing")]
  pub unverified: usize,
  pub uncertain: usize,
  pub stale: usize,
  pub contradicted: usize,
  #[serde(rename = "missingImplementation")]
  pub missing_implementation: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct VerificationLayers {
  #[serde(rename = "projectChecksTotal")]
  pub project_checks_total: usize,
  #[serde(rename = "projectChecksPassed")]
  pub project_checks_passed: usize,
  #[serde(rename = "projectPassed")]
  pub project_passed: bool,
  #[serde(rename = "semanticObligationsTotal")]
  pub semantic_obligations_total: usize,
  #[serde(rename = "semanticSatisfied")]
  pub semantic_satisfied: usize,
  #[serde(rename = "semanticGaps")]
  pub semantic_gaps: usize,
  #[serde(rename = "semanticUncertain")]
  pub semantic_uncertain: usize,
  pub contradictions: usize,
  #[serde(rename = "completionEligible")]
  pub completion_eligible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompletedWorkUnit {
  #[serde(rename = "workUnit")]
  pub work_unit: WorkUnit,
  #[serde(rename = "completedAt")]
  pub completed_at: String,
  #[serde(rename = "verificationRunId")]
  pub verification_run_id: VerificationRunId,
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
  #[serde(default)]
  pub discoveries: Vec<WorkerDiscovery>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeferredCandidate {
  pub lease: WorkLease,
  #[serde(rename = "workerSummary")]
  pub worker_summary: WorkerSummary,
  #[serde(rename = "baseRevision")]
  pub base_revision: String,
  #[serde(rename = "candidateRevision")]
  pub candidate_revision: String,
  #[serde(rename = "changedPaths")]
  pub changed_paths: Vec<String>,
  pub discoveries: Vec<WorkerDiscovery>,
  #[serde(rename = "catalogHash")]
  pub catalog_hash: String,
  #[serde(rename = "gitRef")]
  pub git_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationPhase {
  Prepared,
  GitCommitted,
  StateCommitted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IntegrationTransaction {
  pub version: u32,
  pub id: String,
  #[serde(rename = "runId")]
  pub run_id: String,
  #[serde(rename = "workUnit")]
  pub work_unit: WorkUnit,
  #[serde(rename = "candidateRevision")]
  pub candidate_revision: String,
  #[serde(rename = "oldHead")]
  pub old_head: String,
  #[serde(rename = "newHead")]
  pub new_head: String,
  pub phase: IntegrationPhase,
  #[serde(rename = "verificationRunId")]
  pub verification_run_id: VerificationRunId,
  #[serde(rename = "verificationHash")]
  pub verification_hash: String,
  #[serde(rename = "createdAt")]
  pub created_at: String,
  #[serde(rename = "updatedAt")]
  pub updated_at: String,
}

impl IntegrationTransaction {
  pub const VERSION: u32 = 1;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepairProgress {
  #[serde(rename = "workUnitId")]
  pub work_unit_id: String,
  pub attempt: u32,
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
  #[serde(default, rename = "deferredCandidates")]
  pub deferred_candidates: Vec<DeferredCandidate>,
  #[serde(rename = "workStatuses")]
  pub work_statuses: std::collections::BTreeMap<String, WorkStatus>,
  #[serde(rename = "requirementCounts")]
  pub requirement_counts: RequirementCounts,
  #[serde(default, rename = "verificationLayers")]
  pub verification_layers: VerificationLayers,
  #[serde(rename = "completedWorkUnits")]
  pub completed_work_units: Vec<CompletedWorkUnit>,
  pub discoveries: Vec<DiscoveryRecord>,
  #[serde(default, rename = "stagnationCount")]
  pub stagnation_count: u32,
  #[serde(default, rename = "progressFingerprint")]
  pub progress_fingerprint: Option<String>,
  #[serde(rename = "lastSummary")]
  pub last_summary: String,
  #[serde(default, rename = "currentRepair")]
  pub current_repair: Option<RepairProgress>,
  #[serde(rename = "blockedReason")]
  pub blocked_reason: Option<String>,
  #[serde(rename = "lastError")]
  pub last_error: Option<String>,
  #[serde(rename = "updatedAt")]
  pub updated_at: String,
}

impl State {
  pub const VERSION: u32 = 1;

  pub fn fresh() -> Self {
    Self {
      version: Self::VERSION,
      status: RunStatus::Idle,
      phase: Phase::Initialized,
      run_id: None,
      cycle: 0,
      active_leases: std::collections::BTreeMap::new(),
      candidate_integrations: Vec::new(),
      deferred_candidates: Vec::new(),
      work_statuses: std::collections::BTreeMap::new(),
      requirement_counts: RequirementCounts::default(),
      verification_layers: VerificationLayers::default(),
      completed_work_units: Vec::new(),
      stagnation_count: 0,
      progress_fingerprint: None,
      discoveries: Vec::new(),
      last_summary: "Initialized".into(),
      blocked_reason: None,
      current_repair: None,
      last_error: None,
      updated_at: chrono::Utc::now().to_rfc3339(),
    }
  }
}
