//! Concrete repository, content persistence, candidate snapshots, refs, and temporary state.

use std::{fs, fs::OpenOptions, io::ErrorKind, path::Path};

use anyhow::{Context, Result, bail};
use tenet_application::ports::{
  ContentStoreError, ExpectedEntry, InitObservation, IntegrityObservation, PathResolutionError,
  Repository, RepositoryLock, SnapshotHandle,
};
use tenet_domain::{
  algebra::CompletionContractV1,
  evidence::ContentObjectId,
  paths::{CONFIG_PATH, CONTRACT_PATH},
  policy::VerificationPolicy,
};

mod project;

use project::ContentStore;

#[derive(Clone, Copy, Debug, Default)]
pub struct LocalWorkspace;
struct LocalRepositoryLock {
  file: fs::File,
}

impl RepositoryLock for LocalRepositoryLock {}

impl Drop for LocalRepositoryLock {
  fn drop(&mut self) {
    #[cfg(unix)]
    // SAFETY: `flock` receives the live descriptor owned by this guard.
    unsafe {
      use std::os::fd::AsRawFd;
      libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
    }
  }
}

impl Repository for LocalWorkspace {
  fn initialize(&self, cwd: &Path, spec: Option<&Path>) -> Result<InitObservation> {
    let root = cwd
      .canonicalize()
      .context("canonicalize project directory")?;
    let spec = match spec {
      Some(path) if path.is_absolute() => path.to_path_buf(),
      Some(path) => root.join(path),
      None => root.join("SPEC.md"),
    };
    let (policy, spec_digest, created) = project::initialize(&root, &spec)?;
    Ok(InitObservation {
      root,
      policy,
      spec_digest,
      created,
    })
  }

  fn discover_root(&self, cwd: &Path) -> Result<std::path::PathBuf> {
    project::discover_root(cwd)
  }
  fn acquire_lock(&self, root: &Path) -> Result<Box<dyn RepositoryLock>> {
    let path = root.join(".tenet/lock");
    let file = OpenOptions::new()
      .read(true)
      .write(true)
      .create(true)
      .truncate(false)
      .open(path)?;
    #[cfg(unix)]
    // SAFETY: `flock` receives the live descriptor owned by `file`.
    unsafe {
      use std::os::fd::AsRawFd;
      if libc::flock(file.as_raw_fd(), libc::LOCK_EX) != 0 {
        return Err(std::io::Error::last_os_error().into());
      }
    }
    Ok(Box::new(LocalRepositoryLock { file }))
  }

  fn resolve_relative_path(
    &self,
    root: &Path,
    relative: &str,
    expected: ExpectedEntry,
  ) -> std::result::Result<std::path::PathBuf, PathResolutionError> {
    project::resolve_relative_path(root, relative, expected)
  }

  fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(anyhow::Error::new)
  }

  fn is_executable(&self, path: &Path) -> Result<bool> {
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      Ok(fs::symlink_metadata(path)?.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
      let _ = path;
      Ok(true)
    }
  }

  fn load_policy(&self, root: &Path) -> Result<VerificationPolicy> {
    project::load_policy(root)
  }

  fn specification_digest(&self, root: &Path, policy: &VerificationPolicy) -> Result<String> {
    project::specification_digest(root, policy)
  }

  fn stage_authority_surface(
    &self,
    root: &Path,
    policy: &VerificationPolicy,
    contract: &CompletionContractV1,
  ) -> Result<Box<dyn SnapshotHandle>> {
    ContentStore::open(root)?;
    let temporary_root = root.join(".tenet/tmp/authority");
    fs::create_dir_all(&temporary_root)?;
    let stage = tempfile::Builder::new()
      .prefix("authority-")
      .tempdir_in(temporary_root)?;
    copy_authority_surface(root, stage.path(), policy)?;
    project::atomic_write(
      root,
      &stage.path().join(CONTRACT_PATH),
      &serde_json::to_vec(contract)?,
    )?;
    Ok(Box::new(project::retain_snapshot(stage)))
  }

  fn capture(&self, project_root: &Path, source: &Path) -> Result<ContentObjectId> {
    ContentStore::open(project_root)?.capture(source)
  }

  fn capture_selected(
    &self,
    project_root: &Path,
    source: &Path,
    include: &[String],
    exclude: &[String],
  ) -> Result<ContentObjectId> {
    ContentStore::open(project_root)?.capture_selected(source, include, exclude)
  }

  fn materialize(
    &self,
    project_root: &Path,
    id: &ContentObjectId,
  ) -> std::result::Result<Box<dyn SnapshotHandle>, ContentStoreError> {
    let store = open_store(project_root, id)?;
    Ok(Box::new(store.materialize(id)?))
  }

  fn fresh_scratch(&self, project_root: &Path) -> Result<Box<dyn SnapshotHandle>> {
    let temporary_root = project_root.join(".tenet/tmp/scratch");
    fs::create_dir_all(&temporary_root)?;
    let directory = tempfile::Builder::new()
      .prefix("verifier-")
      .tempdir_in(temporary_root)?;
    Ok(Box::new(project::retain_snapshot(directory)))
  }

  fn fresh_output(&self, project_root: &Path) -> Result<Box<dyn SnapshotHandle>> {
    let temporary_root = project_root.join(".tenet/tmp/output");
    fs::create_dir_all(&temporary_root)?;
    let directory = tempfile::Builder::new()
      .prefix("verifier-")
      .tempdir_in(temporary_root)?;
    Ok(Box::new(project::retain_snapshot(directory)))
  }

  fn manifest(
    &self,
    project_root: &Path,
    id: &ContentObjectId,
  ) -> std::result::Result<tenet_domain::snapshot::TreeManifest, ContentStoreError> {
    open_store(project_root, id)?.manifest(id)
  }

  fn store_object(&self, root: &Path, bytes: &[u8]) -> Result<ContentObjectId> {
    project::store_object(root, bytes)
  }

  fn load_object(
    &self,
    root: &Path,
    id: &ContentObjectId,
  ) -> std::result::Result<Vec<u8>, ContentStoreError> {
    project::load_object(root, id)
  }

  fn read_ref(&self, root: &Path, name: &str) -> Result<Option<ContentObjectId>> {
    validate_ref_name(name)?;
    let relative = format!(".tenet/refs/{name}");
    let path = match project::resolve_relative_path(root, &relative, ExpectedEntry::File) {
      Ok(path) => path,
      Err(PathResolutionError::Missing { .. }) => return Ok(None),
      Err(error) => return Err(error.into()),
    };
    let text = fs::read_to_string(path)?;
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let id = ContentObjectId::new(text.to_owned()).map_err(anyhow::Error::msg)?;
    if id.0 != text {
      bail!("ref `{name}` must use canonical lowercase form");
    }
    project::load_object(root, &id).map_err(anyhow::Error::new)?;
    Ok(Some(id))
  }

  fn write_ref(&self, root: &Path, name: &str, id: &ContentObjectId) -> Result<()> {
    validate_ref_name(name)?;
    let normalized = ContentObjectId::new(id.0.clone()).map_err(anyhow::Error::msg)?;
    if normalized != *id {
      bail!("ref target must use canonical lowercase form");
    }
    project::load_object(root, id).map_err(anyhow::Error::new)?;
    project::atomic_write(
      root,
      &root.join(".tenet/refs").join(name),
      format!("{}\n", id.0).as_bytes(),
    )
  }

  fn remove_ref(&self, root: &Path, name: &str) -> Result<()> {
    validate_ref_name(name)?;
    let path = root.join(".tenet/refs").join(name);
    match fs::symlink_metadata(&path) {
      Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
        fs::remove_file(path)?;
      }
      Ok(_) => bail!("ref `{name}` is not a regular file"),
      Err(error) if error.kind() == ErrorKind::NotFound => {}
      Err(error) => return Err(error.into()),
    }
    Ok(())
  }

  fn list_refs(&self, root: &Path, prefix: &str) -> Result<Vec<(String, ContentObjectId)>> {
    validate_ref_name(prefix)?;
    let directory = root.join(".tenet/refs").join(prefix);
    match fs::symlink_metadata(&directory) {
      Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
      Ok(_) => bail!("ref prefix `{prefix}` is not a regular directory"),
      Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
      Err(error) => return Err(error.into()),
    }
    let mut refs = Vec::new();
    collect_refs(root, prefix, &directory, &mut refs)?;
    refs.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(refs)
  }

  fn inspect_integrity(&self, root: &Path) -> Result<IntegrityObservation> {
    let format = project::resolve_relative_path(root, ".tenet/format", ExpectedEntry::File)?;
    if fs::read_to_string(format)? != "1\n" {
      bail!("unsupported Tenet repository format");
    }
    for file in [".tenet/.gitignore", ".tenet/lock"] {
      project::resolve_relative_path(root, file, ExpectedEntry::File)?;
    }
    for directory in [
      ".tenet/objects",
      ".tenet/blobs",
      ".tenet/refs",
      ".tenet/refs/requirements",
      ".tenet/tmp",
    ] {
      project::resolve_relative_path(root, directory, ExpectedEntry::Directory)?;
    }
    let object_count = inspect_namespace(root, "objects")?;
    let blob_count = inspect_namespace(root, "blobs")?;
    let mut refs = Vec::new();
    collect_refs(root, "", &root.join(".tenet/refs"), &mut refs)?;
    Ok(IntegrityObservation {
      object_count,
      blob_count,
      ref_count: refs.len(),
    })
  }
}

