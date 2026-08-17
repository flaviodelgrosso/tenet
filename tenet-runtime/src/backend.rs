use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use tenet_domain::{config::Config, events::EventSink};

/// Validates worker output against its generated JSON Schema and role-specific Rust type.
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
  pub role: tenet_domain::model::WorkerRole,
  pub worker_id: String,
  pub lease_id: Option<String>,
  pub work_unit_id: Option<String>,
  pub prompt: String,
  pub cwd: PathBuf,
  pub runtime_dir: PathBuf,
  pub schema: serde_json::Value,
  /// Runs JSON Schema validation before the role adapter's typed deserialization boundary.
  pub validate_output: WorkerOutputValidator,
  pub timeout: std::time::Duration,
  pub launch: Option<LaunchMetadata>,
  pub custom: Option<tenet_domain::config::CustomAgentConfig>,
  pub preferences: tenet_domain::config::RolePreference,
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
