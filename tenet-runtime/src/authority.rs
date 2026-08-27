//! Controller bootstrap for repository authority credentials.

#[cfg(unix)]
use std::os::fd::{FromRawFd, RawFd};
use std::{
  fmt::Write as _,
  fs::{self, File, OpenOptions},
  io::{Read, Write},
  path::{Path, PathBuf},
};

use keyring::v1::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

use tenet_domain::config::TENET_DIR;

use crate::store::RunLock;

const AUTHORITY_FILE: &str = "authority.json";
const AUTHORITY_NAMESPACE_ENV: &str = "TENET_CONTROLLER_AUTHORITY_NAMESPACE";
const AUTHORITY_KEY_FD_ENV: &str = "TENET_CONTROLLER_AUTHORITY_KEY_FD";
const CREDENTIAL_SERVICE: &str = "dev.tenet.controller-authority";
const CREDENTIAL_RECORD_PREFIX: &[u8] = b"tenet-authority-v1\0";
const AUTHORITY_SECRET_BYTES: usize = 32;
const MAX_AUTHORITY_ID_BYTES: usize = 128;

/// Credential mechanism used to resolve one repository authority identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialProviderKind {
  /// Native macOS Keychain, Windows Credential Manager, or freedesktop Secret Service.
  OsKeyring,
  /// An inherited descriptor selected explicitly by the launcher environment.
  InheritedFd,
}

impl CredentialProviderKind {
  /// Returns a user-facing provider name without platform or cryptographic details.
  pub fn display_name(self) -> &'static str {
    match self {
      Self::OsKeyring => "OS credential store",
      Self::InheritedFd => "inherited descriptor (advanced/CI)",
    }
  }
}

/// A repository authority credential held only for controller bootstrap.
pub struct AuthorityCredential {
  authority_id: String,
  key_material: Zeroizing<Vec<u8>>,
}

impl AuthorityCredential {
  /// Constructs provider output with a safe public identity and exactly 32 secret bytes.
  pub fn new(authority_id: String, key_material: Vec<u8>) -> Result<Self, AuthorityError> {
    Self::from_zeroizing(authority_id, Zeroizing::new(key_material))
  }

  fn from_zeroizing(
    authority_id: String,
    key_material: Zeroizing<Vec<u8>>,
  ) -> Result<Self, AuthorityError> {
    validate_authority_id(&authority_id)?;
    if key_material.len() != AUTHORITY_SECRET_BYTES {
      return Err(AuthorityError::MalformedCredential);
    }
    Ok(Self {
      authority_id,
      key_material,
    })
  }
}

/// Boundary implemented by secure local and externally managed credential sources.
pub trait CredentialProvider {
  /// Identifies the provider for non-secret repository metadata and user messages.
  fn kind(&self) -> CredentialProviderKind;

  /// Provisions a new stable repository identity and its credential.
  fn provision(&mut self) -> Result<AuthorityCredential, AuthorityError>;

  /// Resolves the credential for an already initialized repository identity.
  fn resolve(&mut self, authority_id: &str) -> Result<AuthorityCredential, AuthorityError>;

  /// Removes a credential created by a failed initialization attempt.
  fn remove(&mut self, authority_id: &str) -> Result<(), AuthorityError>;
}

/// Result of idempotent authority initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityInitialization {
  /// Stable, non-secret repository authority identity.
  pub authority_id: String,
  /// Provider retained as the default for future commands.
  pub provider: CredentialProviderKind,
  /// Whether this invocation provisioned the identity.
  pub created: bool,
}

