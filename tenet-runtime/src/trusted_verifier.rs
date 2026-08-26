use std::{
  collections::{BTreeMap, BTreeSet, HashMap},
  future::Future,
  io::Read,
  os::fd::{FromRawFd, RawFd},
  path::Path,
  sync::Arc,
  time::Instant,
};

use async_trait::async_trait;
use bollard::{
  body_try_stream,
  container::LogOutput,
  models::{ContainerCreateBody, HostConfig, HostConfigLogConfig},
  query_parameters::{
    AttachContainerOptionsBuilder, BuildImageOptionsBuilder, RemoveContainerOptionsBuilder,
    RemoveImageOptionsBuilder, WaitContainerOptionsBuilder,
  },
  BollardRequest, Docker, API_DEFAULT_VERSION,
};
use chrono::Utc;
use futures_util::StreamExt;
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::time::Duration;
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use tenet_domain::{
  ids::{ObligationId, VerificationRunId},
  proof::ExecutionObservation,
  trusted_verifier::{
    CandidateFilesystemPolicy, ControlPlanePolicy, EnvironmentPolicy, NetworkPolicy,
    ProcessNamespacePolicy, RootFilesystemPolicy, TemporaryFilesystemPolicy,
    TrustedExecutionAttestation, TrustedExecutionRecord, TrustedExecutionResult,
    TrustedVerificationSpec, TrustedVerifierBackend,
  },
};

use crate::{git, workspace::WorkspaceManager};

const CONTAINER_WORKSPACE: &str = "/workspace";
const CONTAINER_TMP: &str = "/tmp";
const CONTAINER_USER: &str = "65532:65532";
const DOCKER_HOST_ENV: &str = "TENET_TRUSTED_DOCKER_HOST";
const DOCKER_CA_FD_ENV: &str = "TENET_TRUSTED_DOCKER_CA_FD";
const DOCKER_CERT_FD_ENV: &str = "TENET_TRUSTED_DOCKER_CERT_FD";
const DOCKER_KEY_FD_ENV: &str = "TENET_TRUSTED_DOCKER_KEY_FD";
const AUTHORITY_NAMESPACE_ENV: &str = "TENET_TRUSTED_AUTHORITY_NAMESPACE";
const MAX_CREDENTIAL_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
struct ExclusiveDockerClient {
  docker: Docker,
  control_plane_fingerprint: String,
}

fn load_exclusive_docker_client() -> Result<ExclusiveDockerClient, String> {
  let host_result = std::env::var(DOCKER_HOST_ENV)
    .map_err(|_| format!("{DOCKER_HOST_ENV} is required for trusted verification"));
  std::env::remove_var(DOCKER_HOST_ENV);
  let authority_namespace_result = std::env::var(AUTHORITY_NAMESPACE_ENV)
    .map_err(|_| format!("{AUTHORITY_NAMESPACE_ENV} is required for trusted verification"))
    .and_then(|value| {
      if value.trim().is_empty() {
        Err(format!("{AUTHORITY_NAMESPACE_ENV} cannot be empty"))
      } else {
        Ok(value)
      }
    });
  std::env::remove_var(AUTHORITY_NAMESPACE_ENV);
  let ca_fd = take_credential_fd(DOCKER_CA_FD_ENV);
  let cert_fd = take_credential_fd(DOCKER_CERT_FD_ENV);
  let key_fd = take_credential_fd(DOCKER_KEY_FD_ENV);
  let mut consumed = BTreeSet::new();
  let ca_result = consume_credential_fd(ca_fd, "CA certificate", &mut consumed);
  let cert_result = consume_credential_fd(cert_fd, "client certificate", &mut consumed);
  let key_result = consume_credential_fd(key_fd, "client private key", &mut consumed);

  let host = host_result?;
  let authority_namespace = authority_namespace_result?;
  let ca = ca_result?;
  let cert = cert_result?;
  let key = key_result?;
  let uri: hyper::Uri = host
    .parse()
    .map_err(|error| format!("invalid {DOCKER_HOST_ENV}: {error}"))?;
  if uri.scheme_str() != Some("https")
    || uri.authority().is_none()
    || uri
      .path_and_query()
      .is_some_and(|path| path.as_str() != "/")
  {
    return Err(format!(
      "{DOCKER_HOST_ENV} must be an HTTPS origin without a path or query"
    ));
  }

  let control_plane_fingerprint =
    control_plane_fingerprint(&host, &ca, &cert, &authority_namespace);
  let authorities: Vec<_> = CertificateDer::pem_slice_iter(&ca)
    .collect::<Result<_, _>>()
    .map_err(|error| format!("invalid trusted Docker CA certificate: {error}"))?;
  let client_certificates: Vec<_> = CertificateDer::pem_slice_iter(&cert)
    .collect::<Result<_, _>>()
    .map_err(|error| format!("invalid trusted Docker client certificate: {error}"))?;
  let private_key = PrivateKeyDer::from_pem_slice(&key)
    .map_err(|error| format!("invalid trusted Docker client private key: {error}"))?;
  tenet_storage::install_controller_authority_key(&authority_namespace, private_key.secret_der())
    .map_err(|error| format!("install controller authority identity: {error}"))?;
  if authorities.is_empty() || client_certificates.is_empty() {
    return Err("trusted Docker mTLS certificate chain cannot be empty".into());
  }
  let mut roots = RootCertStore::empty();
  let (accepted, rejected) = roots.add_parsable_certificates(authorities);
  if accepted == 0 || rejected != 0 {
    return Err("trusted Docker CA bundle contains an invalid certificate".into());
  }
  let tls = ClientConfig::builder()
    .with_root_certificates(roots)
    .with_client_auth_cert(client_certificates, private_key)
    .map_err(|error| format!("invalid trusted Docker mTLS identity: {error}"))?;
  let connector = HttpsConnectorBuilder::new()
    .with_tls_config(tls)
    .https_only()
    .enable_http1()
    .build();
  let client = Arc::new(Client::builder(TokioExecutor::new()).build(connector));
  let docker = Docker::connect_with_custom_transport(
    move |request: BollardRequest| {
      let client = Arc::clone(&client);
      Box::pin(async move {
        client
          .request(request)
          .await
          .map_err(bollard::errors::Error::from)
      })
    },
    Some(host),
    120,
    API_DEFAULT_VERSION,
  )
  .map_err(|error| format!("construct trusted Docker API client: {error}"))?;
  Ok(ExclusiveDockerClient {
    docker,
    control_plane_fingerprint,
  })
}

