use std::{collections::BTreeMap, path::Path};

use anyhow::Result;
use async_trait::async_trait;
use tenet_domain::{
  config::Config,
  evidence::{EvidenceProjection, SemanticAssessmentProposal},
  ids::{ObligationId, RequirementId},
  model::{
    AgentReconciliationProposal, ArchitectOutput, CompletedWorkUnit, Discovery, RequirementCatalog,
    VerificationReport, WorkUnit, WorkerSummary,
  },
  verification::ProjectVerificationRun,
};
use tenet_runtime::backend::{BackendContext, LaunchMetadata};

/// Controller-selected reconciliation inputs supplied to a read-only agent.
pub struct ReconciliationRequest<'a> {
  pub catalog: &'a RequirementCatalog,
  pub requirement_handles: &'a BTreeMap<RequirementId, String>,
  pub recent_completed: &'a [CompletedWorkUnit],
  pub discoveries: &'a [Discovery],
  pub evidence: &'a [EvidenceProjection],
}

/// Controller-selected semantic assessment inputs supplied to a read-only agent.
pub struct SemanticAssessmentRequest<'a> {
  pub catalog: &'a RequirementCatalog,
  pub obligation_handles: &'a BTreeMap<ObligationId, String>,
  pub project_verification: &'a ProjectVerificationRun,
  pub evidence: &'a [EvidenceProjection],
}

/// Role-oriented coding-agent port used by the Tenet control plane.
#[async_trait]
pub trait AgentBackend: Send + Sync {
  async fn architect(&self, ctx: &BackendContext, spec: &str) -> Result<ArchitectOutput>;

  /// Resolves the configured launch before the controller publishes run state.
  async fn resolve_launch(&self, cwd: &Path, config: &Config) -> Result<Option<LaunchMetadata>>;

  async fn reconcile(
    &self,
    ctx: &BackendContext,
    request: ReconciliationRequest<'_>,
    semantic_validation_feedback: Option<&str>,
  ) -> Result<AgentReconciliationProposal>;

  async fn implement(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
    work_unit: &WorkUnit,
    discoveries: &[Discovery],
  ) -> Result<WorkerSummary>;

  async fn repair(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
    work_unit: &WorkUnit,
    discoveries: &[Discovery],
    report: &VerificationReport,
  ) -> Result<WorkerSummary>;

  async fn assess(
    &self,
    ctx: &BackendContext,
    request: SemanticAssessmentRequest<'_>,
    semantic_validation_feedback: Option<&str>,
  ) -> Result<SemanticAssessmentProposal>;
}