/// User-facing authority lifecycle and credential failures.
#[derive(Debug, Error)]
pub enum AuthorityError {
  /// Authority metadata has not been initialized.
  #[error("controller authority is not initialized for this repository; run `tenet init`")]
  NotInitialized,
  /// A native credential no longer exists.
  #[error(
    "controller authority credential is missing from the OS credential store; restore the credential for this repository, or remove `.tenet/` to discard controller state and run `tenet init` again"
  )]
  MissingCredential,
  /// The configured native store cannot currently be used.
  #[error(
    "the OS credential store is unavailable or locked; unlock it and retry (Tenet did not modify repository authority state)"
  )]
  CredentialStoreUnavailable,
  /// Provider output cannot be accepted.
  #[error(
    "the stored controller authority credential is inconsistent; restore the credential for this repository, or remove `.tenet/` to discard controller state and run `tenet init` again"
  )]
  MalformedCredential,
  /// Public identity and resolved credential disagree.
  #[error(
    "the supplied controller authority identity does not match this repository; use its original credential, or remove `.tenet/` to discard controller state and run `tenet init` again"
  )]
  IdentityMismatch,
  /// Public authority identity cannot be stored or displayed safely.
  #[error(
    "controller authority identity must start with a letter or digit and contain only ASCII letters, digits, `.`, `_`, `:`, or `-`"
  )]
  InvalidAuthorityId,
  /// An inherited provider was selected incompletely.
  #[error(
    "advanced inherited-FD authority requires both {AUTHORITY_NAMESPACE_ENV} and {AUTHORITY_KEY_FD_ENV}"
  )]
  PartialInheritedFdConfiguration,
  /// An inherited provider is required for a repository initialized that way.
  #[error(
    "this repository uses the advanced inherited-FD authority provider; set {AUTHORITY_NAMESPACE_ENV} and {AUTHORITY_KEY_FD_ENV} to its original identity before retrying"
  )]
  InheritedFdRequired,
  /// The inherited descriptor cannot be used safely.
  #[error("the inherited controller authority descriptor is invalid or unavailable")]
  InvalidInheritedFd,
  /// An inherited credential is not a fixed-size authority key.
  #[error(
    "the inherited controller authority credential must contain exactly 32 cryptographically random bytes"
  )]
  InvalidInheritedCredential,
  /// Repository metadata is missing, malformed, unsafe, or unavailable.
  #[error("controller authority metadata at {path} is unavailable or invalid: {message}")]
  Metadata { path: PathBuf, message: String },
  /// Secure random generation failed.
  #[error("could not provision a controller authority credential from the operating system")]
  RandomUnavailable,
  /// An initialized credential could not be installed into controller persistence.
  #[error("controller authority identity is inconsistent with this process")]
  ProcessIdentityMismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityMetadata {
  version: u32,
  authority_id: String,
  provider: CredentialProviderKind,
  credential_fingerprint: String,
}

/// Native OS credential provider used by normal local commands.
#[derive(Default)]
pub struct OsCredentialProvider;

impl OsCredentialProvider {
  fn entry(authority_id: &str) -> Result<Entry, AuthorityError> {
    Entry::new(CREDENTIAL_SERVICE, authority_id)
      .map_err(|_| AuthorityError::CredentialStoreUnavailable)
  }
}

impl CredentialProvider for OsCredentialProvider {
  fn kind(&self) -> CredentialProviderKind {
    CredentialProviderKind::OsKeyring
  }

  fn provision(&mut self) -> Result<AuthorityCredential, AuthorityError> {
    let authority_id = Uuid::new_v4().to_string();
    let mut key_material = Zeroizing::new(vec![0_u8; AUTHORITY_SECRET_BYTES]);
    getrandom::fill(&mut key_material).map_err(|_| AuthorityError::RandomUnavailable)?;
    let mut record = Zeroizing::new(encode_os_record(&authority_id, &key_material)?);
    let result = Self::entry(&authority_id)?.set_secret(&record);
    record.zeroize();
    result.map_err(|_| AuthorityError::CredentialStoreUnavailable)?;
    AuthorityCredential::from_zeroizing(authority_id, key_material)
  }

  fn resolve(&mut self, authority_id: &str) -> Result<AuthorityCredential, AuthorityError> {
    let entry = Self::entry(authority_id)?;
    let mut record = Zeroizing::new(match entry.get_secret() {
      Ok(record) => record,
      Err(KeyringError::NoEntry) => return Err(AuthorityError::MissingCredential),
      Err(_) => return Err(AuthorityError::CredentialStoreUnavailable),
    });
    let result = decode_os_record(authority_id, &record);
    record.zeroize();
    result
  }

  fn remove(&mut self, authority_id: &str) -> Result<(), AuthorityError> {
    match Self::entry(authority_id)?.delete_credential() {
      Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
      Err(_) => Err(AuthorityError::CredentialStoreUnavailable),
    }
  }
}

/// Explicit advanced provider backed by an inherited, controller-owned descriptor.
pub struct InheritedFdCredentialProvider {
  authority_id: String,
  file: Option<File>,
}

