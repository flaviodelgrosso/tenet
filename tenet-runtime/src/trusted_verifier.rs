use std::{
  future::Future,
  io::{Read, Seek},
  path::{Path, PathBuf},
  sync::Arc,
  time::{Duration, Instant},
};

use async_trait::async_trait;
use chrono::Utc;
use microsandbox::{
  backend::{Backend, LocalBackend},
  sandbox::{
    DeploymentProfile, FsSetAttrs, RlimitResource, RootDisk, RootfsSource, SecurityProfile,
    VolumeMount,
  },
  with_backend, ExecEvent, ExecHandle, Image, MicrosandboxError, Sandbox, SandboxConfig,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::AsyncReadExt as _;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use tenet_domain::{
  ids::{ObligationId, VerificationRunId},
  proof::ExecutionObservation,
  trusted_verifier::{
    CandidateFilesystemPolicy, ControlChannel, EnvironmentPolicy, GuestSecurityProfile,
    HostRepositoryMountPolicy, IsolationBoundary, IsolationCapabilityReport, NetworkPolicy,
    TrustedExecutionBackend, TrustedExecutionRecord, TrustedExecutionResult, TrustedResourcePolicy,
    TrustedVerificationSpec, WritableStoragePolicy, TRUSTED_VERIFIER_CONTRADICTION_EXIT_CODE,
  },
};

use crate::{git, workspace::WorkspaceManager};

const GUEST_WORKSPACE: &str = "/workspace";
const GUEST_BOOT_WORKDIR: &str = "/";
const GUEST_USER: &str = "65532";
const GUEST_UID: u32 = 65_532;
const MICROSANDBOX_SDK_VERSION: &str = "0.6.15";
const SANDBOX_LIFETIME_GRACE_SECS: u64 = 30;
const CLEANUP_TIMEOUT_SECS: u64 = 10;
const METRICS_READINESS_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Error)]
pub enum TrustedVerifierError {
  #[error("trusted verifier specification is invalid: {0}")]
  InvalidTrustedVerifierSpec(String),
  #[error("trusted verifier runtime is unavailable: {0}")]
  TrustedVerifierUnavailable(String),
  #[error("trusted verifier isolation is unavailable: {0}")]
  IsolationUnavailable(String),
  #[error("trusted verifier infrastructure failed: {0}")]
  TrustedVerifierInfrastructureFailure(String),
  #[error("trusted verifier timed out")]
  TrustedVerifierTimeout,
  #[error("trusted verifier execution was cancelled")]
  Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustedVerifierExecution {
  Supports(TrustedExecutionRecord),
  TrustedVerifierObservedContradiction(TrustedExecutionRecord),
}

impl TrustedVerifierExecution {
  pub fn record(&self) -> &TrustedExecutionRecord {
    match self {
      Self::Supports(record) | Self::TrustedVerifierObservedContradiction(record) => record,
    }
  }

  pub fn into_record(self) -> TrustedExecutionRecord {
    match self {
      Self::Supports(record) | Self::TrustedVerifierObservedContradiction(record) => record,
    }
  }
}

#[async_trait]
pub trait TrustedVerifierRunner: Send + Sync {
  async fn execute(
    &self,
    candidate: &Path,
    revision: &str,
    spec: &TrustedVerificationSpec,
    obligation_ids: &[ObligationId],
    max_output_bytes: usize,
    cancel: &CancellationToken,
  ) -> Result<TrustedVerifierExecution, TrustedVerifierError>;
}

#[derive(Debug, Clone)]
pub struct MicrosandboxTrustedVerifier {
  availability: Result<(), String>,
}

impl Default for MicrosandboxTrustedVerifier {
  fn default() -> Self {
    let availability = microsandbox::setup::is_installed()
      .then_some(())
      .ok_or_else(|| "required local Microsandbox runtime files are unavailable".into());
    Self { availability }
  }
}

impl MicrosandboxTrustedVerifier {
  #[cfg(test)]
  fn unavailable(message: impl Into<String>) -> Self {
    Self {
      availability: Err(message.into()),
    }
  }

  fn require_available(&self) -> Result<(), TrustedVerifierError> {
    self
      .availability
      .as_ref()
      .map_err(|message| TrustedVerifierError::TrustedVerifierUnavailable(message.clone()))
      .copied()
  }
}

#[async_trait]
impl TrustedVerifierRunner for MicrosandboxTrustedVerifier {
  async fn execute(
    &self,
    candidate: &Path,
    revision: &str,
    spec: &TrustedVerificationSpec,
    obligation_ids: &[ObligationId],
    max_output_bytes: usize,
    cancel: &CancellationToken,
  ) -> Result<TrustedVerifierExecution, TrustedVerifierError> {
    self.require_available()?;
    spec
      .validate()
      .map_err(|error| TrustedVerifierError::InvalidTrustedVerifierSpec(error.to_string()))?;
    if spec.backend != TrustedExecutionBackend::Microsandbox {
      return Err(TrustedVerifierError::InvalidTrustedVerifierSpec(
        "unsupported trusted verifier backend".into(),
      ));
    }
    if obligation_ids.is_empty() {
      return Err(TrustedVerifierError::InvalidTrustedVerifierSpec(
        "trusted verifier requires exact obligation bindings".into(),
      ));
    }

    let candidate = candidate.canonicalize().map_err(|error| {
      TrustedVerifierError::IsolationUnavailable(format!(
        "canonicalize immutable candidate input: {error}"
      ))
    })?;
    let materialized = materialize_revision(&candidate, revision, &spec.resources).await?;
    let backend: Arc<dyn Backend> = Arc::new(
      LocalBackend::builder()
        .deployment_profile(DeploymentProfile::SingleTenant)
        .try_build_lazy()
        .map_err(|error| TrustedVerifierError::TrustedVerifierUnavailable(error.to_string()))?,
    );

    with_backend(backend, async {
      execute_in_local_microsandbox(
        &materialized,
        revision,
        spec,
        obligation_ids,
        max_output_bytes,
        cancel,
      )
      .await
    })
    .await
  }
}

struct MaterializedRevision {
  directory: tempfile::TempDir,
  archive_hash: String,
  archive_bytes: u64,
  tree_bytes: u64,
  entries: u32,
}

struct RuntimeBinding<'a> {
  revision: &'a str,
  runtime_identity: &'a str,
}

