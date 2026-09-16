//! Infrastructure ports implemented by `tenet-workspace` and `tenet-runner`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use tenet_domain::{
  algebra::{
    CompletionContractV1, CompletionPolicyId, EvidenceResult, ExecutionContext,
    ExecutionObservation, VerifierId, VerifierRun as DomainVerifierRun,
  },
  authority::AdmissionId,
  evidence::{
    AuthorityId, CandidateId, ContentObjectId, ExecutionProvenance, OracleIdentity,
    VerifierObservation,
  },
  policy::{VerificationPolicy, VerifierSpec},
  snapshot::TreeManifest,
};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedEntry {
  Any,
  File,
  Directory,
}

#[derive(Debug, Error)]
pub enum PathResolutionError {
  #[error("path must be a non-empty safe relative path")]
  Invalid,
  #[error("path escapes project root: {path}")]
  PathEscape { path: String },
  #[error("unsupported symlink in trust surface: {path}")]
  UnsupportedSymlink { path: String },
  #[error("path component is missing: {path}")]
  Missing { path: String },
  #[error("path is not a directory: {path}")]
  NotDirectory { path: String },
  #[error("path is not a regular file: {path}")]
  NotFile { path: String },
  #[error("unsupported filesystem entry: {path}")]
  Special { path: String },
  #[error("resolve path {path}: {source}")]
  Io {
    path: String,
    #[source]
    source: std::io::Error,
  },
}

#[derive(Debug, Error)]
pub enum ContentStoreError {
  #[error("content object is missing: {id}")]
  Missing { id: String },
  #[error("content object integrity failure for {id}: {message}")]
  Integrity { id: String, message: String },
  #[error("content object materialization failed for {id}: {message}")]
  MaterializationMessage { id: String, message: String },
  #[error("content object materialization failed for {id}: {source}")]
  Materialization {
    id: String,
    #[source]
    source: std::io::Error,
  },
}

impl ContentStoreError {
  pub fn integrity(id: &ContentObjectId, message: impl Into<String>) -> Self {
    Self::Integrity {
      id: id.0.clone(),
      message: message.into(),
    }
  }

  pub fn materialization(id: &ContentObjectId, message: impl Into<String>) -> Self {
    Self::MaterializationMessage {
      id: id.0.clone(),
      message: message.into(),
    }
  }

  pub fn materialization_io(id: &ContentObjectId, source: std::io::Error) -> Self {
    Self::Materialization {
      id: id.0.clone(),
      source,
    }
  }
}

pub trait SnapshotHandle: Send {
  fn path(&self) -> &Path;
}

pub struct InitObservation {
  pub root: PathBuf,
  pub policy: VerificationPolicy,
  pub spec_digest: String,
  pub created: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrityObservation {
  pub object_count: usize,
  pub blob_count: usize,
  pub ref_count: usize,
}

pub trait RepositoryLock: Send {}

pub trait Repository: Send + Sync {
  fn initialize(&self, cwd: &Path, spec: Option<&Path>) -> Result<InitObservation>;
  fn discover_root(&self, cwd: &Path) -> Result<PathBuf>;
  fn acquire_lock(&self, root: &Path) -> Result<Box<dyn RepositoryLock>>;
  fn resolve_relative_path(
    &self,
    root: &Path,
    relative: &str,
    expected: ExpectedEntry,
  ) -> std::result::Result<PathBuf, PathResolutionError>;
  fn read_file(&self, path: &Path) -> Result<Vec<u8>>;
  fn is_executable(&self, path: &Path) -> Result<bool>;
  fn load_policy(&self, root: &Path) -> Result<VerificationPolicy>;
  fn specification_digest(&self, root: &Path, policy: &VerificationPolicy) -> Result<String>;
  fn stage_authority_surface(
    &self,
    root: &Path,
    policy: &VerificationPolicy,
    contract: &CompletionContractV1,
  ) -> Result<Box<dyn SnapshotHandle>>;
  fn capture(&self, project_root: &Path, source: &Path) -> Result<ContentObjectId>;
  fn capture_selected(
    &self,
    project_root: &Path,
    source: &Path,
    include: &[String],
    exclude: &[String],
  ) -> Result<ContentObjectId>;
  fn materialize(
    &self,
    project_root: &Path,
    id: &ContentObjectId,
  ) -> std::result::Result<Box<dyn SnapshotHandle>, ContentStoreError>;
  fn fresh_scratch(&self, project_root: &Path) -> Result<Box<dyn SnapshotHandle>>;
  /// Fresh controlled output directory for verifier artifacts.
  fn fresh_output(&self, project_root: &Path) -> Result<Box<dyn SnapshotHandle>>;
  fn manifest(
    &self,
    project_root: &Path,
    id: &ContentObjectId,
  ) -> std::result::Result<TreeManifest, ContentStoreError>;
  fn store_object(&self, root: &Path, bytes: &[u8]) -> Result<ContentObjectId>;
  fn load_object(
    &self,
    root: &Path,
    id: &ContentObjectId,
  ) -> std::result::Result<Vec<u8>, ContentStoreError>;
  fn read_ref(&self, root: &Path, name: &str) -> Result<Option<ContentObjectId>>;
  fn write_ref(&self, root: &Path, name: &str, id: &ContentObjectId) -> Result<()>;
  fn remove_ref(&self, root: &Path, name: &str) -> Result<()>;
  fn list_refs(&self, root: &Path, prefix: &str) -> Result<Vec<(String, ContentObjectId)>>;
  fn inspect_integrity(&self, root: &Path) -> Result<IntegrityObservation>;
  /// Materialize an exact Candidate and Authority surface into a private
  /// view for `PROTECTED_V1` execution. The view must enforce, through an OS
  /// boundary rather than detection, that no process other than the confined
  /// verifier can observe transiently different bytes than the captured
  /// identities while the verifier runs. A platform that cannot enforce this
  /// returns an error; the caller then yields an infrastructure result and
  /// never downgrades assurance.
  fn stage_protected_view(
    &self,
    root: &Path,
    candidate: &ContentObjectId,
    authority: &ContentObjectId,
  ) -> Result<Box<dyn ProtectedView>>;
}

/// One expected file inside a protected view: path, exact bytes, and mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewDigest {
  /// View-relative path: `candidate/<relative>` or `authority/<relative>`.
  pub path: String,
  /// Lowercase sha256 hex of the expected bytes.
  pub sha256_hex: String,
  /// Expected executable flag of the view file.
  pub executable: bool,
}