fn open_store(
  project_root: &Path,
  id: &ContentObjectId,
) -> std::result::Result<ContentStore, ContentStoreError> {
  ContentStore::open(project_root)
    .map_err(|error| ContentStoreError::materialization(id, error.to_string()))
}

fn validate_ref_name(name: &str) -> Result<()> {
  project::validate_relative(name)?;
  if name == "." {
    bail!("ref name must not be the root directory");
  }
  Ok(())
}

fn collect_refs(
  root: &Path,
  prefix: &str,
  directory: &Path,
  refs: &mut Vec<(String, ContentObjectId)>,
) -> Result<()> {
  let mut entries = fs::read_dir(directory)?.collect::<std::result::Result<Vec<_>, _>>()?;
  entries.sort_by_key(|entry| entry.file_name());
  for entry in entries {
    let metadata = fs::symlink_metadata(entry.path())?;
    if metadata.file_type().is_symlink() {
      bail!("ref tree contains a symlink: {}", entry.path().display());
    }
    let name = entry.file_name().to_string_lossy().into_owned();
    let full = if prefix.is_empty() {
      name
    } else {
      format!("{prefix}/{name}")
    };
    if metadata.is_dir() {
      collect_refs(root, &full, &entry.path(), refs)?;
    } else if metadata.is_file() {
      let text = fs::read_to_string(entry.path())?;
      let text = text.strip_suffix('\n').unwrap_or(&text);
      let id = ContentObjectId::new(text.to_owned()).map_err(anyhow::Error::msg)?;
      project::load_object(root, &id).map_err(anyhow::Error::new)?;
      refs.push((full, id));
    } else {
      bail!(
        "ref tree contains a special entry: {}",
        entry.path().display()
      );
    }
  }
  Ok(())
}

fn inspect_namespace(root: &Path, namespace: &str) -> Result<usize> {
  let directory = root.join(".tenet").join(namespace);
  let mut entries = fs::read_dir(&directory)?.collect::<std::result::Result<Vec<_>, _>>()?;
  entries.sort_by_key(|entry| entry.file_name());
  for entry in &entries {
    let metadata = fs::symlink_metadata(entry.path())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
      bail!(
        "{namespace} contains a non-file entry: {}",
        entry.path().display()
      );
    }
    let name = entry.file_name().to_string_lossy().into_owned();
    let id = ContentObjectId::new(format!("sha256:{name}")).map_err(anyhow::Error::msg)?;
    let bytes = fs::read(entry.path())?;
    if tenet_kernel::digest::bytes_digest(&bytes) != id.0 {
      bail!("{namespace} content digest mismatch: {name}");
    }
  }
  Ok(entries.len())
}

fn copy_authority_surface(root: &Path, stage: &Path, policy: &VerificationPolicy) -> Result<()> {
  copy_authority_path(root, stage, CONFIG_PATH)?;
  copy_authority_path(root, stage, &policy.spec_path)?;
  for verifier in &policy.verifiers {
    if let Some(path) = &verifier.oracle_path {
      copy_authority_path(root, stage, path)?;
    }
  }
  Ok(())
}

fn copy_authority_path(root: &Path, stage: &Path, path: &str) -> Result<()> {
  let source = project::resolve_relative_path(root, path, ExpectedEntry::Any)?;
  let destination = stage.join(path);
  let metadata = fs::symlink_metadata(&source)?;
  if metadata.is_dir() {
    copy_directory(root, path, &destination)
  } else {
    if let Some(parent) = destination.parent() {
      fs::create_dir_all(parent)?;
    }
    fs::copy(source, destination)?;
    Ok(())
  }
}

fn copy_directory(root: &Path, relative: &str, destination: &Path) -> Result<()> {
  fs::create_dir_all(destination)?;
  let directory = project::resolve_relative_path(root, relative, ExpectedEntry::Directory)?;
  let mut entries = fs::read_dir(directory)?.collect::<std::result::Result<Vec<_>, _>>()?;
  entries.sort_by_key(|entry| entry.file_name());
  for entry in entries {
    let name = entry.file_name().to_string_lossy().into_owned();
    let child = format!("{relative}/{name}");
    let source = project::resolve_relative_path(root, &child, ExpectedEntry::Any)?;
    let target = destination.join(&name);
    if source.is_dir() {
      copy_directory(root, &child, &target)?;
    } else {
      fs::copy(source, target)?;
    }
  }
  Ok(())
}