async fn materialize_revision(
  candidate: &Path,
  revision: &str,
  resources: &TrustedResourcePolicy,
) -> Result<MaterializedRevision, TrustedVerifierError> {
  if git::has_gitlinks(candidate, revision)
    .await
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?
  {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "trusted revision materialization does not admit Git submodules".into(),
    ));
  }
  if git::path_exists(candidate, revision, ".tenet")
    .await
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?
  {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "trusted revision materialization rejects controller-owned .tenet content".into(),
    ));
  }

  let observed = git::head(candidate)
    .await
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
  if observed != revision {
    return Err(TrustedVerifierError::IsolationUnavailable(format!(
      "candidate revision {observed} does not match requested {revision}"
    )));
  }
  let (archive, archive_bytes) =
    git::archive(candidate, revision, resources.max_input_archive_bytes)
      .await
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
  let directory = tempfile::tempdir().map_err(|error| {
    TrustedVerifierError::IsolationUnavailable(format!(
      "create private revision export directory: {error}"
    ))
  })?;
  let export = directory.path().to_path_buf();
  let tree_limit = resources.max_input_tree_bytes;
  let entry_limit = resources.max_input_entries;
  let (archive_hash, tree_bytes, entries) =
    tokio::task::spawn_blocking(move || unpack_and_hash(archive, &export, tree_limit, entry_limit))
      .await
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
  Ok(MaterializedRevision {
    directory,
    archive_hash,
    archive_bytes,
    tree_bytes,
    entries,
  })
}

fn unpack_and_hash(
  mut source: std::fs::File,
  destination: &Path,
  max_tree_bytes: u64,
  max_entries: u32,
) -> std::io::Result<(String, u64, u32)> {
  source.rewind()?;
  let mut reader = HashingReader {
    inner: source,
    digest: Sha256::new(),
  };
  let mut tree_bytes = 0_u64;
  let mut entries = 0_u32;
  {
    let mut archive = tar::Archive::new(&mut reader);
    archive.set_preserve_permissions(true);
    for entry in archive.entries()? {
      let mut entry = entry?;
      let entry_type = entry.header().entry_type();
      if entry_type.is_pax_global_extensions() {
        std::io::copy(&mut entry, &mut std::io::sink())?;
        continue;
      }
      if !(entry_type.is_file() || entry_type.is_dir() || entry_type.is_symlink()) {
        return Err(std::io::Error::other(
          "trusted input archive contains an unsupported entry type",
        ));
      }
      entries = entries
        .checked_add(1)
        .ok_or_else(|| std::io::Error::other("trusted input entry count overflow"))?;
      if entries > max_entries {
        return Err(std::io::Error::other(format!(
          "trusted input exceeds entry limit of {max_entries}"
        )));
      }
      if entry_type.is_file() {
        tree_bytes = tree_bytes
          .checked_add(entry.size())
          .ok_or_else(|| std::io::Error::other("trusted input tree size overflow"))?;
        if tree_bytes > max_tree_bytes {
          return Err(std::io::Error::other(format!(
            "trusted input exceeds materialized tree limit of {max_tree_bytes} bytes"
          )));
        }
      }
      if !entry.unpack_in(destination)? {
        return Err(std::io::Error::other(
          "trusted input archive contains a path outside the export directory",
        ));
      }
    }
  }
  let digest = reader.digest.finalize();
  Ok((hex_digest(&digest), tree_bytes, entries))
}

struct HashingReader<R> {
  inner: R,
  digest: Sha256,
}

impl<R: Read> Read for HashingReader<R> {
  fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
    let read = self.inner.read(buffer)?;
    self.digest.update(&buffer[..read]);
    Ok(read)
  }
}

async fn execute_in_local_microsandbox(
  materialized: &MaterializedRevision,
  revision: &str,
  spec: &TrustedVerificationSpec,
  obligation_ids: &[ObligationId],
  max_output_bytes: usize,
  cancel: &CancellationToken,
) -> Result<TrustedVerifierExecution, TrustedVerifierError> {
  let runtime_identity = local_runtime_identity().await?;
  let name = format!("tenet-verifier-{}", Uuid::new_v4().simple());
  let config = build_sandbox_config(&name, spec).await?;
  validate_requested_config(&config, spec)?;

  let sandbox = match sandbox_request(Sandbox::create(config), spec.timeout_secs, cancel).await {
    Ok(sandbox) => sandbox,
    Err(error) => {
      let cleanup = cleanup_named_sandbox(&name).await;
      return match cleanup {
        Ok(()) => Err(error),
        Err(cleanup) => Err(cleanup),
      };
    }
  };
  let binding = RuntimeBinding {
    revision,
    runtime_identity: &runtime_identity,
  };
  let execution = execute_created_sandbox(
    &sandbox,
    materialized,
    &binding,
    spec,
    obligation_ids,
    max_output_bytes,
    cancel,
  )
  .await;
  let cleanup = cleanup_sandbox(&sandbox).await;
  let execution = match (execution, cleanup) {
    (Ok(execution), Ok(())) => execution,
    (Ok(_), Err(error)) | (Err(error), _) => return Err(error),
  };
  if local_runtime_identity().await? != runtime_identity {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "local Microsandbox runtime files changed during trusted execution".into(),
    ));
  }
  Ok(execution)
}

async fn build_sandbox_config(
  name: &str,
  spec: &TrustedVerificationSpec,
) -> Result<SandboxConfig, TrustedVerifierError> {
  let lifetime = spec
    .timeout_secs
    .saturating_add(SANDBOX_LIFETIME_GRACE_SECS);
  let mut builder = Sandbox::builder(name)
    .image(spec.image.clone())
    .root_disk(spec.resources.writable_root_mib)
    .cpus(spec.resources.vcpus)
    .max_cpus(spec.resources.vcpus)
    .memory(spec.resources.memory_mib)
    .max_memory(spec.resources.memory_mib)
    .disable_network()
    .security(SecurityProfile::Restricted)
    .deployment_profile(DeploymentProfile::SingleTenant)
    .user(GUEST_USER)
    .workdir(GUEST_BOOT_WORKDIR)
    .rlimit(
      RlimitResource::Nproc,
      u64::from(spec.resources.process_limit),
    )
    .ephemeral(true)
    .max_duration(lifetime);
  for (name, value) in &spec.environment {
    builder = builder.env(name, value);
  }
  builder.build().await.map_err(|error| {
    TrustedVerifierError::IsolationUnavailable(format!(
      "construct explicit Microsandbox policy: {error}"
    ))
  })
}