impl InheritedFdCredentialProvider {
  #[cfg(unix)]
  fn new(authority_id: String, descriptor: i64) -> Result<Self, AuthorityError> {
    validate_authority_id(&authority_id)?;
    let descriptor = RawFd::try_from(descriptor).map_err(|_| AuthorityError::InvalidInheritedFd)?;
    if descriptor < 0 {
      return Err(AuthorityError::InvalidInheritedFd);
    }
    // SAFETY: F_GETFD validates the inherited descriptor before ownership is transferred.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags < 0 {
      return Err(AuthorityError::InvalidInheritedFd);
    }
    // SAFETY: the validated descriptor remains owned by the launcher until from_raw_fd below.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
      return Err(AuthorityError::InvalidInheritedFd);
    }
    // SAFETY: launcher contract transfers this validated descriptor to Tenet exactly once.
    let file = unsafe { File::from_raw_fd(descriptor) };
    Ok(Self {
      authority_id,
      file: Some(file),
    })
  }

  #[cfg(not(unix))]
  fn new(_authority_id: String, _descriptor: i64) -> Result<Self, AuthorityError> {
    Err(AuthorityError::InvalidInheritedFd)
  }

  fn take_credential(&mut self) -> Result<AuthorityCredential, AuthorityError> {
    let mut file = self.file.take().ok_or(AuthorityError::InvalidInheritedFd)?;
    let mut key_material = Zeroizing::new(Vec::with_capacity(AUTHORITY_SECRET_BYTES + 1));
    Read::by_ref(&mut file)
      .take((AUTHORITY_SECRET_BYTES + 1) as u64)
      .read_to_end(&mut key_material)
      .map_err(|_| AuthorityError::InvalidInheritedFd)?;
    if key_material.len() != AUTHORITY_SECRET_BYTES {
      return Err(AuthorityError::InvalidInheritedCredential);
    }
    AuthorityCredential::from_zeroizing(self.authority_id.clone(), key_material)
  }
}

impl CredentialProvider for InheritedFdCredentialProvider {
  fn kind(&self) -> CredentialProviderKind {
    CredentialProviderKind::InheritedFd
  }

  fn provision(&mut self) -> Result<AuthorityCredential, AuthorityError> {
    self.take_credential()
  }

  fn resolve(&mut self, authority_id: &str) -> Result<AuthorityCredential, AuthorityError> {
    if self.authority_id != authority_id {
      return Err(AuthorityError::IdentityMismatch);
    }
    self.take_credential()
  }

  fn remove(&mut self, _authority_id: &str) -> Result<(), AuthorityError> {
    Ok(())
  }
}

/// Production provider selection captured before any child process can inherit launcher inputs.
pub struct AuthorityBootstrap {
  os: OsCredentialProvider,
  inherited: Option<InheritedFdCredentialProvider>,
}

impl AuthorityBootstrap {
  /// Captures explicit inherited-FD selection and removes its environment selectors.
  pub fn from_environment() -> Result<Self, AuthorityError> {
    let namespace = std::env::var_os(AUTHORITY_NAMESPACE_ENV);
    let descriptor = std::env::var_os(AUTHORITY_KEY_FD_ENV);
    std::env::remove_var(AUTHORITY_NAMESPACE_ENV);
    std::env::remove_var(AUTHORITY_KEY_FD_ENV);
    let namespace = namespace
      .map(|value| {
        value
          .into_string()
          .map_err(|_| AuthorityError::InvalidInheritedFd)
      })
      .transpose()?;
    let descriptor = descriptor
      .map(|value| {
        value
          .into_string()
          .map_err(|_| AuthorityError::InvalidInheritedFd)
      })
      .transpose()?;
    let inherited = match (namespace, descriptor) {
      (None, None) => None,
      (Some(namespace), Some(descriptor)) => {
        let descriptor = descriptor
          .parse::<i64>()
          .map_err(|_| AuthorityError::InvalidInheritedFd)?;
        Some(InheritedFdCredentialProvider::new(namespace, descriptor)?)
      }
      _ => return Err(AuthorityError::PartialInheritedFdConfiguration),
    };
    Ok(Self {
      os: OsCredentialProvider,
      inherited,
    })
  }

