use std::{collections::BTreeMap, fmt::Write as _};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
  ids::{ObligationId, VerificationRunId},
  proof::{ArtifactObservation, ExecutionObservation},
};

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const DEFAULT_MEMORY_BYTES: u64 = 1024 * 1024 * 1024;
const DEFAULT_CPU_MILLIS: u32 = 1000;
const DEFAULT_PROCESS_LIMIT: u32 = 256;
const DEFAULT_TMP_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrustedVerifierBackend {
  Docker,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CandidateFilesystemPolicy {
  ReadOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RootFilesystemPolicy {
  ReadOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TemporaryFilesystemPolicy {
  DisposableTmpfs,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
  Disabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentPolicy {
  ExplicitOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessNamespacePolicy {
  Private,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlanePolicy {
  ExclusiveMutualTls,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct TrustedIsolationPolicy {
  pub candidate_filesystem: CandidateFilesystemPolicy,
  pub root_filesystem: RootFilesystemPolicy,
  pub temporary_filesystem: TemporaryFilesystemPolicy,
  pub network: NetworkPolicy,
  pub environment: EnvironmentPolicy,
  pub process_namespace: ProcessNamespacePolicy,
  pub control_plane: ControlPlanePolicy,
}

impl Default for TrustedIsolationPolicy {
  fn default() -> Self {
    Self {
      candidate_filesystem: CandidateFilesystemPolicy::ReadOnly,
      root_filesystem: RootFilesystemPolicy::ReadOnly,
      temporary_filesystem: TemporaryFilesystemPolicy::DisposableTmpfs,
      network: NetworkPolicy::Disabled,
      environment: EnvironmentPolicy::ExplicitOnly,
      process_namespace: ProcessNamespacePolicy::Private,
      control_plane: ControlPlanePolicy::ExclusiveMutualTls,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct TrustedResourcePolicy {
  pub memory_bytes: u64,
  pub cpu_millis: u32,
  pub process_limit: u32,
  pub writable_tmp_bytes: u64,
}

impl Default for TrustedResourcePolicy {
  fn default() -> Self {
    Self {
      memory_bytes: DEFAULT_MEMORY_BYTES,
      cpu_millis: DEFAULT_CPU_MILLIS,
      process_limit: DEFAULT_PROCESS_LIMIT,
      writable_tmp_bytes: DEFAULT_TMP_BYTES,
    }
  }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrustedVerifierProtocol {
  #[default]
  ExitCode,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrustedVerificationSpec {
  pub name: String,
  pub backend: TrustedVerifierBackend,
  pub image: String,
  pub program: String,
  #[serde(default)]
  pub args: Vec<String>,
  #[serde(default = "default_working_directory")]
  pub working_directory: String,
  #[serde(default)]
  pub environment: BTreeMap<String, String>,
  #[serde(default = "default_timeout_secs")]
  pub timeout_secs: u64,
  #[serde(default)]
  pub isolation: TrustedIsolationPolicy,
  #[serde(default)]
  pub resources: TrustedResourcePolicy,
  #[serde(default)]
  pub protocol: TrustedVerifierProtocol,
}

impl TrustedVerificationSpec {
  pub fn validate(&self) -> Result<(), TrustedVerifierSpecError> {
    if self.name.trim().is_empty() {
      return Err(TrustedVerifierSpecError::BlankName);
    }
    if !is_digest_pinned_image(&self.image) {
      return Err(TrustedVerifierSpecError::UnpinnedImage);
    }
    if self.program.trim().is_empty() {
      return Err(TrustedVerifierSpecError::BlankProgram);
    }
    validate_relative_directory(&self.working_directory)?;
    if self.timeout_secs == 0 {
      return Err(TrustedVerifierSpecError::InvalidTimeout);
    }
    if self.resources.memory_bytes < 6 * 1024 * 1024 {
      return Err(TrustedVerifierSpecError::InvalidMemoryLimit);
    }
    if self.resources.cpu_millis == 0 {
      return Err(TrustedVerifierSpecError::InvalidCpuLimit);
    }
    if self.resources.process_limit == 0 {
      return Err(TrustedVerifierSpecError::InvalidProcessLimit);
    }
    if self.resources.writable_tmp_bytes == 0 {
      return Err(TrustedVerifierSpecError::InvalidTemporaryStorageLimit);
    }
    for name in self.environment.keys() {
      if name.trim().is_empty() || name.contains('=') || matches!(name.as_str(), "HOME" | "TMPDIR")
      {
        return Err(TrustedVerifierSpecError::InvalidEnvironmentName(
          name.clone(),
        ));
      }
    }
    Ok(())
  }

  pub fn fingerprint(&self) -> Result<String, serde_json::Error> {
    fingerprint("tenet-trusted-verifier-spec-v2", self)
  }

  pub fn isolation_policy_hash(&self) -> Result<String, serde_json::Error> {
    fingerprint(
      "tenet-trusted-verifier-isolation-v2",
      &(&self.isolation, &self.resources),
    )
  }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TrustedVerifierSpecError {
  #[error("trusted verifier name must not be blank")]
  BlankName,
  #[error("trusted verifier image must use an immutable sha256 digest")]
  UnpinnedImage,
  #[error("trusted verifier program must not be blank")]
  BlankProgram,
  #[error("trusted verifier working_directory must remain within the candidate repository")]
  InvalidWorkingDirectory,
  #[error("trusted verifier timeout_secs must be at least 1")]
  InvalidTimeout,
  #[error("trusted verifier memory_bytes must be at least 6291456")]
  InvalidMemoryLimit,
  #[error("trusted verifier cpu_millis must be at least 1")]
  InvalidCpuLimit,
  #[error("trusted verifier process_limit must be at least 1")]
  InvalidProcessLimit,
  #[error("trusted verifier writable_tmp_bytes must be at least 1")]
  InvalidTemporaryStorageLimit,
  #[error("trusted verifier environment name is reserved or malformed: {0:?}")]
  InvalidEnvironmentName(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrustedExecutionAttestation {
  pub backend: TrustedVerifierBackend,
  pub backend_version: String,
  pub image_id: String,
  pub control_plane: ControlPlanePolicy,
  pub control_plane_fingerprint: String,
  pub candidate_filesystem: CandidateFilesystemPolicy,
  pub root_filesystem: RootFilesystemPolicy,
  pub temporary_filesystem: TemporaryFilesystemPolicy,
  pub network: NetworkPolicy,
  pub environment: EnvironmentPolicy,
  pub process_namespace: ProcessNamespacePolicy,
  pub capabilities_dropped: bool,
  pub no_new_privileges: bool,
  pub unprivileged_user: bool,
  pub memory_bytes: u64,
  pub cpu_millis: u32,
  pub process_limit: u32,
  pub writable_tmp_bytes: u64,
}

impl TrustedExecutionAttestation {
  pub fn satisfies(&self, spec: &TrustedVerificationSpec) -> bool {
    self.backend == spec.backend
      && !self.backend_version.trim().is_empty()
      && !self.image_id.trim().is_empty()
      && self.control_plane == spec.isolation.control_plane
      && !self.control_plane_fingerprint.trim().is_empty()
      && self.candidate_filesystem == spec.isolation.candidate_filesystem
      && self.root_filesystem == spec.isolation.root_filesystem
      && self.temporary_filesystem == spec.isolation.temporary_filesystem
      && self.network == spec.isolation.network
      && self.environment == spec.isolation.environment
      && self.process_namespace == spec.isolation.process_namespace
      && self.capabilities_dropped
      && self.no_new_privileges
      && self.unprivileged_user
      && self.memory_bytes <= spec.resources.memory_bytes
      && self.cpu_millis <= spec.resources.cpu_millis
      && self.process_limit <= spec.resources.process_limit
      && self.writable_tmp_bytes <= spec.resources.writable_tmp_bytes
  }

  pub fn fingerprint(&self) -> Result<String, serde_json::Error> {
    fingerprint("tenet-trusted-verifier-attestation-v2", self)
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "classification", rename_all = "snake_case")]
pub enum TrustedExecutionResult {
  Supports,
  Contradicts { exit_code: i32 },
  TimedOut,
  InfrastructureFailure { message: String },
}

impl TrustedExecutionResult {
  pub fn authoritative_observation(&self) -> Option<ArtifactObservation> {
    match self {
      Self::Supports => Some(ArtifactObservation::Supports),
      Self::Contradicts { .. } => Some(ArtifactObservation::Contradicts),
      Self::TimedOut | Self::InfrastructureFailure { .. } => None,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrustedExecutionRecord {
  pub id: VerificationRunId,
  pub revision: String,
  pub verifier_name: String,
  pub spec_hash: String,
  pub isolation_policy_hash: String,
  pub attestation: Option<TrustedExecutionAttestation>,
  pub started_at: DateTime<Utc>,
  pub finished_at: DateTime<Utc>,
  pub result: TrustedExecutionResult,
  pub observation: ExecutionObservation,
  pub obligation_ids: Vec<ObligationId>,
}

impl TrustedExecutionRecord {
  pub fn record_hash(&self) -> Result<String, serde_json::Error> {
    fingerprint("tenet-trusted-execution-record-v1", self)
  }

  pub fn can_issue_authority(&self, spec: &TrustedVerificationSpec) -> bool {
    !self.revision.trim().is_empty()
      && self.verifier_name == spec.name
      && spec.fingerprint().ok().as_deref() == Some(self.spec_hash.as_str())
      && spec.isolation_policy_hash().ok().as_deref() == Some(self.isolation_policy_hash.as_str())
      && self
        .attestation
        .as_ref()
        .is_some_and(|attestation| attestation.satisfies(spec))
      && match (
        &self.result,
        self.observation.exit_code,
        self.observation.timed_out,
      ) {
        (TrustedExecutionResult::Supports, Some(0), false) => true,
        (
          TrustedExecutionResult::Contradicts {
            exit_code: expected,
          },
          Some(actual),
          false,
        ) => *expected == actual && actual != 0,
        _ => false,
      }
      && !self.obligation_ids.is_empty()
  }
}

fn default_working_directory() -> String {
  ".".into()
}

fn default_timeout_secs() -> u64 {
  DEFAULT_TIMEOUT_SECS
}

fn is_digest_pinned_image(image: &str) -> bool {
  let Some((name, digest)) = image.rsplit_once("@sha256:") else {
    return false;
  };
  !name.trim().is_empty()
    && digest.len() == 64
    && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_relative_directory(value: &str) -> Result<(), TrustedVerifierSpecError> {
  let path = std::path::Path::new(value);
  if value.trim().is_empty()
    || path.is_absolute()
    || path.components().any(|component| {
      !matches!(
        component,
        std::path::Component::Normal(_) | std::path::Component::CurDir
      )
    })
  {
    return Err(TrustedVerifierSpecError::InvalidWorkingDirectory);
  }
  Ok(())
}

fn fingerprint<T: Serialize>(domain: &str, value: &T) -> Result<String, serde_json::Error> {
  let bytes = serde_json::to_vec(&(domain, value))?;
  let digest = Sha256::digest(bytes);
  let mut hash = String::with_capacity(digest.len() * 2);
  for byte in digest {
    write!(hash, "{byte:02x}").expect("writing to String cannot fail");
  }
  Ok(hash)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn spec() -> TrustedVerificationSpec {
    TrustedVerificationSpec {
      name: "expiry-boundary".into(),
      backend: TrustedVerifierBackend::Docker,
      image: format!("example/verifier@sha256:{}", "a".repeat(64)),
      program: "verify".into(),
      args: vec!["--expiry".into()],
      working_directory: ".".into(),
      environment: BTreeMap::from([("CI".into(), "true".into())]),
      timeout_secs: 30,
      isolation: TrustedIsolationPolicy::default(),
      resources: TrustedResourcePolicy::default(),
      protocol: TrustedVerifierProtocol::ExitCode,
    }
  }

  #[test]
  fn fingerprint_changes_with_execution_semantics() {
    let original = spec();
    let mut changed = original.clone();
    changed.resources.process_limit -= 1;

    assert_ne!(
      original.fingerprint().expect("original fingerprint"),
      changed.fingerprint().expect("changed fingerprint")
    );
  }

  #[test]
  fn mutable_image_tag_is_rejected() {
    let mut value = spec();
    value.image = "example/verifier:latest".into();

    assert_eq!(
      value.validate(),
      Err(TrustedVerifierSpecError::UnpinnedImage)
    );
  }

  #[test]
  fn controller_environment_names_are_reserved() {
    let mut value = spec();
    value.environment.insert("HOME".into(), "/host".into());

    assert_eq!(
      value.validate(),
      Err(TrustedVerifierSpecError::InvalidEnvironmentName(
        "HOME".into()
      ))
    );
  }
}