fn validate_requested_config(
  config: &SandboxConfig,
  spec: &TrustedVerificationSpec,
) -> Result<(), TrustedVerifierError> {
  let root_disk_matches = matches!(
    &config.spec.image,
    RootfsSource::Oci(oci)
      if oci.reference == spec.image
        && matches!(
          oci.root_disk,
          Some(RootDisk::Managed { size_mib: Some(size) })
            if size == spec.resources.writable_root_mib
        )
  );
  let process_limit_matches = config.spec.rlimits.iter().any(|limit| {
    limit.resource == RlimitResource::Nproc
      && limit.soft == u64::from(spec.resources.process_limit)
      && limit.hard == u64::from(spec.resources.process_limit)
  });
  let no_input_patches = config.spec.patches.is_empty();
  let network = &config.spec.network;
  let environment_matches = config.spec.env.len() == spec.environment.len()
    && spec.environment.iter().all(|(name, value)| {
      config
        .spec
        .env
        .iter()
        .any(|item| item.key == *name && item.value == *value)
    });
  let exact = root_disk_matches
    && config.spec.resources.cpus == spec.resources.vcpus
    && config.spec.resources.max_cpus == spec.resources.vcpus
    && config.spec.resources.memory_mib == spec.resources.memory_mib
    && config.spec.resources.max_memory_mib == spec.resources.memory_mib
    && !network.enabled
    && network.ports.is_empty()
    && network
      .secrets
      .as_ref()
      .is_none_or(|secrets| secrets.secrets.is_empty())
    && !network.trust_host_cas
    && config.spec.security_profile == SecurityProfile::Restricted
    && config.spec.deployment_profile == DeploymentProfile::SingleTenant
    && config.spec.runtime.user.as_deref() == Some(GUEST_USER)
    && config.spec.runtime.workdir.as_deref() == Some(GUEST_BOOT_WORKDIR)
    && config.spec.mounts.is_empty()
    && config.spec.vsock.is_empty()
    && process_limit_matches
    && config.spec.lifecycle.ephemeral
    && config.spec.lifecycle.max_duration_secs
      == Some(
        spec
          .timeout_secs
          .saturating_add(SANDBOX_LIFETIME_GRACE_SECS),
      )
    && no_input_patches
    && environment_matches;
  if !exact {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "Microsandbox effective request is weaker or different from the trusted policy".into(),
    ));
  }
  Ok(())
}

async fn execute_created_sandbox(
  sandbox: &Sandbox,
  materialized: &MaterializedRevision,
  binding: &RuntimeBinding<'_>,
  spec: &TrustedVerificationSpec,
  obligation_ids: &[ObligationId],
  max_output_bytes: usize,
  cancel: &CancellationToken,
) -> Result<TrustedVerifierExecution, TrustedVerifierError> {
  if sandbox.local().is_none() {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "trusted verifier was not created by the local Microsandbox backend".into(),
    ));
  }
  sandbox
    .ping()
    .await
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
  let metrics = wait_for_live_metrics(sandbox, cancel).await?;
  if metrics.memory_limit_bytes != u64::from(spec.resources.memory_mib) * 1024 * 1024 {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "Microsandbox did not enforce the requested memory limit".into(),
    ));
  }
  let resolved_image_digest = validate_effective_config(sandbox, spec).await?;
  upload_materialized_revision(sandbox, materialized).await?;

  let isolation_report = IsolationCapabilityReport {
    backend: TrustedExecutionBackend::Microsandbox,
    backend_version: format!("microsandbox-rust-sdk/{MICROSANDBOX_SDK_VERSION}"),
    runtime_identity: binding.runtime_identity.into(),
    boundary: IsolationBoundary::HardwareVirtualizedMicroVm,
    image: spec.image.clone(),
    resolved_image_digest: resolved_image_digest.clone(),
    input_revision: binding.revision.into(),
    input_materialization_hash: materialized.archive_hash.clone(),
    input_archive_bytes: materialized.archive_bytes,
    input_tree_bytes: materialized.tree_bytes,
    input_entries: materialized.entries,
    candidate_filesystem: CandidateFilesystemPolicy::PrivateWritable,
    host_repository_mounts: HostRepositoryMountPolicy::None,
    writable_storage: WritableStoragePolicy::DisposableSandboxPrivate,
    network: NetworkPolicy::Disabled,
    environment: EnvironmentPolicy::ExplicitOnly,
    guest_security_profile: GuestSecurityProfile::Restricted,
    guest_user: GUEST_USER.into(),
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
    sandbox_lifetime_secs: spec
      .timeout_secs
      .saturating_add(SANDBOX_LIFETIME_GRACE_SECS),
  };
  if !isolation_report.satisfies(spec) {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "Microsandbox capability report does not satisfy the trusted policy".into(),
    ));
  }

  let started_at = Utc::now();
  let timer = Instant::now();
  let execution = sandbox.exec_stream_with(&spec.program, |command| {
    command
      .args(spec.args.clone())
      .cwd(guest_working_directory(&spec.working_directory))
      .user(GUEST_USER)
      .timeout(Duration::from_secs(spec.timeout_secs))
      .stdin_null()
      .rlimit(
        RlimitResource::Nproc,
        u64::from(spec.resources.process_limit),
      )
  });
  let handle = tokio::select! {
    _ = cancel.cancelled() => return Err(TrustedVerifierError::Cancelled),
    handle = execution => handle.map_err(map_exec_error)?,
  };
  let output =
    collect_bounded_execution(handle, max_output_bytes, spec.timeout_secs, cancel).await?;
  let finished_at = Utc::now();
  let final_image_digest = validate_effective_config(sandbox, spec).await?;
  if final_image_digest != resolved_image_digest {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "Microsandbox resolved image identity changed during execution".into(),
    ));
  }

  let exit_code = output.exit_code;
  let result = classify_exit_code(exit_code)?;
  let record = TrustedExecutionRecord {
    id: VerificationRunId::new(),
    revision: binding.revision.into(),
    input_materialization_hash: materialized.archive_hash.clone(),
    verifier_name: spec.name.clone(),
    spec_hash: spec
      .fingerprint()
      .map_err(|error| TrustedVerifierError::InvalidTrustedVerifierSpec(error.to_string()))?,
    isolation_policy_hash: spec
      .isolation_policy_hash()
      .map_err(|error| TrustedVerifierError::InvalidTrustedVerifierSpec(error.to_string()))?,
    isolation_report: Some(isolation_report),
    started_at,
    finished_at,
    result,
    observation: ExecutionObservation {
      command: spec
        .fingerprint()
        .map_err(|error| TrustedVerifierError::InvalidTrustedVerifierSpec(error.to_string()))?,
      exit_code: Some(exit_code),
      timed_out: false,
      duration_ms: u64::try_from(timer.elapsed().as_millis()).unwrap_or(u64::MAX),
      stdout: bounded_output(&output.stdout, max_output_bytes),
      stderr: bounded_output(&output.stderr, max_output_bytes),
    },
    obligation_ids: obligation_ids.to_vec(),
  };
  if !record.can_issue_authority(spec) {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "trusted execution observation failed authority admission".into(),
    ));
  }
  Ok(if exit_code == 0 {
    TrustedVerifierExecution::Supports(record)
  } else {
    TrustedVerifierExecution::TrustedVerifierObservedContradiction(record)
  })
}