fn take_credential_fd(name: &str) -> Result<RawFd, String> {
  let value = std::env::var(name).map_err(|_| format!("{name} is required"))?;
  std::env::remove_var(name);
  let fd = value
    .parse::<RawFd>()
    .map_err(|_| format!("{name} must name an inherited file descriptor"))?;
  if fd <= 2 {
    return Err(format!("{name} cannot use a standard I/O descriptor"));
  }
  Ok(fd)
}

fn consume_credential_fd(
  fd: Result<RawFd, String>,
  description: &str,
  consumed: &mut BTreeSet<RawFd>,
) -> Result<Vec<u8>, String> {
  let fd = fd?;
  if !consumed.insert(fd) {
    return Err("trusted Docker credential descriptors must be distinct".into());
  }
  read_credential_fd(fd, description)
}

fn read_credential_fd(fd: RawFd, description: &str) -> Result<Vec<u8>, String> {
  // SAFETY: this function takes exclusive ownership of a distinct inherited descriptor.
  let file = unsafe { std::fs::File::from_raw_fd(fd) };
  let mut value = Vec::new();
  file
    .take(MAX_CREDENTIAL_BYTES + 1)
    .read_to_end(&mut value)
    .map_err(|error| format!("read trusted Docker {description}: {error}"))?;
  if value.is_empty() || value.len() as u64 > MAX_CREDENTIAL_BYTES {
    return Err(format!(
      "trusted Docker {description} is empty or exceeds {MAX_CREDENTIAL_BYTES} bytes"
    ));
  }
  Ok(value)
}

fn control_plane_fingerprint(
  host: &str,
  ca: &[u8],
  certificate: &[u8],
  authority_namespace: &str,
) -> String {
  let mut digest = Sha256::new();
  digest.update(b"tenet-exclusive-docker-mtls-v2");
  for value in [
    host.as_bytes(),
    ca,
    certificate,
    authority_namespace.as_bytes(),
  ] {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
  }
  digest
    .finalize()
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect()
}

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
pub struct DockerTrustedVerifier {
  client: Result<ExclusiveDockerClient, String>,
}

impl Default for DockerTrustedVerifier {
  fn default() -> Self {
    Self {
      client: load_exclusive_docker_client(),
    }
  }
}

impl DockerTrustedVerifier {
  #[cfg(test)]
  fn unavailable(message: impl Into<String>) -> Self {
    Self {
      client: Err(message.into()),
    }
  }

  fn client(&self) -> Result<&Docker, TrustedVerifierError> {
    self
      .client
      .as_ref()
      .map(|client| &client.docker)
      .map_err(|error| TrustedVerifierError::TrustedVerifierUnavailable(error.clone()))
  }

  fn control_plane_fingerprint(&self) -> Result<&str, TrustedVerifierError> {
    self
      .client
      .as_ref()
      .map(|client| client.control_plane_fingerprint.as_str())
      .map_err(|error| TrustedVerifierError::TrustedVerifierUnavailable(error.clone()))
  }
}

#[async_trait]
impl TrustedVerifierRunner for DockerTrustedVerifier {
  async fn execute(
    &self,
    candidate: &Path,
    revision: &str,
    spec: &TrustedVerificationSpec,
    obligation_ids: &[ObligationId],
    max_output_bytes: usize,
    cancel: &CancellationToken,
  ) -> Result<TrustedVerifierExecution, TrustedVerifierError> {
    spec
      .validate()
      .map_err(|error| TrustedVerifierError::InvalidTrustedVerifierSpec(error.to_string()))?;
    if spec.backend != TrustedVerifierBackend::Docker {
      return Err(TrustedVerifierError::InvalidTrustedVerifierSpec(
        "unsupported trusted verifier backend".into(),
      ));
    }
    let candidate = candidate.canonicalize().map_err(|error| {
      TrustedVerifierError::IsolationUnavailable(format!(
        "canonicalize immutable candidate input: {error}"
      ))
    })?;

    let backend_version = self.backend_version(spec, cancel).await?;
    self.admit_base_image(spec, cancel).await?;
    let snapshot = self
      .build_snapshot_image(&candidate, revision, spec, max_output_bytes, cancel)
      .await?;
    let body = container_create_body(&snapshot, spec)?;
    let created = match self
      .request(
        self.client()?.create_container(None, body),
        spec.timeout_secs,
        cancel,
      )
      .await
    {
      Ok(created) => created,
      Err(error) => {
        self.remove_snapshot_image(&snapshot, spec).await?;
        return Err(error);
      }
    };
    if created.id.is_empty() || !created.warnings.is_empty() {
      self.remove_snapshot_image(&snapshot, spec).await?;
      return Err(TrustedVerifierError::TrustedVerifierInfrastructureFailure(
        format!(
          "Docker container creation was ambiguous: {:?}",
          created.warnings
        ),
      ));
    }
    let container_id = created.id;
    let outcome = self
      .execute_created_container(
        &container_id,
        &snapshot,
        revision,
        spec,
        obligation_ids,
        max_output_bytes,
        &backend_version,
        cancel,
      )
      .await;
    let remove_options = RemoveContainerOptionsBuilder::default()
      .force(true)
      .v(true)
      .build();
    let cleanup_cancel = CancellationToken::new();
    let container_cleanup = self
      .request(
        self
          .client()?
          .remove_container(&container_id, Some(remove_options)),
        spec.timeout_secs,
        &cleanup_cancel,
      )
      .await;
    let image_cleanup = self.remove_snapshot_image(&snapshot, spec).await;
    match (outcome, container_cleanup, image_cleanup) {
      (Ok(execution), Ok(()), Ok(())) => Ok(execution),
      (Ok(_), Err(error), _) | (Ok(_), _, Err(error)) | (Err(error), _, _) => Err(error),
    }
  }
}

