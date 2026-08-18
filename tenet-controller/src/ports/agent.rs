use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use tenet_domain::{
  config::Config,
  evidence::{EvidenceProjection, SemanticAssessmentReport},
  model::{
    ArchitectOutput, CompletedWorkUnit, Discovery, ReconcileResult, RequirementCatalog,
    VerificationReport, WorkUnit, WorkerSummary,
  },
  verification::ProjectVerificationRun,
};
use tenet_runtime::backend::{BackendContext, LaunchMetadata};

/// Role-oriented coding-agent port used by the Tenet control plane.
#[async_trait]
pub trait AgentBackend: Send + Sync {
  async fn architect(&self, ctx: &BackendContext, spec: &str) -> Result<ArchitectOutput>;

  /// Resolves the configured launch before the controller publishes run state.
  async fn resolve_launch(&self, cwd: &Path, config: &Config) -> Result<Option<LaunchMetadata>>;

  async fn reconcile(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
    recent_completed: &[CompletedWorkUnit],
    discoveries: &[Discovery],
    evidence: &[EvidenceProjection],
    semantic_validation_feedback: Option<&str>,
  ) -> Result<ReconcileResult>;

  async fn implement(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
    work_unit: &WorkUnit,
  ) -> Result<WorkerSummary>;

  async fn repair(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
    work_unit: &WorkUnit,
    report: &VerificationReport,
  ) -> Result<WorkerSummary>;

  async fn assess(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
    project_verification: &ProjectVerificationRun,
    evidence: &[EvidenceProjection],
    semantic_validation_feedback: Option<&str>,
  ) -> Result<SemanticAssessmentReport>;
}