async fn validate_effective_config(
  sandbox: &Sandbox,
  spec: &TrustedVerificationSpec,
) -> Result<String, TrustedVerifierError> {
  if sandbox.local().is_none() {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "trusted verifier effective backend is not local Microsandbox".into(),
    ));
  }

  let image = Image::inspect(&spec.image)
    .await
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
  let resolved_digest = image.handle.manifest_digest().ok_or_else(|| {
    TrustedVerifierError::IsolationUnavailable(
      "Microsandbox did not expose the resolved OCI manifest digest".into(),
    )
  })?;
  if image.handle.reference() != spec.image {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "Microsandbox image cache returned a different verifier reference".into(),
    ));
  }
  let mut expected_environment = image
    .config
    .as_ref()
    .into_iter()
    .flat_map(|config| &config.env)
    .filter_map(|entry| entry.split_once('='))
    .filter(|(name, _)| !spec.environment.contains_key(*name))
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect::<Vec<_>>();
  expected_environment.extend(
    spec
      .environment
      .iter()
      .map(|(name, value)| (name.clone(), value.clone())),
  );
  let actual_environment = sandbox
    .config()
    .spec
    .env
    .iter()
    .map(|item| (item.key.clone(), item.value.clone()))
    .collect::<Vec<_>>();
  let expected_tmpfs_mib = (spec.resources.memory_mib / 4).clamp(1, 512);
  let expected_tmpfs = matches!(
    sandbox.config().spec.mounts.as_slice(),
    [VolumeMount::Tmpfs { guest, size_mib: Some(size), .. }]
      if guest == "/tmp" && *size == expected_tmpfs_mib
  );
  if actual_environment != expected_environment || !expected_tmpfs {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "Microsandbox effective guest environment or writable storage changed unexpectedly".into(),
    ));
  }

  let mut requested_shape = sandbox.config().clone();
  requested_shape
    .spec
    .env
    .retain(|item| spec.environment.get(&item.key) == Some(&item.value));
  requested_shape.spec.mounts.clear();
  validate_requested_config(&requested_shape, spec).map_err(|_| {
    TrustedVerifierError::IsolationUnavailable(
      "Microsandbox effective configuration changed or is weaker than requested".into(),
    )
  })?;
  Ok(resolved_digest.to_owned())
}

enum MaterializedEntry {
  Directory {
    guest: String,
    mode: u32,
  },
  File {
    host: PathBuf,
    guest: String,
    mode: u32,
  },
  Symlink {
    guest: String,
    target: String,
  },
}

async fn upload_materialized_revision(
  sandbox: &Sandbox,
  materialized: &MaterializedRevision,
) -> Result<(), TrustedVerifierError> {
  let entries = collect_materialized_entries(materialized.directory.path())?;
  let fs = sandbox.fs();
  if fs
    .exists(GUEST_WORKSPACE)
    .await
    .map_err(materialization_error)?
  {
    fs.remove_dir(GUEST_WORKSPACE)
      .await
      .map_err(materialization_error)?;
  }
  fs.mkdir(GUEST_WORKSPACE)
    .await
    .map_err(materialization_error)?;

  for entry in &entries {
    if let MaterializedEntry::Directory { guest, .. } = entry {
      fs.mkdir(guest).await.map_err(materialization_error)?;
    }
  }
  let mut buffer = vec![0_u8; 64 * 1024];
  for entry in &entries {
    match entry {
      MaterializedEntry::File { host, guest, mode } => {
        let mut source = tokio::fs::File::open(host)
          .await
          .map_err(materialization_error)?;
        let sink = fs
          .write_stream(guest)
          .await
          .map_err(materialization_error)?;
        loop {
          let read = source
            .read(&mut buffer)
            .await
            .map_err(materialization_error)?;
          if read == 0 {
            break;
          }
          sink
            .write(&buffer[..read])
            .await
            .map_err(materialization_error)?;
        }
        sink.close().await.map_err(materialization_error)?;
        fs.set_stat(guest, false, guest_owned_attrs(Some(*mode)))
          .await
          .map_err(materialization_error)?;
      }
      MaterializedEntry::Symlink { guest, target } => {
        fs.symlink(target, guest)
          .await
          .map_err(materialization_error)?;
        fs.set_stat(guest, false, guest_owned_attrs(None))
          .await
          .map_err(materialization_error)?;
      }
      MaterializedEntry::Directory { .. } => {}
    }
  }
  for entry in entries.iter().rev() {
    if let MaterializedEntry::Directory { guest, mode } = entry {
      fs.set_stat(guest, false, guest_owned_attrs(Some(*mode)))
        .await
        .map_err(materialization_error)?;
    }
  }
  fs.set_stat(GUEST_WORKSPACE, false, guest_owned_attrs(Some(0o755)))
    .await
    .map_err(materialization_error)?;
  if !fs
    .exists(GUEST_WORKSPACE)
    .await
    .map_err(materialization_error)?
  {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "Microsandbox did not retain the private candidate workspace".into(),
    ));
  }
  Ok(())
}