  /// Provisions or validates the stable identity used by this repository.
  pub fn initialize(&mut self, cwd: &Path) -> Result<AuthorityInitialization, AuthorityError> {
    let directory = cwd.join(TENET_DIR);
    fs::create_dir_all(&directory).map_err(|error| metadata_error(cwd, error))?;
    let _lock = RunLock::acquire(cwd).map_err(|error| AuthorityError::Metadata {
      path: metadata_path(cwd),
      message: error.to_string(),
    })?;
    match read_metadata(cwd)? {
      Some(metadata) => {
        let provider = self.provider_for(metadata.provider)?;
        resolve_metadata(metadata, provider)
      }
      None => {
        let provider: &mut dyn CredentialProvider = match self.inherited.as_mut() {
          Some(inherited) => inherited,
          None => &mut self.os,
        };
        initialize_repository_authority(cwd, provider)
      }
    }
  }

  /// Resolves and installs the repository credential before controller-owned state is used.
  pub fn install(&mut self, cwd: &Path) -> Result<(), AuthorityError> {
    let metadata = read_metadata(cwd)?.ok_or(AuthorityError::NotInitialized)?;
    let provider = self.provider_for(metadata.provider)?;
    install_metadata(metadata, provider)
  }

  fn provider_for(
    &mut self,
    preferred: CredentialProviderKind,
  ) -> Result<&mut dyn CredentialProvider, AuthorityError> {
    if let Some(inherited) = self.inherited.as_mut() {
      return Ok(inherited);
    }
    match preferred {
      CredentialProviderKind::OsKeyring => Ok(&mut self.os),
      CredentialProviderKind::InheritedFd => Err(AuthorityError::InheritedFdRequired),
    }
  }
}

/// Initializes repository metadata through an injected credential provider.
pub fn initialize_repository_authority(
  cwd: &Path,
  provider: &mut dyn CredentialProvider,
) -> Result<AuthorityInitialization, AuthorityError> {
  if read_metadata(cwd)?.is_some() {
    return resolve_initialized_authority(cwd, provider);
  }
  let credential = provider.provision()?;
  let metadata = AuthorityMetadata {
    version: 1,
    authority_id: credential.authority_id.clone(),
    provider: provider.kind(),
    credential_fingerprint: credential_fingerprint(&credential),
  };
  if let Err(error) = write_metadata(cwd, &metadata) {
    let _ = provider.remove(&credential.authority_id);
    return Err(error);
  }
  Ok(AuthorityInitialization {
    authority_id: metadata.authority_id,
    provider: metadata.provider,
    created: true,
  })
}

/// Resolves repository metadata and validates it through an injected provider.
pub fn resolve_initialized_authority(
  cwd: &Path,
  provider: &mut dyn CredentialProvider,
) -> Result<AuthorityInitialization, AuthorityError> {
  let metadata = read_metadata(cwd)?.ok_or(AuthorityError::NotInitialized)?;
  resolve_metadata(metadata, provider)
}

/// Resolves and installs repository authority through an injected provider.
pub fn install_repository_authority(
  cwd: &Path,
  provider: &mut dyn CredentialProvider,
) -> Result<(), AuthorityError> {
  let metadata = read_metadata(cwd)?.ok_or(AuthorityError::NotInitialized)?;
  install_metadata(metadata, provider)
}

fn resolve_metadata(
  metadata: AuthorityMetadata,
  provider: &mut dyn CredentialProvider,
) -> Result<AuthorityInitialization, AuthorityError> {
  let credential = validated_credential(provider.resolve(&metadata.authority_id)?, &metadata)?;
  drop(credential);
  Ok(AuthorityInitialization {
    authority_id: metadata.authority_id,
    provider: metadata.provider,
    created: false,
  })
}

fn install_metadata(
  metadata: AuthorityMetadata,
  provider: &mut dyn CredentialProvider,
) -> Result<(), AuthorityError> {
  let credential = validated_credential(provider.resolve(&metadata.authority_id)?, &metadata)?;
  tenet_storage::install_controller_authority_key(
    &credential.authority_id,
    credential.key_material.as_slice(),
  )
  .map_err(|_| AuthorityError::ProcessIdentityMismatch)
}

fn validated_credential(
  credential: AuthorityCredential,
  metadata: &AuthorityMetadata,
) -> Result<AuthorityCredential, AuthorityError> {
  if credential.authority_id != metadata.authority_id
    || credential_fingerprint(&credential) != metadata.credential_fingerprint
  {
    return Err(AuthorityError::IdentityMismatch);
  }
  Ok(credential)
}

