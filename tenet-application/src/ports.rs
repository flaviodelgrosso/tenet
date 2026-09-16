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
}

pub trait VerifierRunner: Send + Sync {
  fn run(&self, request: &VerifierRun<'_>) -> Result<ExecutedVerifier>;
}