fn collect_materialized_entries(
  root: &Path,
) -> Result<Vec<MaterializedEntry>, TrustedVerifierError> {
  let mut entries = Vec::new();
  let mut pending = vec![root.to_path_buf()];
  while let Some(directory) = pending.pop() {
    let mut children = std::fs::read_dir(&directory)
      .map_err(materialization_error)?
      .collect::<Result<Vec<_>, _>>()
      .map_err(materialization_error)?;
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
      let host = child.path();
      let relative = host.strip_prefix(root).map_err(materialization_error)?;
      let guest = guest_input_path(relative)?;
      let metadata = std::fs::symlink_metadata(&host).map_err(materialization_error)?;
      let file_type = metadata.file_type();
      if file_type.is_dir() {
        entries.push(MaterializedEntry::Directory {
          guest,
          mode: unix_mode(&metadata),
        });
        pending.push(host);
      } else if file_type.is_file() {
        entries.push(MaterializedEntry::File {
          host,
          guest,
          mode: unix_mode(&metadata),
        });
      } else if file_type.is_symlink() {
        let target = std::fs::read_link(&host).map_err(materialization_error)?;
        let target = target.to_str().ok_or_else(|| {
          TrustedVerifierError::IsolationUnavailable(
            "trusted revision contains a non-UTF-8 symlink target unsupported by Microsandbox"
              .into(),
          )
        })?;
        entries.push(MaterializedEntry::Symlink {
          guest,
          target: target.into(),
        });
      } else {
        return Err(TrustedVerifierError::IsolationUnavailable(
          "trusted revision contains an unsupported filesystem entry".into(),
        ));
      }
    }
  }
  Ok(entries)
}

fn guest_input_path(relative: &Path) -> Result<String, TrustedVerifierError> {
  let mut guest = GUEST_WORKSPACE.to_owned();
  for component in relative.components() {
    let std::path::Component::Normal(value) = component else {
      return Err(TrustedVerifierError::IsolationUnavailable(
        "trusted revision export contained an invalid relative path".into(),
      ));
    };
    let value = value.to_str().ok_or_else(|| {
      TrustedVerifierError::IsolationUnavailable(
        "trusted revision contains a non-UTF-8 path unsupported by Microsandbox".into(),
      )
    })?;
    guest.push('/');
    guest.push_str(value);
  }
  Ok(guest)
}