/// A protected Candidate/Authority view for one verifier run. The runner
/// executes the verifier against `candidate_root`/`authority_root`; the
/// workspace enforces immutability of those roots against external processes
/// (macOS: a read-only mounted volume whose backing store is unlinked). On
/// platforms where the runner creates the private namespace (Linux
/// Bubblewrap), `view_digests`/`view_directories` are the trusted
/// expectations the runner must materialize and verify inside that namespace
/// before the verifier observes anything, and `verify_intact` is trivially
/// true because the namespace itself is the boundary.
pub trait ProtectedView: Send {
  fn candidate_root(&self) -> &Path;
  fn authority_root(&self) -> &Path;
  fn view_digests(&self) -> &[ViewDigest];
  /// View-relative paths of expected directories (including empty ones).
  fn view_directories(&self) -> &[String];
  /// Post-run check that the enforcing boundary still holds and that the
  /// view's bytes still hash to the expected identities. `false` means the
  /// run cannot contribute admissible evidence. On platforms where the
  /// runner materialized a private namespace copy verified against
  /// `view_digests`, the namespace itself is the boundary and this is
  /// trivially true because the staging source is transient by design.
  fn verify_intact(
    &self,
    expected_candidate: &ContentObjectId,
    expected_authority: &ContentObjectId,
  ) -> Result<bool>;
}

pub struct ExecutedVerifier {
  pub observation: VerifierObservation,
  pub result: EvidenceResult,
  pub infrastructure_error: Option<String>,
  pub context: ExecutionContext,
  pub execution: ExecutionProvenance,
}

impl ExecutedVerifier {
  pub fn domain_run(
    &self,
    admission: AdmissionId,
    authority: AuthorityId,
    contract: ContentObjectId,
    completion_policy: CompletionPolicyId,
    candidate: CandidateId,
    verifier: impl Into<String>,
  ) -> DomainVerifierRun {
    DomainVerifierRun {
      admission,
      authority,
      contract,
      completion_policy,
      candidate,
      verifier: VerifierId(verifier.into()),
      observation: ExecutionObservation {
        result: self.result,
        exit_code: self.observation.exit_code,
        timed_out: self.observation.timed_out,
        infrastructure_error: self.infrastructure_error.clone(),
      },
      context: self.context.clone(),
      provenance: self.execution.clone(),
    }
  }
}

pub struct VerifierRun<'a> {
  pub candidate_root: &'a Path,
  pub authority_root: &'a Path,
  pub scratch_root: &'a Path,
  pub output_root: &'a Path,
  pub verifier: &'a VerifierSpec,
  pub authority_id: &'a AuthorityId,
  pub candidate_id: &'a CandidateId,
  pub oracle_identity: &'a OracleIdentity,
  /// Trusted expectations for every file in the protected view; empty for
  /// local runs. A namespace-creating runner must build its private copy
  /// exactly from these expectations and verify it before exec'ing the
  /// verifier.
  pub view_digests: &'a [ViewDigest],
  /// Expected view directories, including empty ones.
  pub view_directories: &'a [String],
}

pub trait VerifierRunner: Send + Sync {
  fn run(&self, request: &VerifierRun<'_>) -> Result<ExecutedVerifier>;
}