impl DockerTrustedVerifier {
  async fn request<T>(
    &self,
    future: impl Future<Output = Result<T, bollard::errors::Error>>,
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
  async fn remove_snapshot_image(
    &self,
    snapshot: &str,
    spec: &TrustedVerificationSpec,
  ) -> Result<(), TrustedVerifierError> {
    let options = RemoveImageOptionsBuilder::default()
      .force(true)
      .noprune(false)
      .build();
    self
      .request(
        self.client()?.remove_image(snapshot, Some(options), None),
        spec.timeout_secs,
        &CancellationToken::new(),
      )
      .await?;
    Ok(())
  }

  async fn admit_base_image(
    &self,
    spec: &TrustedVerificationSpec,
    cancel: &CancellationToken,
  ) -> Result<(), TrustedVerifierError> {
    let observed = self
      .request(
        self.client()?.inspect_image(&spec.image),
        spec.timeout_secs,
        cancel,
      )
      .await?;
    let image: DockerImageInspect = serde_json::from_value(
      serde_json::to_value(observed)
        .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?,
    )
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
    if !image
      .repo_digests
      .iter()
      .any(|digest| digest == &spec.image)
      || image
        .config
        .on_build
        .as_ref()
        .is_some_and(|steps| !steps.is_empty())
    {
      return Err(TrustedVerifierError::IsolationUnavailable(
        "trusted verifier image identity is ambiguous or contains ONBUILD execution".into(),
      ));
    }
    Ok(())
  }

  async fn build_snapshot_image(
    &self,
    candidate: &Path,
    revision: &str,
    spec: &TrustedVerificationSpec,
    max_output_bytes: usize,
    cancel: &CancellationToken,
  ) -> Result<String, TrustedVerifierError> {
    if git::has_gitlinks(candidate, revision)
      .await
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?
    {
      return Err(TrustedVerifierError::IsolationUnavailable(
        "trusted snapshot materialization does not admit Git submodules".into(),
      ));
    }
    let temporary = tempfile::tempdir().map_err(|error| {
      TrustedVerifierError::IsolationUnavailable(format!(
        "create trusted snapshot repository: {error}"
      ))
    })?;
    let snapshot = temporary.path().join("snapshot");
    git::clone_without_checkout(candidate, &snapshot)
      .await
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
    git::checkout_detached(&snapshot, revision)
      .await
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
    if git::head(&snapshot)
      .await
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?
      != revision
    {
      return Err(TrustedVerifierError::IsolationUnavailable(
        "trusted snapshot clone did not resolve the requested revision".into(),
      ));
    }

    let (dockerfile_name, ignore_name) = loop {
      let identity = Uuid::new_v4().simple().to_string();
      let dockerfile_name = format!(".tenet-trusted-{identity}.Dockerfile");
      let ignore_name = format!("{dockerfile_name}.dockerignore");
      let dockerfile_exists = git::path_exists(&snapshot, revision, &dockerfile_name)
        .await
        .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
      let ignore_exists = git::path_exists(&snapshot, revision, &ignore_name)
        .await
        .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
      if !dockerfile_exists && !ignore_exists {
        break (dockerfile_name, ignore_name);
      }
    };
    let dockerfile = format!("FROM {}\nCOPY . /workspace\n", spec.image);
    let ignore = format!("{dockerfile_name}\n{ignore_name}\n");
    tokio::fs::write(snapshot.join(&dockerfile_name), &dockerfile)
      .await
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
    tokio::fs::write(snapshot.join(&ignore_name), &ignore)
      .await
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
    let derived_revision = git::commit_all(&snapshot, "materialize trusted verifier snapshot")
      .await
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
    let parent = git::parent(&snapshot, &derived_revision)
      .await
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
    let changes = git::changed_paths(&snapshot, revision, &derived_revision)
      .await
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
    if parent != revision || changes != [dockerfile_name.clone(), ignore_name.clone()] {
      return Err(TrustedVerifierError::IsolationUnavailable(
        "trusted snapshot derivation included candidate changes".into(),
      ));
    }
    let persisted_dockerfile = git::read_blob(&snapshot, &derived_revision, &dockerfile_name)
      .await
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
    let persisted_ignore = git::read_blob(&snapshot, &derived_revision, &ignore_name)
      .await
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
    if persisted_dockerfile != dockerfile.trim() || persisted_ignore != ignore.trim() {
      return Err(TrustedVerifierError::IsolationUnavailable(
        "trusted snapshot build definition changed before admission".into(),
      ));
    }

    let archive = git::archive(&snapshot, &derived_revision)
      .await
      .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
    let body = body_try_stream(ReaderStream::new(tokio::fs::File::from_std(archive)));
    let options = BuildImageOptionsBuilder::default()
      .dockerfile(&dockerfile_name)
      .q(true)
      .nocache(true)
      .pull("false")
      .rm(true)
      .forcerm(true)
      .networkmode("none")
      .build();
    let mut stream = self.client()?.build_image(options, None, Some(body));
    let build = async {
      let mut image_id = None;
      let mut diagnostic = String::new();
      while let Some(message) = stream.next().await {
        let message = message.map_err(|error| {
          TrustedVerifierError::TrustedVerifierInfrastructureFailure(error.to_string())
        })?;
        if let Some(error) = message.error_detail {
          return Err(TrustedVerifierError::TrustedVerifierInfrastructureFailure(
            error
              .message
              .unwrap_or_else(|| "Docker snapshot build failed".into()),
          ));
        }
        if let Some(aux) = message.aux.and_then(|aux| aux.id) {
          image_id = Some(aux);
        }
        if let Some(output) = message.stream {
          append_limited(&mut diagnostic, output.as_bytes(), max_output_bytes);
          if let Some(found) = output
            .split_whitespace()
            .find(|value| value.starts_with("sha256:"))
          {
            image_id = Some(found.to_owned());
          }
        }
      }
      image_id
        .filter(|value| value.starts_with("sha256:"))
        .ok_or_else(|| {
          TrustedVerifierError::TrustedVerifierInfrastructureFailure(format!(
            "Docker snapshot build returned no image identity: {diagnostic}"
          ))
        })
    };
    tokio::select! {
      _ = cancel.cancelled() => Err(TrustedVerifierError::Cancelled),
      result = tokio::time::timeout(Duration::from_secs(spec.timeout_secs), build) => {
        result.map_err(|_| TrustedVerifierError::TrustedVerifierTimeout)?
      }
    }
  }
  async fn backend_version(
    &self,
    spec: &TrustedVerificationSpec,
    cancel: &CancellationToken,
  ) -> Result<String, TrustedVerifierError> {
    let observed = self
      .request(self.client()?.version(), spec.timeout_secs, cancel)
      .await
      .map_err(map_runtime_unavailable)?;
    let version: DockerVersion = serde_json::from_value(
      serde_json::to_value(observed)
        .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?,
    )
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?;
    if version.os != "linux" || version.version.trim().is_empty() {
      return Err(TrustedVerifierError::IsolationUnavailable(
        "trusted verifier requires a Linux Docker server with an identified version".into(),
      ));
    }
    Ok(version.version)
  }

  // The explicit authority inputs keep policy, revision, and bindings visible at this boundary.
  #[expect(clippy::too_many_arguments)]
  async fn execute_created_container(
    &self,
    container_id: &str,
    snapshot_image: &str,
    revision: &str,
    spec: &TrustedVerificationSpec,
    obligation_ids: &[ObligationId],
    max_output_bytes: usize,
    backend_version: &str,
    cancel: &CancellationToken,
  ) -> Result<TrustedVerifierExecution, TrustedVerifierError> {
    let inspect_before = self.inspect(container_id, spec, cancel).await?;
    let attestation = attest(
      &inspect_before,
      snapshot_image,
      spec,
      backend_version,
      self.control_plane_fingerprint()?,
    )?;
    if !attestation.satisfies(spec) {
      return Err(TrustedVerifierError::IsolationUnavailable(
        "Docker container capabilities are weaker than the requested policy".into(),
      ));
    }

    let attach_options = AttachContainerOptionsBuilder::default()
      .stdin(false)
      .stdout(true)
      .stderr(true)
      .stream(true)
      .logs(false)
      .build();
    let attached = self
      .request(
        self
          .client()?
          .attach_container(container_id, Some(attach_options)),
        spec.timeout_secs,
        cancel,
      )
      .await?;
    let started_at = Utc::now();
    let timer = Instant::now();
    self
      .request(
        self.client()?.start_container(container_id, None),
        spec.timeout_secs,
        cancel,
      )
      .await?;
    let mut output = attached.output;
    let mut wait = self.client()?.wait_container(
      container_id,
      Some(
        WaitContainerOptionsBuilder::default()
          .condition("not-running")
          .build(),
      ),
    );
    let observation = async {
      let output_task = async {
        let mut stdout = String::new();
        let mut stderr = String::new();
        while let Some(chunk) = output.next().await {
          match chunk.map_err(|error| {
            TrustedVerifierError::TrustedVerifierInfrastructureFailure(error.to_string())
          })? {
            LogOutput::StdOut { message } | LogOutput::Console { message } => {
              append_limited(&mut stdout, &message, max_output_bytes);
            }
            LogOutput::StdErr { message } => {
              append_limited(&mut stderr, &message, max_output_bytes);
            }
            LogOutput::StdIn { .. } => {}
          }
        }
        Ok::<_, TrustedVerifierError>((stdout, stderr))
      };
      let wait_task = async {
        wait
          .next()
          .await
          .ok_or_else(|| {
            TrustedVerifierError::TrustedVerifierInfrastructureFailure(
              "Docker wait returned no completion observation".into(),
            )
          })?
          .map_err(|error| {
            TrustedVerifierError::TrustedVerifierInfrastructureFailure(error.to_string())
          })?;
        Ok::<_, TrustedVerifierError>(())
      };
      let (output, ()) = tokio::try_join!(output_task, wait_task)?;
      Ok::<_, TrustedVerifierError>(output)
    };
    let (stdout, stderr) = tokio::select! {
      _ = cancel.cancelled() => return Err(TrustedVerifierError::Cancelled),
      result = tokio::time::timeout(Duration::from_secs(spec.timeout_secs), observation) => {
        result.map_err(|_| TrustedVerifierError::TrustedVerifierTimeout)??
      }
    };
    let finished_at = Utc::now();
    let inspect_after = self.inspect(container_id, spec, cancel).await?;
    let final_attestation = attest(
      &inspect_after,
      snapshot_image,
      spec,
      backend_version,
      self.control_plane_fingerprint()?,
    )?;
    if final_attestation != attestation {
      return Err(TrustedVerifierError::IsolationUnavailable(
        "trusted verifier isolation changed during execution".into(),
      ));
    }
    if inspect_after.state.oom_killed || !inspect_after.state.error.is_empty() {
      return Err(TrustedVerifierError::TrustedVerifierInfrastructureFailure(
        format!(
          "container runtime reported OOM={} error={:?}",
          inspect_after.state.oom_killed, inspect_after.state.error
        ),
      ));
    }
    if inspect_after.state.running || !inspect_after.state.finished_at.contains('T') {
      return Err(TrustedVerifierError::TrustedVerifierInfrastructureFailure(
        "container did not reach an inspectable completed state".into(),
      ));
    }
    let exit_code = i32::try_from(inspect_after.state.exit_code).map_err(|_| {
      TrustedVerifierError::TrustedVerifierInfrastructureFailure(
        "container exit status exceeds i32".into(),
      )
    })?;
    let result = if exit_code == 0 {
      TrustedExecutionResult::Supports
    } else {
      TrustedExecutionResult::Contradicts { exit_code }
    };
    let record = TrustedExecutionRecord {
      id: VerificationRunId::new(),
      revision: revision.into(),
      verifier_name: spec.name.clone(),
      spec_hash: spec
        .fingerprint()
        .map_err(|error| TrustedVerifierError::InvalidTrustedVerifierSpec(error.to_string()))?,
      isolation_policy_hash: spec
        .isolation_policy_hash()
        .map_err(|error| TrustedVerifierError::InvalidTrustedVerifierSpec(error.to_string()))?,
      attestation: Some(attestation),
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
        stdout,
        stderr,
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

  async fn inspect(
    &self,
    container_id: &str,
    spec: &TrustedVerificationSpec,
    cancel: &CancellationToken,
  ) -> Result<DockerInspect, TrustedVerifierError> {
    let observed = self
      .request(
        self.client()?.inspect_container(container_id, None),
        spec.timeout_secs,
        cancel,
      )
      .await?;
    serde_json::from_value(
      serde_json::to_value(observed)
        .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))?,
    )
    .map_err(|error| TrustedVerifierError::IsolationUnavailable(error.to_string()))
  }
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

fn container_create_body(
  snapshot_image: &str,
  spec: &TrustedVerificationSpec,
) -> Result<ContainerCreateBody, TrustedVerifierError> {
  let memory = i64::try_from(spec.resources.memory_bytes).map_err(|_| {
    TrustedVerifierError::InvalidTrustedVerifierSpec("memory_bytes exceeds Docker i64".into())
  })?;
  let mut command = vec![spec.program.clone()];
  command.extend(spec.args.iter().cloned());
  let mut environment = vec!["HOME=/tmp/home".into(), "TMPDIR=/tmp".into()];
  environment.extend(
    spec
      .environment
      .iter()
      .map(|(name, value)| format!("{name}={value}")),
  );
  Ok(ContainerCreateBody {
    user: Some(CONTAINER_USER.into()),
    attach_stdin: Some(false),
    attach_stdout: Some(true),
    attach_stderr: Some(true),
    tty: Some(false),
    open_stdin: Some(false),
    env: Some(environment),
    cmd: Some(command),
    entrypoint: Some(vec![String::new()]),
    image: Some(snapshot_image.into()),
    working_dir: Some(container_working_directory(spec)),
    network_disabled: Some(true),
    host_config: Some(HostConfig {
      memory: Some(memory),
      memory_swap: Some(memory),
      nano_cpus: Some(i64::from(spec.resources.cpu_millis) * 1_000_000),
      pids_limit: Some(i64::from(spec.resources.process_limit)),
      binds: Some(Vec::new()),
      mounts: Some(Vec::new()),
      network_mode: Some("none".into()),
      pid_mode: Some("private".into()),
      privileged: Some(false),
      readonly_rootfs: Some(true),
      cap_add: Some(Vec::new()),
      cap_drop: Some(vec!["ALL".into()]),
      security_opt: Some(vec!["no-new-privileges=true".into()]),
      tmpfs: Some(HashMap::from([(
        CONTAINER_TMP.into(),
        format!("rw,nosuid,nodev,size={}", spec.resources.writable_tmp_bytes),
      )])),
      ipc_mode: Some("private".into()),
      auto_remove: Some(false),
      log_config: Some(HostConfigLogConfig {
        typ: Some("none".into()),
        config: Some(HashMap::new()),
      }),
      ..Default::default()
    }),
    ..Default::default()
  })
}
fn container_working_directory(spec: &TrustedVerificationSpec) -> String {
  if spec.working_directory == "." {
    CONTAINER_WORKSPACE.into()
  } else {
    format!("{CONTAINER_WORKSPACE}/{}", spec.working_directory)
  }
}

fn attest(
  inspect: &DockerInspect,
  snapshot_image: &str,
  spec: &TrustedVerificationSpec,
  backend_version: &str,
  control_plane_fingerprint: &str,
) -> Result<TrustedExecutionAttestation, TrustedVerifierError> {
  let candidate_snapshot_is_immutable = inspect.mounts.is_empty();
  let tmpfs_size = inspect
    .host_config
    .tmpfs
    .get(CONTAINER_TMP)
    .and_then(|options| {
      let sizes: Vec<_> = options
        .split(',')
        .filter_map(|option| option.strip_prefix("size="))
        .collect();
      (sizes.len() == 1)
        .then(|| sizes[0].parse::<u64>().ok())
        .flatten()
        .filter(|size| {
          *size == spec.resources.writable_tmp_bytes
            && options.split(',').any(|option| option == "nosuid")
            && options.split(',').any(|option| option == "nodev")
        })
    });
  let caps_dropped = inspect
    .host_config
    .cap_drop
    .iter()
    .any(|capability| capability.eq_ignore_ascii_case("ALL"));
  let no_new_privileges = inspect
    .host_config
    .security_opt
    .iter()
    .any(|option| option == "no-new-privileges" || option == "no-new-privileges=true");
  let resources_match = inspect.host_config.memory == spec.resources.memory_bytes
    && inspect.host_config.memory_swap == spec.resources.memory_bytes
    && inspect.host_config.nano_cpus == u64::from(spec.resources.cpu_millis) * 1_000_000
    && inspect.host_config.pids_limit == i64::from(spec.resources.process_limit);
  let network_is_disabled = inspect.network_settings.networks.is_empty()
    || (inspect.network_settings.networks.len() == 1
      && inspect
        .network_settings
        .networks
        .get("none")
        .is_some_and(|network| {
          network.ip_address.is_empty()
            && network.global_ipv6_address.is_empty()
            && network.gateway.is_empty()
        }));
  let expected_command: Vec<_> = std::iter::once(&spec.program)
    .chain(spec.args.iter())
    .cloned()
    .collect();
  let mut actual_environment = BTreeMap::new();
  for entry in &inspect.config.env {
    if let Some((name, value)) = entry.split_once('=') {
      actual_environment.insert(name, value);
    }
  }
  let mut expected_environment = BTreeMap::from([("HOME", "/tmp/home"), ("TMPDIR", "/tmp")]);
  expected_environment.extend(
    spec
      .environment
      .iter()
      .map(|(name, value)| (name.as_str(), value.as_str())),
  );
  let execution_semantics_match = inspect.image == snapshot_image
    && inspect.config.image == snapshot_image
    && inspect.config.entrypoint.as_ref().is_none_or(Vec::is_empty)
    && inspect.config.cmd == expected_command
    && inspect.config.working_dir == container_working_directory(spec)
    && expected_environment
      .iter()
      .all(|(name, value)| actual_environment.get(name) == Some(value))
    && actual_environment.len() == expected_environment.len();
  let policy_matches = candidate_snapshot_is_immutable
    && inspect.host_config.readonly_rootfs
    && !inspect.host_config.privileged
    && inspect.host_config.network_mode == "none"
    && inspect.host_config.pid_mode == "private"
    && network_is_disabled
    && inspect.host_config.log_config.typ == "none"
    && inspect.host_config.log_config.config.is_empty()
    && tmpfs_size.is_some()
    && caps_dropped
    && inspect.host_config.cap_add.is_empty()
    && no_new_privileges
    && inspect.config.user == CONTAINER_USER
    && resources_match
    && execution_semantics_match;
  if !policy_matches {
    return Err(TrustedVerifierError::IsolationUnavailable(
      "Docker inspect did not attest every requested isolation control".into(),
    ));
  }
  Ok(TrustedExecutionAttestation {
    backend: TrustedVerifierBackend::Docker,
    backend_version: backend_version.into(),
    image_id: inspect.image.clone(),
    control_plane: ControlPlanePolicy::ExclusiveMutualTls,
    control_plane_fingerprint: control_plane_fingerprint.into(),
    candidate_filesystem: CandidateFilesystemPolicy::ReadOnly,
    root_filesystem: RootFilesystemPolicy::ReadOnly,
    temporary_filesystem: TemporaryFilesystemPolicy::DisposableTmpfs,
    network: NetworkPolicy::Disabled,
    environment: EnvironmentPolicy::ExplicitOnly,
    process_namespace: ProcessNamespacePolicy::Private,
    capabilities_dropped: true,
    no_new_privileges: true,
    unprivileged_user: true,
    memory_bytes: inspect.host_config.memory,
    cpu_millis: u32::try_from(inspect.host_config.nano_cpus / 1_000_000).map_err(|_| {
      TrustedVerifierError::IsolationUnavailable("Docker CPU attestation exceeds u32".into())
    })?,
    process_limit: u32::try_from(inspect.host_config.pids_limit).map_err(|_| {
      TrustedVerifierError::IsolationUnavailable("Docker process attestation is invalid".into())
    })?,
    writable_tmp_bytes: tmpfs_size.expect("validated tmpfs size"),
  })
}

fn map_runtime_unavailable(error: TrustedVerifierError) -> TrustedVerifierError {
  match error {
    TrustedVerifierError::TrustedVerifierTimeout => {
      TrustedVerifierError::TrustedVerifierUnavailable(
        "Docker runtime capability check timed out".into(),
      )
    }
    TrustedVerifierError::TrustedVerifierInfrastructureFailure(message) => {
      TrustedVerifierError::TrustedVerifierUnavailable(message)
    }
    other => other,
  }
}

fn append_limited(target: &mut String, bytes: &[u8], limit: usize) {
  let remaining = limit.saturating_sub(target.len());
  target.push_str(&String::from_utf8_lossy(
    &bytes[..bytes.len().min(remaining)],
  ));
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerImageInspect {
  repo_digests: Vec<String>,
  config: DockerImageConfig,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerImageConfig {
  on_build: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerVersion {
  version: String,
  #[serde(rename = "Os")]
  os: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerInspect {
  image: String,
  config: DockerContainerConfig,
  host_config: DockerHostConfig,
  mounts: Vec<serde_json::Value>,
  network_settings: DockerNetworkSettings,
  state: DockerState,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerContainerConfig {
  user: String,
  image: String,
  cmd: Vec<String>,
  entrypoint: Option<Vec<String>>,
  working_dir: String,
  env: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerHostConfig {
  readonly_rootfs: bool,
  privileged: bool,
  network_mode: String,
  pid_mode: String,
  pids_limit: i64,
  memory: u64,
  memory_swap: u64,
  nano_cpus: u64,
  cap_drop: Vec<String>,
  cap_add: Vec<String>,
  security_opt: Vec<String>,
  tmpfs: BTreeMap<String, String>,
  log_config: DockerLogConfig,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerLogConfig {
  #[serde(rename = "Type")]
  typ: String,
  config: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerNetworkSettings {
  networks: BTreeMap<String, DockerNetworkEndpoint>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerNetworkEndpoint {
  ip_address: String,
  global_ipv6_address: String,
  gateway: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DockerState {
  running: bool,
  oom_killed: bool,
  error: String,
  exit_code: i64,
  finished_at: String,
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use tenet_domain::trusted_verifier::{
    TrustedIsolationPolicy, TrustedResourcePolicy, TrustedVerifierProtocol,
  };

  use super::*;
  const SNAPSHOT_IMAGE: &str = "sha256:immutable-snapshot";

  fn spec() -> TrustedVerificationSpec {
    TrustedVerificationSpec {
      name: "expiry-boundary".into(),
      backend: TrustedVerifierBackend::Docker,
      image: format!("example/verifier@sha256:{}", "a".repeat(64)),
      program: "verify".into(),
      args: vec!["--expiry".into()],
      working_directory: "crate".into(),
      environment: BTreeMap::from([("CI".into(), "true".into())]),
      timeout_secs: 30,
      isolation: TrustedIsolationPolicy::default(),
      resources: TrustedResourcePolicy::default(),
      protocol: TrustedVerifierProtocol::ExitCode,
    }
  }
  fn inspect(value: &TrustedVerificationSpec) -> DockerInspect {
    DockerInspect {
      image: SNAPSHOT_IMAGE.into(),
      config: DockerContainerConfig {
        user: CONTAINER_USER.into(),
        image: SNAPSHOT_IMAGE.into(),
        cmd: vec![value.program.clone(), value.args[0].clone()],
        entrypoint: None,
        working_dir: container_working_directory(value),
        env: vec![
          "HOME=/tmp/home".into(),
          "TMPDIR=/tmp".into(),
          "CI=true".into(),
        ],
      },
      host_config: DockerHostConfig {
        readonly_rootfs: true,
        privileged: false,
        network_mode: "none".into(),
        pid_mode: "private".into(),
        pids_limit: i64::from(value.resources.process_limit),
        memory: value.resources.memory_bytes,
        memory_swap: value.resources.memory_bytes,
        nano_cpus: u64::from(value.resources.cpu_millis) * 1_000_000,
        cap_drop: vec!["ALL".into()],
        cap_add: Vec::new(),
        security_opt: vec!["no-new-privileges:true".into()],
        tmpfs: BTreeMap::from([(
          CONTAINER_TMP.into(),
          format!(
            "rw,nosuid,nodev,size={}",
            value.resources.writable_tmp_bytes
          ),
        )]),
        log_config: DockerLogConfig {
          typ: "none".into(),
          config: BTreeMap::new(),
        },
      },
      mounts: Vec::new(),
      network_settings: DockerNetworkSettings {
        networks: BTreeMap::from([(
          "none".into(),
          DockerNetworkEndpoint {
            ip_address: String::new(),
            global_ipv6_address: String::new(),
            gateway: String::new(),
          },
        )]),
      },
      state: DockerState {
        running: false,
        oom_killed: false,
        error: String::new(),
        exit_code: 0,
        finished_at: "2026-08-26T00:00:00Z".into(),
      },
    }
  }
  fn assert_attestation_rejected(observed: &DockerInspect, value: &TrustedVerificationSpec) {
    let error = attest(observed, SNAPSHOT_IMAGE, value, "Docker 1", "control-plane")
      .expect_err("weaker observation must fail attestation");
    assert!(matches!(
      error,
      TrustedVerifierError::IsolationUnavailable(_)
    ));
  }

  #[test]
  fn docker_attestation_rejects_image_environment_extras() {
    let value = spec();
    let mut observed = inspect(&value);
    observed.config.env.push("HOST_SECRET=present".into());

    assert_attestation_rejected(&observed, &value);
  }

  #[test]
  fn docker_attestation_rejects_readded_capabilities() {
    let value = spec();
    let mut observed = inspect(&value);
    observed.host_config.cap_add.push("SYS_ADMIN".into());

    assert_attestation_rejected(&observed, &value);
  }

  #[test]
  fn docker_attestation_rejects_oversized_tmpfs() {
    let value = spec();
    let mut observed = inspect(&value);
    observed.host_config.tmpfs.insert(
      CONTAINER_TMP.into(),
      format!(
        "rw,nosuid,nodev,size={}0",
        value.resources.writable_tmp_bytes
      ),
    );

    assert_attestation_rejected(&observed, &value);
  }

  #[test]
  fn docker_attestation_rejects_disabled_no_new_privileges() {
    let value = spec();
    let mut observed = inspect(&value);
    observed.host_config.security_opt = vec!["no-new-privileges=false".into()];

    assert_attestation_rejected(&observed, &value);
  }

  #[test]
  fn docker_attestation_rejects_effective_network_attachment() {
    let value = spec();
    let mut observed = inspect(&value);
    observed.network_settings.networks = BTreeMap::from([(
      "bridge".into(),
      DockerNetworkEndpoint {
        ip_address: "172.17.0.2".into(),
        global_ipv6_address: String::new(),
        gateway: "172.17.0.1".into(),
      },
    )]);

    assert_attestation_rejected(&observed, &value);
  }

  #[test]
  fn docker_attestation_rejects_weaker_isolation() {
    let value = spec();
    let mut observed = inspect(&value);
    observed.host_config.network_mode = "bridge".into();

    let error = attest(
      &observed,
      SNAPSHOT_IMAGE,
      &value,
      "Docker 1",
      "control-plane",
    )
    .expect_err("network access must fail attestation");

    assert!(matches!(
      error,
      TrustedVerifierError::IsolationUnavailable(_)
    ));
  }

  #[test]
  fn docker_attestation_rejects_changed_executable_semantics() {
    let value = spec();
    let mut observed = inspect(&value);
    observed.config.cmd = vec!["different-program".into()];

    let error = attest(
      &observed,
      SNAPSHOT_IMAGE,
      &value,
      "Docker 1",
      "control-plane",
    )
    .expect_err("different executable must fail attestation");

    assert!(matches!(
      error,
      TrustedVerifierError::IsolationUnavailable(_)
    ));
  }

  #[test]
  fn docker_request_contains_every_required_isolation_control() {
    let body = container_create_body(SNAPSHOT_IMAGE, &spec()).expect("container request");
    let host = body.host_config.expect("host policy");

    assert_eq!(host.network_mode.as_deref(), Some("none"));
    assert_eq!(body.network_disabled, Some(true));
    assert_eq!(host.readonly_rootfs, Some(true));
    assert_eq!(host.cap_drop, Some(vec!["ALL".into()]));
    assert_eq!(host.cap_add, Some(Vec::new()));
    assert_eq!(
      host.security_opt,
      Some(vec!["no-new-privileges=true".into()])
    );
    assert_eq!(host.pid_mode.as_deref(), Some("private"));
  }

  #[test]
  fn docker_request_has_no_host_mounts() {
    let body = container_create_body(SNAPSHOT_IMAGE, &spec()).expect("container request");
    let host = body.host_config.expect("host policy");

    assert_eq!(host.binds, Some(Vec::new()));
    assert_eq!(host.mounts, Some(Vec::new()));
  }

  #[test]
  fn docker_request_executes_structured_program_without_shell() {
    let body = container_create_body(SNAPSHOT_IMAGE, &spec()).expect("container request");

    assert_eq!(body.image.as_deref(), Some(SNAPSHOT_IMAGE));
    assert_eq!(body.entrypoint, Some(vec![String::new()]));
    assert_eq!(body.cmd, Some(vec!["verify".into(), "--expiry".into()]));
  }

  #[test]
  fn docker_request_passes_only_explicit_container_environment() {
    let body = container_create_body(SNAPSHOT_IMAGE, &spec()).expect("container request");

    assert_eq!(
      body.env,
      Some(vec![
        "HOME=/tmp/home".into(),
        "TMPDIR=/tmp".into(),
        "CI=true".into()
      ])
    );
  }

  #[test]
  fn unavailable_exclusive_control_plane_is_fail_closed() {
    let runner = DockerTrustedVerifier::unavailable("mTLS credentials unavailable");

    assert!(matches!(
      runner.client(),
      Err(TrustedVerifierError::TrustedVerifierUnavailable(_))
    ));
  }
}

#[cfg(test)]
mod failure_tests {
  use std::collections::BTreeMap;

  use tenet_domain::trusted_verifier::{
    TrustedIsolationPolicy, TrustedResourcePolicy, TrustedVerifierProtocol,
  };

  use super::*;

  fn spec() -> TrustedVerificationSpec {
    TrustedVerificationSpec {
      name: "failure-boundary".into(),
      backend: TrustedVerifierBackend::Docker,
      image: format!("example/verifier@sha256:{}", "a".repeat(64)),
      program: "verify".into(),
      args: Vec::new(),
      working_directory: ".".into(),
      environment: BTreeMap::new(),
      timeout_secs: 1,
      isolation: TrustedIsolationPolicy::default(),
      resources: TrustedResourcePolicy::default(),
      protocol: TrustedVerifierProtocol::ExitCode,
    }
  }

  #[tokio::test]
  async fn unavailable_runtime_fails_without_execution_record() {
    let candidate = tempfile::tempdir().expect("candidate");
    let runner = DockerTrustedVerifier::unavailable("exclusive Docker API unavailable");

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
      .expect_err("missing Docker must fail closed");

    assert!(matches!(
      error,
      TrustedVerifierError::TrustedVerifierUnavailable(_)
    ));
  }

  #[test]
  fn weaker_capability_attestation_is_rejected() {
    let value = spec();
    let attestation = TrustedExecutionAttestation {
      backend: TrustedVerifierBackend::Docker,
      backend_version: "27.0".into(),
      image_id: "sha256:image".into(),
      control_plane: ControlPlanePolicy::ExclusiveMutualTls,
      control_plane_fingerprint: "control-plane".into(),
      candidate_filesystem: CandidateFilesystemPolicy::ReadOnly,
      root_filesystem: RootFilesystemPolicy::ReadOnly,
      temporary_filesystem: TemporaryFilesystemPolicy::DisposableTmpfs,
      network: NetworkPolicy::Disabled,
      environment: EnvironmentPolicy::ExplicitOnly,
      process_namespace: ProcessNamespacePolicy::Private,
      capabilities_dropped: false,
      no_new_privileges: true,
      unprivileged_user: true,
      memory_bytes: value.resources.memory_bytes,
      cpu_millis: value.resources.cpu_millis,
      process_limit: value.resources.process_limit,
      writable_tmp_bytes: value.resources.writable_tmp_bytes,
    };

    assert!(!attestation.satisfies(&value));
  }
}