#[cfg(unix)]
fn unix_mode(metadata: &std::fs::Metadata) -> u32 {
  use std::os::unix::fs::PermissionsExt as _;
  metadata.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn unix_mode(metadata: &std::fs::Metadata) -> u32 {
  if metadata.is_dir() {
    0o755
  } else {
    0o644
  }
}

fn guest_owned_attrs(mode: Option<u32>) -> FsSetAttrs {
  FsSetAttrs {
    mode,
    uid: Some(GUEST_UID),
    gid: Some(GUEST_UID),
    ..FsSetAttrs::default()
  }
}

fn materialization_error(error: impl std::fmt::Display) -> TrustedVerifierError {
  TrustedVerifierError::IsolationUnavailable(format!(
    "materialize exact revision in private Microsandbox workspace: {error}"
  ))
}

struct BoundedExecutionOutput {
  exit_code: i32,
  stdout: Vec<u8>,
  stderr: Vec<u8>,
}

async fn collect_bounded_execution(
  mut handle: ExecHandle,
  output_limit: usize,
  timeout_secs: u64,
  cancel: &CancellationToken,
) -> Result<BoundedExecutionOutput, TrustedVerifierError> {
  let mut stdout = Vec::with_capacity(output_limit.min(8 * 1024));
  let mut stderr = Vec::with_capacity(output_limit.min(8 * 1024));
  let deadline = tokio::time::sleep(Duration::from_secs(timeout_secs));
  tokio::pin!(deadline);
  loop {
    let event = tokio::select! {
      _ = cancel.cancelled() => {
        let _ = handle.kill().await;
        return Err(TrustedVerifierError::Cancelled);
      }
      () = &mut deadline => {
        let _ = handle.kill().await;
        return Err(TrustedVerifierError::TrustedVerifierTimeout);
      }
      event = handle.recv() => event,
    };
    match event {
      Some(ExecEvent::Started { .. }) => {}
      Some(ExecEvent::Stdout(bytes)) => append_bounded(&mut stdout, &bytes, output_limit),
      Some(ExecEvent::Stderr(bytes)) => append_bounded(&mut stderr, &bytes, output_limit),
      Some(ExecEvent::Exited { code }) => {
        return Ok(BoundedExecutionOutput {
          exit_code: code,
          stdout,
          stderr,
        });
      }
      Some(ExecEvent::Failed(error)) => {
        return Err(map_exec_error(MicrosandboxError::ExecFailed(error)));
      }
      Some(ExecEvent::StdinError(error)) => {
        return Err(TrustedVerifierError::TrustedVerifierInfrastructureFailure(
          format!("trusted verifier stdin failed unexpectedly: {error:?}"),
        ));
      }
      None => {
        return Err(TrustedVerifierError::TrustedVerifierInfrastructureFailure(
          "trusted verifier exec stream ended without an exit status".into(),
        ));
      }
    }
  }
}

fn append_bounded(output: &mut Vec<u8>, bytes: &[u8], limit: usize) {
  let remaining = limit.saturating_sub(output.len());
  output.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
}

fn classify_exit_code(exit_code: i32) -> Result<TrustedExecutionResult, TrustedVerifierError> {
  match exit_code {
    0 => Ok(TrustedExecutionResult::Supports),
    TRUSTED_VERIFIER_CONTRADICTION_EXIT_CODE => {
      Ok(TrustedExecutionResult::Contradicts { exit_code })
    }
    other => Err(TrustedVerifierError::TrustedVerifierInfrastructureFailure(
      format!("trusted verifier terminated with non-semantic status {other}"),
    )),
  }
}

fn map_exec_error(error: MicrosandboxError) -> TrustedVerifierError {
  match error {
    MicrosandboxError::ExecTimeout(_) => TrustedVerifierError::TrustedVerifierTimeout,
    other => TrustedVerifierError::TrustedVerifierInfrastructureFailure(other.to_string()),
  }
}

async fn local_runtime_identity() -> Result<String, TrustedVerifierError> {
  let msb = microsandbox::config::resolve_msb_path()
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
  let libkrunfw = microsandbox::config::resolve_libkrunfw_path()
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
  let msb_digest = sha256_file(&msb).await?;
  let libkrunfw_digest = sha256_file(&libkrunfw).await?;
  Ok(format!(
    "local-msb-sha256:{msb_digest}/libkrunfw-sha256:{libkrunfw_digest}"
  ))
}

async fn sha256_file(path: &Path) -> Result<String, TrustedVerifierError> {
  let mut file = tokio::fs::File::open(path).await.map_err(|error| {
    TrustedVerifierError::IsolationUnavailable(format!(
      "open local Microsandbox runtime file {}: {error}",
      path.display()
    ))
  })?;
  let mut digest = Sha256::new();
  let mut buffer = vec![0_u8; 64 * 1024];
  loop {
    let read = file.read(&mut buffer).await.map_err(|error| {
      TrustedVerifierError::IsolationUnavailable(format!(
        "hash local Microsandbox runtime file {}: {error}",
        path.display()
      ))
    })?;
    if read == 0 {
      break;
    }
    digest.update(&buffer[..read]);
  }
  Ok(hex_digest(&digest.finalize()))
}

async fn wait_for_live_metrics(
  sandbox: &Sandbox,
  cancel: &CancellationToken,
) -> Result<microsandbox::sandbox::SandboxMetrics, TrustedVerifierError> {
  let deadline = tokio::time::Instant::now() + Duration::from_secs(METRICS_READINESS_TIMEOUT_SECS);
  loop {
    match sandbox.metrics().await {
      Ok(metrics) => return Ok(metrics),
      Err(error)
        if (matches!(error, MicrosandboxError::MetricsUnavailable(_))
          || error.to_string().contains("has no live metrics slot"))
          && tokio::time::Instant::now() < deadline => {}
      Err(error) => {
        return Err(TrustedVerifierError::IsolationUnavailable(
          error.to_string(),
        ));
      }
    }
    tokio::select! {
      _ = cancel.cancelled() => return Err(TrustedVerifierError::Cancelled),
      () = tokio::time::sleep(Duration::from_millis(100)) => {}
    }
  }
}

async fn sandbox_request<T>(
  future: impl Future<Output = Result<T, MicrosandboxError>>,
  timeout_secs: u64,
  cancel: &CancellationToken,
) -> Result<T, TrustedVerifierError> {
  tokio::select! {
    _ = cancel.cancelled() => Err(TrustedVerifierError::Cancelled),
    result = tokio::time::timeout(Duration::from_secs(timeout_secs), future) => {
      match result {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(TrustedVerifierError::TrustedVerifierInfrastructureFailure(error.to_string())),
        Err(_) => Err(TrustedVerifierError::TrustedVerifierTimeout),
      }
    }
  }
}

async fn cleanup_sandbox(sandbox: &Sandbox) -> Result<(), TrustedVerifierError> {
  sandbox
    .stop_with_timeout(Duration::from_secs(CLEANUP_TIMEOUT_SECS))
    .await
    .map_err(|error| {
      TrustedVerifierError::TrustedVerifierInfrastructureFailure(format!(
        "Microsandbox cleanup failed to stop sandbox: {error}"
      ))
    })?;
  match sandbox.remove_persisted().await {
    Ok(()) | Err(MicrosandboxError::SandboxNotFound(_)) => {}
    Err(error) => {
      return Err(TrustedVerifierError::TrustedVerifierInfrastructureFailure(
        format!("Microsandbox cleanup failed to remove sandbox: {error}"),
      ));
    }
  }
  ensure_sandbox_removed(sandbox.name()).await
}

async fn cleanup_named_sandbox(name: &str) -> Result<(), TrustedVerifierError> {
  let sandbox = match Sandbox::get(name).await {
    Ok(handle) => handle.start().await.map_err(|error| {
      TrustedVerifierError::TrustedVerifierInfrastructureFailure(format!(
        "recover partially-created Microsandbox for cleanup: {error}"
      ))
    })?,
    Err(MicrosandboxError::SandboxNotFound(_)) => return Ok(()),
    Err(error) => {
      return Err(TrustedVerifierError::TrustedVerifierInfrastructureFailure(
        format!("inspect partially-created Microsandbox for cleanup: {error}"),
      ));
    }
  };
  cleanup_sandbox(&sandbox).await
}

async fn ensure_sandbox_removed(name: &str) -> Result<(), TrustedVerifierError> {
  match Sandbox::get(name).await {
    Err(MicrosandboxError::SandboxNotFound(missing)) if missing == name => Ok(()),
    Ok(_) => Err(TrustedVerifierError::TrustedVerifierInfrastructureFailure(
      "Microsandbox cleanup left disposable sandbox state behind".into(),
    )),
    Err(error) => Err(TrustedVerifierError::TrustedVerifierInfrastructureFailure(
      format!("Microsandbox cleanup could not verify removal: {error}"),
    )),
  }
}

fn guest_working_directory(relative: &str) -> String {
  if relative == "." {
    GUEST_WORKSPACE.into()
  } else {
    format!("{GUEST_WORKSPACE}/{relative}")
  }
}

fn bounded_output(bytes: &[u8], limit: usize) -> String {
  String::from_utf8_lossy(&bytes[..bytes.len().min(limit)]).into_owned()
}

fn hex_digest(bytes: &[u8]) -> String {
  let mut value = String::with_capacity(bytes.len() * 2);
  for byte in bytes {
    use std::fmt::Write as _;
    write!(value, "{byte:02x}").expect("writing to String cannot fail");
  }
  value
}

pub struct TrustedVerifierRequest<'a> {
  pub repository: &'a Path,
  pub workspaces: &'a WorkspaceManager,
  pub revision: &'a str,
  pub spec: &'a TrustedVerificationSpec,
  pub obligation_ids: &'a [ObligationId],
  pub max_output_bytes: usize,
  pub runner: &'a dyn TrustedVerifierRunner,
  pub cancel: &'a CancellationToken,
}