fn credential_fingerprint(credential: &AuthorityCredential) -> String {
  let mut digest = Sha256::new();
  digest.update(b"tenet-authority-credential-fingerprint-v1");
  digest.update((credential.authority_id.len() as u64).to_be_bytes());
  digest.update(credential.authority_id.as_bytes());
  digest.update((credential.key_material.len() as u64).to_be_bytes());
  digest.update(credential.key_material.as_slice());
  hex(&digest.finalize())
}

fn encode_os_record(authority_id: &str, key_material: &[u8]) -> Result<Vec<u8>, AuthorityError> {
  let id = Uuid::parse_str(authority_id).map_err(|_| AuthorityError::MalformedCredential)?;
  if key_material.len() != AUTHORITY_SECRET_BYTES {
    return Err(AuthorityError::MalformedCredential);
  }
  let mut record = Vec::with_capacity(CREDENTIAL_RECORD_PREFIX.len() + 16 + key_material.len());
  record.extend_from_slice(CREDENTIAL_RECORD_PREFIX);
  record.extend_from_slice(id.as_bytes());
  record.extend_from_slice(key_material);
  Ok(record)
}

fn decode_os_record(
  expected_authority_id: &str,
  record: &[u8],
) -> Result<AuthorityCredential, AuthorityError> {
  let expected_len = CREDENTIAL_RECORD_PREFIX.len() + 16 + AUTHORITY_SECRET_BYTES;
  if record.len() != expected_len || !record.starts_with(CREDENTIAL_RECORD_PREFIX) {
    return Err(AuthorityError::MalformedCredential);
  }
  let id_start = CREDENTIAL_RECORD_PREFIX.len();
  let id_end = id_start + 16;
  let id = Uuid::from_slice(&record[id_start..id_end])
    .map_err(|_| AuthorityError::MalformedCredential)?
    .to_string();
  if id != expected_authority_id {
    return Err(AuthorityError::IdentityMismatch);
  }
  AuthorityCredential::new(id, record[id_end..].to_vec())
}

fn validate_authority_id(authority_id: &str) -> Result<(), AuthorityError> {
  let bytes = authority_id.as_bytes();
  if bytes.is_empty()
    || bytes.len() > MAX_AUTHORITY_ID_BYTES
    || !bytes[0].is_ascii_alphanumeric()
    || !bytes
      .iter()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
  {
    return Err(AuthorityError::InvalidAuthorityId);
  }
  Ok(())
}

fn read_metadata(cwd: &Path) -> Result<Option<AuthorityMetadata>, AuthorityError> {
  let path = metadata_path(cwd);
  let file_type = match fs::symlink_metadata(&path) {
    Ok(metadata) if metadata.file_type().is_file() => metadata.file_type(),
    Ok(_) => {
      return Err(AuthorityError::Metadata {
        path,
        message: "expected a regular file".into(),
      })
    }
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(metadata_error(cwd, error)),
  };
  debug_assert!(file_type.is_file());
  let bytes = fs::read(&path).map_err(|error| metadata_error(cwd, error))?;
  let metadata: AuthorityMetadata =
    serde_json::from_slice(&bytes).map_err(|_| AuthorityError::Metadata {
      path: path.clone(),
      message: "invalid authority metadata format".into(),
    })?;
  if metadata.version != 1 {
    return Err(AuthorityError::Metadata {
      path,
      message: format!(
        "unsupported authority metadata version {}",
        metadata.version
      ),
    });
  }
  validate_authority_id(&metadata.authority_id).map_err(|_| AuthorityError::Metadata {
    path: metadata_path(cwd),
    message: "invalid repository authority identity".into(),
  })?;
  if metadata.credential_fingerprint.len() != 64
    || !metadata
      .credential_fingerprint
      .bytes()
      .all(|byte| byte.is_ascii_hexdigit())
  {
    return Err(AuthorityError::Metadata {
      path: metadata_path(cwd),
      message: "invalid credential binding".into(),
    });
  }
  Ok(Some(metadata))
}

