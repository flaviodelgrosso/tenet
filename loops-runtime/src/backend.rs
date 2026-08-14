use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use loops_domain::{
  config::Config,
  events::EventSink,
  model::{
    ArchitectOutput, CompletedWorkUnit, Discovery, ReconcileResult, RequirementCatalog,
    VerificationReport, WorkUnit, WorkerSummary,
  },
};

/// Validates a schema-valid worker result against its role-specific output type.
pub type WorkerOutputValidator = Arc<dyn Fn(&serde_json::Value) -> Result<()> + Send + Sync>;

#[derive(Clone)]
pub struct BackendContext {
  pub cwd: PathBuf,
  pub runtime_dir: PathBuf,
  pub config: Arc<Config>,
  pub cancel: CancellationToken,
  pub events: EventSink,
  pub launch: Option<LaunchMetadata>,
  pub worker_id: String,
  pub lease_id: Option<String>,
  pub work_unit_id: Option<String>,
}

/// A role-independent request executed by an agent runtime.
#[derive(Clone)]
pub struct WorkerRequest {
  pub role: loops_domain::model::WorkerRole,
  pub worker_id: String,
  pub lease_id: Option<String>,
  pub work_unit_id: Option<String>,
  pub prompt: String,
  pub cwd: PathBuf,
  pub runtime_dir: PathBuf,
  pub schema: serde_json::Value,
  /// Validates that schema-valid output can be deserialized by the role adapter.
  pub validate_output: WorkerOutputValidator,
  pub timeout: std::time::Duration,
  pub launch: Option<LaunchMetadata>,
  pub custom: Option<loops_domain::config::CustomAgentConfig>,
  pub preferences: loops_domain::config::RolePreference,
  pub completion_retries: u32,
}

/// The structured response produced by a worker runtime.
#[derive(Clone, Debug)]
pub struct WorkerResult {
  pub structured_output: serde_json::Value,
}

/// Command details resolved from an agent registry or supplied by a host.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LaunchMetadata {
  pub command: String,
  pub args: Vec<String>,
  pub env: BTreeMap<String, String>,
  pub provenance: String,
}

#[async_trait]
pub trait AgentRuntime: Send + Sync {
  async fn run_worker(&self, request: WorkerRequest) -> Result<WorkerResult>;
}

#[async_trait]
pub trait AgentBackend: Send + Sync {
  async fn architect(&self, ctx: &BackendContext, spec: &str) -> Result<ArchitectOutput>;

  /// Resolves the configured agent launch once before the runtime publishes run state.
  async fn resolve_launch(
    &self,
    cwd: &std::path::Path,
    config: &Config,
  ) -> Result<Option<LaunchMetadata>>;

  async fn reconcile(
    &self,
    ctx: &BackendContext,
    catalog: &RequirementCatalog,
    recent_completed: &[CompletedWorkUnit],
    discoveries: &[Discovery],
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