pub async fn run_isolated_trusted_verifier(
  request: TrustedVerifierRequest<'_>,
) -> Result<TrustedVerifierExecution, TrustedVerifierError> {
  let canonical_before = git::repository_state(request.repository)
    .await
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
  let candidate = request
    .workspaces
    .create_disposable(
      &format!("trusted-verifier-{}", request.spec.name),
      request.revision,
    )
    .await
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
  let observed_revision = git::head(&candidate)
    .await
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
  if observed_revision != request.revision {
    let _ = request.workspaces.remove(&candidate).await;
    return Err(TrustedVerifierError::IsolationUnavailable(format!(
      "immutable candidate revision {observed_revision} does not match requested {}",
      request.revision
    )));
  }
  let execution = request
    .runner
    .execute(
      &candidate,
      request.revision,
      request.spec,
      request.obligation_ids,
      request.max_output_bytes,
      request.cancel,
    )
    .await;
  let cleanup = request
    .workspaces
    .remove(&candidate)
    .await
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()));
  let canonical_after = git::repository_state(request.repository)
    .await
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()));
  cleanup?;
  if canonical_after? != canonical_before {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "trusted verifier execution changed canonical repository state".into(),
    ));
  }

  execution
}

#[cfg(test)]
mod tests {
  use super::*;

  fn spec() -> TrustedVerificationSpec {
    TrustedVerificationSpec {
      name: "boundary".into(),
      backend: TrustedExecutionBackend::Microsandbox,
      image: format!("example/verifier@sha256:{}", "a".repeat(64)),
      program: "verify".into(),
      args: vec!["--boundary".into()],
      working_directory: ".".into(),
      environment: [("CI".into(), "true".into())].into(),
      timeout_secs: 30,
      isolation: Default::default(),
      resources: Default::default(),
      protocol: Default::default(),
    }
  }

  #[tokio::test]
  async fn unavailable_runtime_fails_without_execution_record() {
    let candidate = tempfile::tempdir().expect("candidate");
    let runner = MicrosandboxTrustedVerifier::unavailable("virtualization unavailable");

    let error = runner
      .execute(
        candidate.path(),
        "revision",
        &spec(),
        &[ObligationId::from("VO-1")],
        1024,
        &CancellationToken::new(),
      )
      .await
      .expect_err("unavailable runtime must fail closed");

    assert!(matches!(
      error,
      TrustedVerifierError::TrustedVerifierUnavailable(_)
    ));
  }

  #[test]
  fn bounded_output_never_exceeds_requested_input_bytes() {
    assert_eq!(bounded_output("€x".as_bytes(), 2), "�");
  }

  #[test]
  fn streamed_output_is_bounded_while_draining_chunks() {
    let mut output = Vec::new();
    append_bounded(&mut output, b"abc", 4);
    append_bounded(&mut output, b"def", 4);
    append_bounded(&mut output, b"ghi", 4);
    assert_eq!(output, b"abcd");
  }

  #[test]
  fn only_exit_one_is_a_semantic_contradiction() {
    assert!(matches!(
      classify_exit_code(0),
      Ok(TrustedExecutionResult::Supports)
    ));
    assert!(matches!(
      classify_exit_code(1),
      Ok(TrustedExecutionResult::Contradicts { exit_code: 1 })
    ));
    assert!(matches!(
      classify_exit_code(2),
      Err(TrustedVerifierError::TrustedVerifierInfrastructureFailure(
        _
      ))
    ));
    assert!(matches!(
      classify_exit_code(137),
      Err(TrustedVerifierError::TrustedVerifierInfrastructureFailure(
        _
      ))
    ));
  }

  #[test]
  fn guest_working_directory_stays_inside_private_workspace() {
    assert_eq!(
      guest_working_directory("crates/domain"),
      "/workspace/crates/domain"
    );
  }

  #[test]
  fn sdk_timeout_is_infrastructure_not_semantic_contradiction() {
    let error = map_exec_error(MicrosandboxError::ExecTimeout(Duration::from_secs(1)));
    assert!(matches!(
      error,
      TrustedVerifierError::TrustedVerifierTimeout
    ));
  }
  #[cfg(unix)]
  fn run_git(repository: &Path, arguments: &[&str]) {
    let output = std::process::Command::new("git")
      .arg("-C")
      .arg(repository)
      .args(arguments)
      .output()
      .expect("run git for Microsandbox acceptance fixture");
    assert!(
      output.status.success(),
      "git {:?} failed: {}",
      arguments,
      String::from_utf8_lossy(&output.stderr)
    );
  }