fn write_metadata(cwd: &Path, metadata: &AuthorityMetadata) -> Result<(), AuthorityError> {
  let directory = cwd.join(TENET_DIR);
  fs::create_dir_all(&directory).map_err(|error| metadata_error(cwd, error))?;
  let path = metadata_path(cwd);
  let temporary_path = directory.join(format!(".{AUTHORITY_FILE}.{}.tmp", std::process::id()));
  let result = (|| {
    let mut file = OpenOptions::new()
      .create_new(true)
      .write(true)
      .open(&temporary_path)?;
    serde_json::to_writer_pretty(&mut file, metadata)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary_path, &path)?;
    Ok::<_, Box<dyn std::error::Error>>(())
  })();
  if let Err(error) = result {
    let _ = fs::remove_file(&temporary_path);
    return Err(AuthorityError::Metadata {
      path,
      message: error.to_string(),
    });
  }
  Ok(())
}

fn metadata_path(cwd: &Path) -> PathBuf {
  cwd.join(TENET_DIR).join(AUTHORITY_FILE)
}

fn metadata_error(cwd: &Path, error: std::io::Error) -> AuthorityError {
  AuthorityError::Metadata {
    path: metadata_path(cwd),
    message: error.to_string(),
  }
}

fn hex(bytes: &[u8]) -> String {
  let mut encoded = String::with_capacity(bytes.len() * 2);
  for byte in bytes {
    write!(encoded, "{byte:02x}").expect("writing to String cannot fail");
  }
  encoded
}

#[cfg(test)]
mod tests {
  use std::{collections::BTreeMap, io::Seek};

  use super::*;

  struct MemoryProvider {
    kind: CredentialProviderKind,
    next_id: String,
    next_key: Vec<u8>,
    credentials: BTreeMap<String, Vec<u8>>,
    provisions: usize,
  }

  impl MemoryProvider {
    fn os() -> Self {
      Self {
        kind: CredentialProviderKind::OsKeyring,
        next_id: "4aaf2199-123c-482a-99ea-1db20bcfe699".into(),
        next_key: b"memory-provider-authority-secret".to_vec(),
        credentials: BTreeMap::new(),
        provisions: 0,
      }
    }
  }

  impl CredentialProvider for MemoryProvider {
    fn kind(&self) -> CredentialProviderKind {
      self.kind
    }

    fn provision(&mut self) -> Result<AuthorityCredential, AuthorityError> {
      self.provisions += 1;
      self
        .credentials
        .insert(self.next_id.clone(), self.next_key.clone());
      AuthorityCredential::new(self.next_id.clone(), self.next_key.clone())
    }

    fn resolve(&mut self, authority_id: &str) -> Result<AuthorityCredential, AuthorityError> {
      let key = self
        .credentials
        .get(authority_id)
        .cloned()
        .ok_or(AuthorityError::MissingCredential)?;
      AuthorityCredential::new(authority_id.to_owned(), key)
    }

    fn remove(&mut self, authority_id: &str) -> Result<(), AuthorityError> {
      self.credentials.remove(authority_id);
      Ok(())
    }
  }

  #[test]
  fn init_then_resolve_uses_one_stable_repository_identity() {
    let repository = tempfile::tempdir().expect("temporary repository");
    let mut provider = MemoryProvider::os();

    let initialized = initialize_repository_authority(repository.path(), &mut provider)
      .expect("initialize authority");
    let resolved =
      resolve_initialized_authority(repository.path(), &mut provider).expect("resolve authority");

    assert_eq!(resolved.authority_id, initialized.authority_id);
  }

  #[test]
  fn repeated_init_resolves_without_reprovisioning() {
    let repository = tempfile::tempdir().expect("temporary repository");
    let mut provider = MemoryProvider::os();
    initialize_repository_authority(repository.path(), &mut provider)
      .expect("initialize authority");

    let result = initialize_repository_authority(repository.path(), &mut provider)
      .expect("repeat initialization");

    assert!(!result.created);
    assert_eq!(provider.provisions, 1);
  }

  #[test]
  fn authority_metadata_never_contains_secret_material() {
    let repository = tempfile::tempdir().expect("temporary repository");
    let mut provider = MemoryProvider::os();
    initialize_repository_authority(repository.path(), &mut provider)
      .expect("initialize authority");

    let metadata = fs::read_to_string(metadata_path(repository.path())).expect("read metadata");

    assert!(!metadata.contains("memory-provider-authority-secret"));
  }

