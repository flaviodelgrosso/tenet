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
const DEFAULT_MEMORY_MIB: u32 = 1024;
const DEFAULT_VCPUS: u8 = 1;
const DEFAULT_PROCESS_LIMIT: u32 = 256;
const DEFAULT_WRITABLE_ROOT_MIB: u32 = 4096;
const DEFAULT_MAX_INPUT_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_MAX_INPUT_TREE_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_MAX_INPUT_ENTRIES: u32 = 100_000;
const MAX_TIMEOUT_SECS: u64 = 3_600;
const MAX_MEMORY_MIB: u32 = 16 * 1024;
const MAX_VCPUS: u8 = 16;
const MAX_PROCESS_LIMIT: u32 = 4_096;
const MAX_WRITABLE_ROOT_MIB: u32 = 16 * 1024;
const MAX_INPUT_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_INPUT_TREE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_INPUT_ENTRIES: u32 = 250_000;
pub const TRUSTED_VERIFIER_CONTRADICTION_EXIT_CODE: i32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrustedExecutionBackend {
  Microsandbox,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IsolationBoundary {
  HardwareVirtualizedMicroVm,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CandidateFilesystemPolicy {
  PrivateWritable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostRepositoryMountPolicy {
  None,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WritableStoragePolicy {
  DisposableSandboxPrivate,
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
pub enum GuestSecurityProfile {
  Restricted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlChannel {
  LocalHostDriven,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct TrustedIsolationPolicy {
  pub boundary: IsolationBoundary,
  pub candidate_filesystem: CandidateFilesystemPolicy,
  pub host_repository_mounts: HostRepositoryMountPolicy,
  pub writable_storage: WritableStoragePolicy,
  pub network: NetworkPolicy,
  pub environment: EnvironmentPolicy,
  pub guest_security_profile: GuestSecurityProfile,
  pub control_channel: ControlChannel,
}

impl Default for TrustedIsolationPolicy {
  fn default() -> Self {
    Self {
      boundary: IsolationBoundary::HardwareVirtualizedMicroVm,
      candidate_filesystem: CandidateFilesystemPolicy::PrivateWritable,
      host_repository_mounts: HostRepositoryMountPolicy::None,
      writable_storage: WritableStoragePolicy::DisposableSandboxPrivate,
      network: NetworkPolicy::Disabled,
      environment: EnvironmentPolicy::ExplicitOnly,
      guest_security_profile: GuestSecurityProfile::Restricted,
      control_channel: ControlChannel::LocalHostDriven,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct TrustedResourcePolicy {
  pub memory_mib: u32,
  pub vcpus: u8,
  pub process_limit: u32,
  pub writable_root_mib: u32,
  pub max_input_archive_bytes: u64,
  pub max_input_tree_bytes: u64,
  pub max_input_entries: u32,
}

impl Default for TrustedResourcePolicy {
  fn default() -> Self {
    Self {
      memory_mib: DEFAULT_MEMORY_MIB,
      vcpus: DEFAULT_VCPUS,
      process_limit: DEFAULT_PROCESS_LIMIT,
      writable_root_mib: DEFAULT_WRITABLE_ROOT_MIB,
      max_input_archive_bytes: DEFAULT_MAX_INPUT_ARCHIVE_BYTES,
      max_input_tree_bytes: DEFAULT_MAX_INPUT_TREE_BYTES,
      max_input_entries: DEFAULT_MAX_INPUT_ENTRIES,
    }
  }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrustedVerifierProtocol {
  #[default]
  ExitCode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciImageDigest(String);

impl OciImageDigest {
  pub fn parse(value: &str) -> Option<Self> {
    let hex = value.strip_prefix("sha256:")?;
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
      return None;
    }
    Some(Self(format!("sha256:{}", hex.to_ascii_lowercase())))
  }

  pub fn from_image_reference(image: &str) -> Option<Self> {
    let (name, digest) = image.split_once('@')?;
    if name.trim().is_empty()
      || name.contains('@')
      || name.bytes().any(|byte| byte.is_ascii_whitespace())
    {
      return None;
    }
    Self::parse(digest)
  }

  pub fn as_str(&self) -> &str {
    &self.0
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrustedVerificationSpec {
  pub name: String,
  pub backend: TrustedExecutionBackend,
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
    if self.backend != TrustedExecutionBackend::Microsandbox {
      return Err(TrustedVerifierSpecError::UnsupportedBackend);
    }
    self.image_digest()?;
    if self.program.trim().is_empty() {
      return Err(TrustedVerifierSpecError::BlankProgram);
    }
    validate_relative_directory(&self.working_directory)?;
    if !(1..=MAX_TIMEOUT_SECS).contains(&self.timeout_secs) {
      return Err(TrustedVerifierSpecError::InvalidTimeout);
    }
    if !(128..=MAX_MEMORY_MIB).contains(&self.resources.memory_mib) {
      return Err(TrustedVerifierSpecError::InvalidMemoryLimit);
    }
    if !(1..=MAX_VCPUS).contains(&self.resources.vcpus) {
      return Err(TrustedVerifierSpecError::InvalidCpuLimit);
    }
    if !(1..=MAX_PROCESS_LIMIT).contains(&self.resources.process_limit) {
      return Err(TrustedVerifierSpecError::InvalidProcessLimit);
    }
    if !(1..=MAX_WRITABLE_ROOT_MIB).contains(&self.resources.writable_root_mib) {
      return Err(TrustedVerifierSpecError::InvalidWritableStorageLimit);
    }
    if !(1..=MAX_INPUT_ARCHIVE_BYTES).contains(&self.resources.max_input_archive_bytes) {
      return Err(TrustedVerifierSpecError::InvalidInputArchiveLimit);
    }
    if !(1..=MAX_INPUT_TREE_BYTES).contains(&self.resources.max_input_tree_bytes) {
      return Err(TrustedVerifierSpecError::InvalidInputTreeLimit);
    }
    if !(1..=MAX_INPUT_ENTRIES).contains(&self.resources.max_input_entries) {
      return Err(TrustedVerifierSpecError::InvalidInputEntryLimit);
    }
    for name in self.environment.keys() {
      if name.trim().is_empty()
        || name.contains('=')
        || name.starts_with("MSB_")
        || matches!(name.as_str(), "HOME" | "PATH" | "TMPDIR" | "USER")
      {
        return Err(TrustedVerifierSpecError::InvalidEnvironmentName(
          name.clone(),
        ));
      }
    }
    Ok(())
  }

  pub fn image_digest(&self) -> Result<OciImageDigest, TrustedVerifierSpecError> {
    OciImageDigest::from_image_reference(&self.image).ok_or(TrustedVerifierSpecError::UnpinnedImage)
  }

  pub fn fingerprint(&self) -> Result<String, serde_json::Error> {
    fingerprint("tenet-trusted-verifier-spec-v3", self)
  }

  pub fn isolation_policy_hash(&self) -> Result<String, serde_json::Error> {
    fingerprint(
      "tenet-trusted-verifier-isolation-v3",
      &(&self.isolation, &self.resources),
    )
  }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TrustedVerifierSpecError {
  #[error("trusted verifier name must not be blank")]
  BlankName,
  #[error("trusted verifier backend must be microsandbox")]
  UnsupportedBackend,
  #[error("trusted verifier image must use an immutable sha256 digest")]
  UnpinnedImage,
  #[error("trusted verifier program must not be blank")]
  BlankProgram,
  #[error("trusted verifier working_directory must remain within the candidate repository")]
  InvalidWorkingDirectory,
  #[error("trusted verifier timeout_secs is outside controller bounds")]
  InvalidTimeout,
  #[error("trusted verifier memory_mib is outside controller bounds")]
  InvalidMemoryLimit,
  #[error("trusted verifier vcpus is outside controller bounds")]
  InvalidCpuLimit,
  #[error("trusted verifier process_limit is outside controller bounds")]
  InvalidProcessLimit,
  #[error("trusted verifier writable_root_mib is outside controller bounds")]
  InvalidWritableStorageLimit,
  #[error("trusted verifier max_input_archive_bytes is outside controller bounds")]
  InvalidInputArchiveLimit,
  #[error("trusted verifier max_input_tree_bytes is outside controller bounds")]
  InvalidInputTreeLimit,
  #[error("trusted verifier max_input_entries is outside controller bounds")]
  InvalidInputEntryLimit,
  #[error("trusted verifier environment name is reserved or malformed: {0:?}")]
  InvalidEnvironmentName(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IsolationCapabilityReport {
  pub backend: TrustedExecutionBackend,
  pub backend_version: String,
  pub runtime_identity: String,
  pub boundary: IsolationBoundary,
  pub image: String,
  pub resolved_image_digest: String,
  pub input_revision: String,
  pub input_materialization_hash: String,
  pub input_archive_bytes: u64,
  pub input_tree_bytes: u64,
  pub input_entries: u32,
  pub candidate_filesystem: CandidateFilesystemPolicy,
  pub host_repository_mounts: HostRepositoryMountPolicy,
  pub writable_storage: WritableStoragePolicy,
  pub network: NetworkPolicy,
  pub environment: EnvironmentPolicy,
  pub guest_security_profile: GuestSecurityProfile,
  pub guest_user: String,
  pub unprivileged_user: bool,
  pub control_channel: ControlChannel,
  pub memory_mib: u32,
  pub vcpus: u8,
  pub process_limit: u32,
  pub writable_root_mib: u32,
  pub max_input_archive_bytes: u64,
  pub max_input_tree_bytes: u64,
  pub max_input_entries: u32,
  pub execution_timeout_secs: u64,
  pub sandbox_lifetime_secs: u64,
}

impl IsolationCapabilityReport {
  pub fn satisfies(&self, spec: &TrustedVerificationSpec) -> bool {
    self.backend == spec.backend
      && !self.backend_version.trim().is_empty()
      && !self.runtime_identity.trim().is_empty()
      && self.boundary == spec.isolation.boundary
      && self.image == spec.image
      && spec.image_digest().ok().is_some_and(|expected| {
        OciImageDigest::parse(&self.resolved_image_digest).as_ref() == Some(&expected)
      })
      && !self.input_revision.trim().is_empty()
      && !self.input_materialization_hash.trim().is_empty()
      && self.input_archive_bytes <= spec.resources.max_input_archive_bytes
      && self.input_tree_bytes <= spec.resources.max_input_tree_bytes
      && self.input_entries <= spec.resources.max_input_entries
      && self.candidate_filesystem == spec.isolation.candidate_filesystem
      && self.host_repository_mounts == spec.isolation.host_repository_mounts
      && self.writable_storage == spec.isolation.writable_storage
      && self.network == spec.isolation.network
      && self.environment == spec.isolation.environment
      && self.guest_security_profile == spec.isolation.guest_security_profile
      && !self.guest_user.trim().is_empty()
      && self.guest_user != "0"
      && self.guest_user != "root"
      && self.unprivileged_user
      && self.control_channel == spec.isolation.control_channel
      && self.memory_mib == spec.resources.memory_mib
      && self.vcpus == spec.resources.vcpus
      && self.process_limit == spec.resources.process_limit
      && self.writable_root_mib == spec.resources.writable_root_mib
      && self.max_input_archive_bytes == spec.resources.max_input_archive_bytes
      && self.max_input_tree_bytes == spec.resources.max_input_tree_bytes
      && self.max_input_entries == spec.resources.max_input_entries
      && self.execution_timeout_secs == spec.timeout_secs
      && self.sandbox_lifetime_secs == spec.timeout_secs.saturating_add(30)
  }

  pub fn fingerprint(&self) -> Result<String, serde_json::Error> {
    fingerprint("tenet-isolation-capability-report-v1", self)
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
  pub input_materialization_hash: String,
  pub verifier_name: String,
  pub spec_hash: String,
  pub isolation_policy_hash: String,
  pub isolation_report: Option<IsolationCapabilityReport>,
  pub started_at: DateTime<Utc>,
  pub finished_at: DateTime<Utc>,
  pub result: TrustedExecutionResult,
  pub observation: ExecutionObservation,
  pub obligation_ids: Vec<ObligationId>,
}

impl TrustedExecutionRecord {
  pub fn record_hash(&self) -> Result<String, serde_json::Error> {
    fingerprint("tenet-trusted-execution-record-v2", self)
  }

  pub fn can_issue_authority(&self, spec: &TrustedVerificationSpec) -> bool {
    !self.revision.trim().is_empty()
      && !self.input_materialization_hash.trim().is_empty()
      && self.verifier_name == spec.name
      && spec.fingerprint().ok().as_deref() == Some(self.spec_hash.as_str())
      && spec.isolation_policy_hash().ok().as_deref() == Some(self.isolation_policy_hash.as_str())
      && self.isolation_report.as_ref().is_some_and(|report| {
        report.satisfies(spec)
          && report.input_revision == self.revision
          && report.input_materialization_hash == self.input_materialization_hash
      })
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
        ) => {
          *expected == TRUSTED_VERIFIER_CONTRADICTION_EXIT_CODE
            && actual == TRUSTED_VERIFIER_CONTRADICTION_EXIT_CODE
        }
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
      backend: TrustedExecutionBackend::Microsandbox,
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

  fn report(spec: &TrustedVerificationSpec) -> IsolationCapabilityReport {
    IsolationCapabilityReport {
      backend: TrustedExecutionBackend::Microsandbox,
      backend_version: "microsandbox-rust-sdk/0.6.15".into(),
      runtime_identity: "local-msb/sdk-protocol-compatible".into(),
      boundary: IsolationBoundary::HardwareVirtualizedMicroVm,
      image: spec.image.clone(),
      resolved_image_digest: spec
        .image_digest()
        .expect("pinned fixture image")
        .as_str()
        .into(),
      input_revision: "revision".into(),
      input_materialization_hash: "archive-hash".into(),
      input_archive_bytes: 1024,
      input_tree_bytes: 512,
      input_entries: 2,
      candidate_filesystem: CandidateFilesystemPolicy::PrivateWritable,
      host_repository_mounts: HostRepositoryMountPolicy::None,
      writable_storage: WritableStoragePolicy::DisposableSandboxPrivate,
      network: NetworkPolicy::Disabled,
      environment: EnvironmentPolicy::ExplicitOnly,
      guest_security_profile: GuestSecurityProfile::Restricted,
      guest_user: "65532".into(),
      unprivileged_user: true,
      control_channel: ControlChannel::LocalHostDriven,
      memory_mib: spec.resources.memory_mib,
      vcpus: spec.resources.vcpus,
      process_limit: spec.resources.process_limit,
      writable_root_mib: spec.resources.writable_root_mib,
      max_input_archive_bytes: spec.resources.max_input_archive_bytes,
      max_input_tree_bytes: spec.resources.max_input_tree_bytes,
      max_input_entries: spec.resources.max_input_entries,
      execution_timeout_secs: spec.timeout_secs,
      sandbox_lifetime_secs: spec.timeout_secs + 30,
    }
  }

  #[test]
  fn fingerprint_changes_with_execution_semantics() {
    let original = spec();
    let original_hash = original.fingerprint().expect("original fingerprint");
    let mut changes = Vec::new();
    let mut image = original.clone();
    image.image = format!("example/other@sha256:{}", "c".repeat(64));
    changes.push(image);
    let mut program = original.clone();
    program.program = "other-verifier".into();
    changes.push(program);
    let mut args = original.clone();
    args.args.push("--strict".into());
    changes.push(args);
    let mut environment = original.clone();
    environment
      .environment
      .insert("MODE".into(), "strict".into());
    changes.push(environment);
    let mut resources = original.clone();
    resources.resources.process_limit -= 1;
    changes.push(resources);

    assert!(changes
      .into_iter()
      .all(|changed| { changed.fingerprint().expect("changed fingerprint") != original_hash }));
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
  fn malformed_or_unpinned_image_digest_is_rejected() {
    for image in [
      "example/verifier:latest",
      "example/verifier@sha512:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "example/verifier@sha256:abc",
      "example/verifier@sha256:gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
      "example/first@example/verifier@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
      let mut value = spec();
      value.image = image.into();
      assert_eq!(
        value.validate(),
        Err(TrustedVerifierSpecError::UnpinnedImage),
        "accepted malformed image {image}"
      );
    }
  }

  #[test]
  fn image_digest_is_normalized_to_lowercase() {
    let mut value = spec();
    value.image = format!("example/verifier@sha256:{}", "A".repeat(64));

    assert_eq!(
      value.image_digest().expect("valid digest").as_str(),
      format!("sha256:{}", "a".repeat(64))
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

  #[test]
  fn controller_resource_ceilings_reject_host_exhaustion_configuration() {
    let mut timeout = spec();
    timeout.timeout_secs = MAX_TIMEOUT_SECS + 1;
    assert_eq!(
      timeout.validate(),
      Err(TrustedVerifierSpecError::InvalidTimeout)
    );

    let mut memory = spec();
    memory.resources.memory_mib = MAX_MEMORY_MIB + 1;
    assert_eq!(
      memory.validate(),
      Err(TrustedVerifierSpecError::InvalidMemoryLimit)
    );

    let mut cpus = spec();
    cpus.resources.vcpus = MAX_VCPUS + 1;
    assert_eq!(
      cpus.validate(),
      Err(TrustedVerifierSpecError::InvalidCpuLimit)
    );

    let mut processes = spec();
    processes.resources.process_limit = MAX_PROCESS_LIMIT + 1;
    assert_eq!(
      processes.validate(),
      Err(TrustedVerifierSpecError::InvalidProcessLimit)
    );

    let mut root = spec();
    root.resources.writable_root_mib = MAX_WRITABLE_ROOT_MIB + 1;
    assert_eq!(
      root.validate(),
      Err(TrustedVerifierSpecError::InvalidWritableStorageLimit)
    );

    let mut archive = spec();
    archive.resources.max_input_archive_bytes = MAX_INPUT_ARCHIVE_BYTES + 1;
    assert_eq!(
      archive.validate(),
      Err(TrustedVerifierSpecError::InvalidInputArchiveLimit)
    );

    let mut tree = spec();
    tree.resources.max_input_tree_bytes = MAX_INPUT_TREE_BYTES + 1;
    assert_eq!(
      tree.validate(),
      Err(TrustedVerifierSpecError::InvalidInputTreeLimit)
    );

    let mut entries = spec();
    entries.resources.max_input_entries = MAX_INPUT_ENTRIES + 1;
    assert_eq!(
      entries.validate(),
      Err(TrustedVerifierSpecError::InvalidInputEntryLimit)
    );
  }

  #[test]
  fn matching_resolved_image_digest_satisfies_spec() {
    let spec = spec();

    assert!(report(&spec).satisfies(&spec));
  }

  #[test]
  fn mismatched_resolved_image_digest_does_not_satisfy_spec() {
    let spec = spec();
    let mut report = report(&spec);
    report.resolved_image_digest = format!("sha256:{}", "b".repeat(64));

    assert!(!report.satisfies(&spec));
  }

  #[test]
  fn reconstructed_mismatched_report_does_not_satisfy_spec() {
    let spec = spec();
    let mut original = report(&spec);
    original.resolved_image_digest = format!("sha256:{}", "b".repeat(64));
    let encoded = serde_json::to_vec(&original).expect("serialize report");
    let reconstructed: IsolationCapabilityReport =
      serde_json::from_slice(&encoded).expect("reconstruct report");

    assert!(!reconstructed.satisfies(&spec));
  }

  #[test]
  fn weaker_or_different_capability_report_cannot_satisfy_policy() {
    let spec = spec();
    let mut weaker = report(&spec);
    weaker.unprivileged_user = false;
    assert!(!weaker.satisfies(&spec));

    let mut different = report(&spec);
    different.memory_mib -= 1;
    assert!(!different.satisfies(&spec));

    assert!(report(&spec).satisfies(&spec));
  }
}