  #[cfg(unix)]
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  #[ignore = "requires local Microsandbox, hardware virtualization, and OCI registry access"]
  async fn local_microsandbox_acceptance_establishes_the_real_boundary() {
    use std::os::unix::fs::PermissionsExt as _;

    assert_eq!(
      std::env::var("TENET_MICROSANDBOX_PLANTED_SECRET").as_deref(),
      Ok("must-not-enter-guest"),
      "the acceptance command must plant the host-only canary"
    );
    let image_digest = match std::env::consts::ARCH {
      "aarch64" => "sha256:2c9d26f410d032d5b1525aa8a873e238b05b90c4ae8618743d4311f0cc827e37",
      "x86_64" => "sha256:7c8cb692ae09657cbc4a3f3cbd0e8d5a2690ba38386aaaf252dbb060bf5eb2e6",
      architecture => panic!("no pinned acceptance image for {architecture}"),
    };
    let repository = tempfile::tempdir().expect("acceptance repository");
    run_git(repository.path(), &["init", "-q"]);
    run_git(
      repository.path(),
      &["config", "user.email", "tenet@example.invalid"],
    );
    run_git(
      repository.path(),
      &["config", "user.name", "Tenet Acceptance"],
    );
    std::fs::write(
      repository.path().join("input.txt"),
      "exact-revision-input\n",
    )
    .expect("write exact revision input");
    let verifier = repository.path().join("verify-boundary");
    std::fs::write(
      &verifier,
      concat!(
        "#!/bin/sh\n",
        "set -eu\n",
        "test \"$(cat input.txt)\" = exact-revision-input\n",
        "test \"$(id -u)\" = 65532\n",
        "test \"$TENET_ACCEPTANCE_EXPLICIT\" = present\n",
        "test -z \"${TENET_MICROSANDBOX_PLANTED_SECRET+x}\"\n",
        "case \"${TENET_ACCEPTANCE_MODE:-supports}\" in\n",
        "  contradict) exit 1 ;;\n",
        "  timeout) sleep 5 ;;\n",
        "esac\n",
        "if wget -q -T 2 -O /dev/null http://1.1.1.1; then\n",
        "  exit 21\n",
        "fi\n",
        "printf '%s\\n' guest-private > guest-created.txt\n",
        "printf '%s\\n' microsandbox-acceptance-ok\n",
      ),
    )
    .expect("write boundary verifier");
    let mut permissions = std::fs::metadata(&verifier)
      .expect("verifier metadata")
      .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&verifier, permissions).expect("make verifier executable");
    run_git(repository.path(), &["add", "input.txt", "verify-boundary"]);
    run_git(
      repository.path(),
      &["commit", "-q", "-m", "acceptance fixture"],
    );
    let revision = git::head(repository.path())
      .await
      .expect("fixture revision");
    let resources = TrustedResourcePolicy {
      memory_mib: 512,
      vcpus: 1,
      process_limit: 64,
      writable_root_mib: 512,
      ..TrustedResourcePolicy::default()
    };
    let (raw_archive, raw_archive_bytes) = git::archive(
      repository.path(),
      &revision,
      resources.max_input_archive_bytes,
    )
    .await
    .expect("raw fixture archive");
    assert_eq!(
      raw_archive.metadata().expect("archive metadata").len(),
      raw_archive_bytes
    );
    let mut raw_tar = tar::Archive::new(raw_archive);
    assert!(raw_tar.entries().expect("archive entries").next().is_some());
    let exported = materialize_revision(repository.path(), &revision, &resources)
      .await
      .expect("materialize fixture revision");
    assert!(
      exported.directory.path().join("verify-boundary").exists(),
      "exported entries: {:?}",
      std::fs::read_dir(exported.directory.path())
        .expect("read exported tree")
        .map(|entry| entry.expect("exported entry").file_name())
        .collect::<Vec<_>>()
    );
    assert!(exported.directory.path().join("input.txt").exists());
    let spec = TrustedVerificationSpec {
      name: "microsandbox-real-runtime".into(),
      backend: TrustedExecutionBackend::Microsandbox,
      image: format!("docker.io/library/alpine@{image_digest}"),
      program: "/bin/sh".into(),
      args: vec!["/workspace/verify-boundary".into()],
      working_directory: ".".into(),
      environment: [("TENET_ACCEPTANCE_EXPLICIT".into(), "present".into())].into(),
      timeout_secs: 120,
      isolation: Default::default(),
      resources,
      protocol: Default::default(),
    };
    let workspaces = WorkspaceManager::new(repository.path().to_path_buf(), "msb-acceptance");
    let runner = MicrosandboxTrustedVerifier::default();
    let execution = run_isolated_trusted_verifier(TrustedVerifierRequest {
      repository: repository.path(),
      workspaces: &workspaces,
      revision: &revision,
      spec: &spec,
      obligation_ids: &[ObligationId::from("VO-MICROSANDBOX-ACCEPTANCE")],
      max_output_bytes: 4096,
      runner: &runner,
      cancel: &CancellationToken::new(),
    })
    .await
    .expect("real Microsandbox execution");
    let record = execution.record();
    assert!(
      matches!(execution, TrustedVerifierExecution::Supports(_)),
      "unexpected verifier result: {:?}",
      record.observation
    );
    assert!(record.can_issue_authority(&spec));
    assert!(record
      .observation
      .stdout
      .contains("microsandbox-acceptance-ok"));
    let report = record.isolation_report.as_ref().expect("capability report");
    assert_eq!(report.resolved_image_digest, image_digest);
    assert!(report.runtime_identity.starts_with("local-msb-sha256:"));
    assert!(report.runtime_identity.contains("/libkrunfw-sha256:"));
    assert!(!repository.path().join("guest-created.txt").exists());

    let mut contradiction_spec = spec.clone();
    contradiction_spec
      .environment
      .insert("TENET_ACCEPTANCE_MODE".into(), "contradict".into());
    let contradiction = run_isolated_trusted_verifier(TrustedVerifierRequest {
      repository: repository.path(),
      workspaces: &workspaces,
      revision: &revision,
      spec: &contradiction_spec,
      obligation_ids: &[ObligationId::from("VO-MICROSANDBOX-CONTRADICTION")],
      max_output_bytes: 4096,
      runner: &runner,
      cancel: &CancellationToken::new(),
    })
    .await
    .expect("real Microsandbox semantic contradiction");
    assert!(matches!(
      &contradiction,
      TrustedVerifierExecution::TrustedVerifierObservedContradiction(_)
    ));
    assert!(contradiction
      .record()
      .can_issue_authority(&contradiction_spec));

    let mut timeout_spec = spec.clone();
    timeout_spec.timeout_secs = 2;
    timeout_spec
      .environment
      .insert("TENET_ACCEPTANCE_MODE".into(), "timeout".into());
    let timeout = run_isolated_trusted_verifier(TrustedVerifierRequest {
      repository: repository.path(),
      workspaces: &workspaces,
      revision: &revision,
      spec: &timeout_spec,
      obligation_ids: &[ObligationId::from("VO-MICROSANDBOX-TIMEOUT")],
      max_output_bytes: 4096,
      runner: &runner,
      cancel: &CancellationToken::new(),
    })
    .await
    .expect_err("real Microsandbox timeout must not return a semantic record");
    assert!(matches!(
      timeout,
      TrustedVerifierError::TrustedVerifierTimeout
    ));
    assert_eq!(
      git::head(repository.path())
        .await
        .expect("final revision after failures"),
      revision
    );
  }
}