  #[test]
  fn missing_credential_fails_closed_with_recovery_instruction() {
    let repository = tempfile::tempdir().expect("temporary repository");
    let mut provider = MemoryProvider::os();
    initialize_repository_authority(repository.path(), &mut provider)
      .expect("initialize authority");
    provider.credentials.clear();

    let error = resolve_initialized_authority(repository.path(), &mut provider)
      .expect_err("missing credential must fail");

    assert!(error.to_string().contains("remove `.tenet/`"));
  }

  #[test]
  fn changed_credential_is_rejected_before_storage_access() {
    let repository = tempfile::tempdir().expect("temporary repository");
    let mut provider = MemoryProvider::os();
    let initialized = initialize_repository_authority(repository.path(), &mut provider)
      .expect("initialize authority");
    provider.credentials.insert(
      initialized.authority_id,
      b"fedcba9876543210fedcba9876543210".to_vec(),
    );

    let error = resolve_initialized_authority(repository.path(), &mut provider)
      .expect_err("changed credential must fail");

    assert!(matches!(error, AuthorityError::IdentityMismatch));
  }

  #[test]
  fn malformed_os_record_is_rejected_without_returning_bytes() {
    let error = decode_os_record("4aaf2199-123c-482a-99ea-1db20bcfe699", b"not-a-record")
      .err()
      .expect("malformed record must fail");

    assert!(matches!(error, AuthorityError::MalformedCredential));
  }

  #[test]
  fn os_record_is_bound_to_the_expected_repository_identity() {
    let record = encode_os_record(
      "4aaf2199-123c-482a-99ea-1db20bcfe699",
      &[7_u8; AUTHORITY_SECRET_BYTES],
    )
    .expect("encode record");

    let error = decode_os_record("9e88af55-972a-48f1-ae00-c09939cc342f", &record)
      .err()
      .expect("wrong identity must fail");

    assert!(matches!(error, AuthorityError::IdentityMismatch));
  }
  #[test]
  fn inherited_authority_identity_rejects_terminal_control_characters() {
    let error = AuthorityCredential::new("ci-authority\nforged-output".into(), vec![7_u8; 32])
      .err()
      .expect("control characters must fail");

    assert!(matches!(error, AuthorityError::InvalidAuthorityId));
  }

  #[cfg(unix)]
  #[test]
  fn authority_metadata_symlink_is_rejected() {
    use std::os::unix::fs::symlink;

    let repository = tempfile::tempdir().expect("temporary repository");
    fs::create_dir_all(repository.path().join(TENET_DIR)).expect("create Tenet directory");
    let outside = repository.path().join("outside.json");
    fs::write(&outside, "{}").expect("write symlink target");
    symlink(&outside, metadata_path(repository.path())).expect("create authority symlink");
    let mut provider = MemoryProvider::os();

    let error = initialize_repository_authority(repository.path(), &mut provider)
      .expect_err("authority metadata symlink must fail");

    assert!(error.to_string().contains("expected a regular file"));
  }

  #[cfg(unix)]
  #[test]
  fn oversized_inherited_credential_is_rejected() {
    use std::os::fd::IntoRawFd;

    let file = tempfile::tempfile().expect("temporary credential descriptor");
    file
      .set_len((AUTHORITY_SECRET_BYTES + 1) as u64)
      .expect("size oversized credential");
    let mut provider =
      InheritedFdCredentialProvider::new("ci-authority".into(), i64::from(file.into_raw_fd()))
        .expect("capture inherited descriptor");

    let error = provider
      .provision()
      .err()
      .expect("oversized credential must fail");

    assert!(matches!(error, AuthorityError::InvalidInheritedCredential));
  }

  #[cfg(unix)]
  #[test]
  fn inherited_descriptor_is_quarantined_then_consumed_on_resolution() {
    use std::os::fd::{AsRawFd, IntoRawFd};

    let mut file = tempfile::tempfile().expect("temporary credential descriptor");
    file
      .write_all(b"0123456789abcdef0123456789abcdef")
      .expect("write credential");
    file.rewind().expect("rewind credential");
    let descriptor = file.as_raw_fd();
    let mut provider =
      InheritedFdCredentialProvider::new("ci-authority".into(), i64::from(file.into_raw_fd()))
        .expect("capture inherited descriptor");
    // SAFETY: F_GETFD only reads flags from the validated owned descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    assert_ne!(flags & libc::FD_CLOEXEC, 0);

    let credential = provider.provision().expect("read inherited credential");
    drop(credential);

    assert!(provider.file.is_none());
  }
}
