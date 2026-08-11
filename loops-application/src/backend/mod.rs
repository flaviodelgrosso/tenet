pub mod omp_rpc;

use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use loops_domain::{
  config::Config,
  events::EventSink,
  model::{
    ArchitectOutput, CompletedWorkUnit, ReconcileResult, RequirementCatalog, VerificationReport,
    WorkUnit, WorkerSummary,
  },
};

#[derive(Clone)]
pub struct BackendContext {
  pub cwd: PathBuf,
  pub runtime_dir: PathBuf,
  pub config: Arc<Config>,
  pub cancel: CancellationToken,
  pub events: EventSink,
}

#[async_trait]
pub trait AgentBackend: Send + Sync {
  async fn architect(&self, ctx: &BackendContext, spec: &str) -> Result<ArchitectOutput>;
  async fn reconcile(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
    recent_completed: &[CompletedWorkUnit],
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
  ) -> Result<ReconcileResult>;
}
