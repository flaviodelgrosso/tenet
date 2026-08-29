use std::{
  collections::BTreeSet,
  fs::{self, OpenOptions},
  io::{ErrorKind, Write},
  path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use tenet_domain::{
  digest::{bytes_digest, canonical_digest},
  evidence::ContentObjectId,
  policy::{ProjectConfig, VerificationPolicy, validate_policy},
};
use thiserror::Error;

pub const TENET_DIR: &str = ".tenet";
pub const CONFIG_PATH: &str = ".tenet/tenet.toml";
pub const CONTRACT_PATH: &str = ".tenet/contract.json";
pub const STATE_PATH: &str = ".tenet/state.json";
pub const SKILL_PATH: &str = ".agents/skills/tenet/SKILL.md";
const DEFAULT_SPECIFICATION: &str = "# Tenet completion specification\n\nDescribe the required behavior and acceptance criteria for this project.\n";

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
  fn integrity(id: &ContentObjectId, message: impl Into<String>) -> Self {
    Self::Integrity {
      id: id.0.clone(),
      message: message.into(),
    }
  }

  fn materialization(id: &ContentObjectId, message: impl Into<String>) -> Self {
    Self::MaterializationMessage {
      id: id.0.clone(),
      message: message.into(),
    }
  }

  fn materialization_io(id: &ContentObjectId, source: std::io::Error) -> Self {
    Self::Materialization {
      id: id.0.clone(),
      source,
    }
  }
}

pub fn resolve_relative_path(
  root: &Path,
  relative: &str,
  expected: ExpectedEntry,
) -> std::result::Result<PathBuf, PathResolutionError> {
  validate_relative(relative).map_err(|_| PathResolutionError::Invalid)?;
  let root = fs::canonicalize(root).map_err(|source| PathResolutionError::Io {
    path: root.display().to_string(),
    source,
  })?;
  let components = Path::new(relative)
    .components()
    .filter_map(|component| match component {
      Component::Normal(name) => Some(name),
      Component::CurDir => None,
      _ => None,
    })
    .collect::<Vec<_>>();
  let mut current = root.clone();
  for (index, name) in components.iter().enumerate() {
    let next = current.join(name);
    let path = next.strip_prefix(&root).map_or_else(
      |_| next.display().to_string(),
      |relative| relative.display().to_string(),
    );
    let metadata = match fs::symlink_metadata(&next) {
      Ok(metadata) => metadata,
      Err(source) if source.kind() == ErrorKind::NotFound => {
        return Err(PathResolutionError::Missing { path });
      }
      Err(source) => return Err(PathResolutionError::Io { path, source }),
    };
    if metadata.file_type().is_symlink() {
      return Err(PathResolutionError::UnsupportedSymlink { path });
    }
    if index + 1 != components.len() && !metadata.is_dir() {
      return Err(PathResolutionError::NotDirectory { path });
    }
    current = next;
  }
  let canonical = current
    .canonicalize()
    .map_err(|source| PathResolutionError::Io {
      path: relative.into(),
      source,
    })?;
  if !canonical.starts_with(&root) {
    return Err(PathResolutionError::PathEscape {
      path: relative.into(),
    });
  }
  let metadata = fs::symlink_metadata(&current).map_err(|source| PathResolutionError::Io {
    path: relative.into(),
    source,
  })?;
  if metadata.file_type().is_symlink() {
    return Err(PathResolutionError::UnsupportedSymlink {
      path: relative.into(),
    });
  }
  match expected {
    ExpectedEntry::Any if metadata.is_file() || metadata.is_dir() => {}
    ExpectedEntry::Any => {
      return Err(PathResolutionError::Special {
        path: relative.into(),
      });
    }
    ExpectedEntry::File if metadata.is_file() => {}
    ExpectedEntry::File => {
      return Err(PathResolutionError::NotFile {
        path: relative.into(),
      });
    }
    ExpectedEntry::Directory if metadata.is_dir() => {}
    ExpectedEntry::Directory => {
      return Err(PathResolutionError::NotDirectory {
        path: relative.into(),
      });
    }
  }
  Ok(current)
}
pub const SKILL: &str = r#"---
name: tenet
description: Use when completion is governed by a Tenet contract.
compatibility: Requires Tenet MCP tools.
metadata:
  tenet-skill-version: "1"
---

# Tenet workflow

Tenet judges immutable Candidate Snapshot R under independently sealed Authority Capsule A. Keep **authority construction** separate from **candidate engineering**.

## Authority construction (before A is sealed)

1. Inspect the specification and call `tenet_status`.
2. Before `tenet_contract_propose`, ensure `.tenet/tenet.toml` contains suitable verifier definitions for the evidence contracts the specification requires.
3. If no suitable verifier exists:
   - inspect the specification to determine the required observations;
   - call `tenet_policy_schema` and use its Rust-derived schema as the authoritative policy format;
   - edit `.tenet/tenet.toml` to define the required verifier(s);
   - when using `authority_snapshot`, create the required authority-owned oracle assets;
   - re-read `tenet_status` after every authority change;
   - propose the contract using those verifier IDs.

Editing verification policy and creating authority-owned oracle assets are **authority-definition work** and are allowed before A is sealed. Do not implement candidate product behavior during authority construction. Project-authority verifiers remain valid; choose verifier authority based on the evidence contract, not on a hard-coded technology stack. A weak configuration is surfaced through the returned verification profile and warnings; it is not a reason to invent stack-specific verifier choices or to implement the candidate first.

## Sealed-authority lifecycle

```text
inspect specification
→ construct verification authority if needed
→ tenet_contract_propose
→ explicit human approval of the exact proposal
→ tenet_contract_approve
→ tenet_authority_seal → authorityId A
→ explicit human selection of A
→ candidate engineering
→ tenet_candidate_capture → candidateId R
→ tenet_gate({ authorityId: A, candidateId: R })
```

Never claim completion until `tenet_gate` returns `done` for that exact `(A, R)` pair.

Project verifiers execute in R with `argv[0]` passed to the operating system process launcher. `authority_snapshot` verifiers execute from A-owned `oracle_path`; `argv[0]` directly names a bundled executable, `cwd` is bundle-relative, and `TENET_CANDIDATE_ROOT` exposes R.
"#;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TreeManifest {
  pub version: u32,
  pub entries: Vec<TreeEntry>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TreeEntry {
  pub path: String,
  pub kind: EntryKind,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub content_id: Option<ContentObjectId>,
  pub executable: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
  Directory,
  File,
}
pub struct MaterializedSnapshot {
  directory: TempDir,
}
impl MaterializedSnapshot {
  pub fn path(&self) -> &Path {
    self.directory.path()
  }
}
pub struct ContentStore {
  root: PathBuf,
}

impl ContentStore {
  pub fn open(project_root: &Path) -> Result<Self> {
    let project_root = project_root.canonicalize()?;
    let tenet = project_root.join(TENET_DIR);
    match fs::symlink_metadata(&tenet) {
      Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
      Ok(metadata) if metadata.file_type().is_symlink() => {
        return Err(anyhow::Error::new(
          PathResolutionError::UnsupportedSymlink {
            path: TENET_DIR.into(),
          },
        ));
      }
      Ok(_) => {
        return Err(anyhow::Error::new(PathResolutionError::NotDirectory {
          path: TENET_DIR.into(),
        }));
      }
      Err(error) if error.kind() == ErrorKind::NotFound => fs::create_dir_all(&tenet)?,
      Err(error) => return Err(error.into()),
    }
    let root = tenet.join("store").join("snapshots");
    for (relative, directory) in [
      (format!("{TENET_DIR}/store"), tenet.join("store")),
      (format!("{TENET_DIR}/store/snapshots"), root.clone()),
    ] {
      match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(metadata) if metadata.file_type().is_symlink() => {
          return Err(anyhow::Error::new(
            PathResolutionError::UnsupportedSymlink { path: relative },
          ));
        }
        Ok(_) => {
          return Err(anyhow::Error::new(PathResolutionError::NotDirectory {
            path: relative,
          }));
        }
        Err(error) if error.kind() == ErrorKind::NotFound => fs::create_dir_all(&directory)?,
        Err(error) => return Err(error.into()),
      }
    }
    Ok(Self { root })
  }

  pub fn capture<F>(&self, source: &Path, excluded: F) -> Result<ContentObjectId>
  where
    F: Fn(&str) -> bool,
  {
    let source_metadata = fs::symlink_metadata(source)?;
    if source_metadata.file_type().is_symlink() {
      bail!("unsupported symlink in snapshot root");
    }
    if !source_metadata.is_dir() {
      bail!("snapshot capture root must be a directory");
    }
    let source = source.canonicalize()?;
    let mut entries = Vec::new();
    collect(&source, &source, &excluded, &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = TreeManifest {
      version: 1,
      entries,
    };
    let id = content_id(canonical_digest(&manifest)?)?;
    let target = self.path_for(&id)?;
    match fs::symlink_metadata(&target) {
      Ok(_) => {
        self.verify(&id)?;
        return Ok(id);
      }
      Err(error) if error.kind() == ErrorKind::NotFound => {}
      Err(error) => return Err(error.into()),
    }
    let temporary = tempfile::Builder::new()
      .prefix("capture-")
      .tempdir_in(self.root.parent().context("store has no parent")?)?;
    let tree = temporary.path().join("tree");
    fs::create_dir(&tree)?;
    for entry in &manifest.entries {
      let destination = tree.join(&entry.path);
      match entry.kind {
        EntryKind::Directory => fs::create_dir_all(destination)?,
        EntryKind::File => {
          fs::create_dir_all(destination.parent().context("entry has no parent")?)?;
          let source_file = source.join(&entry.path);
          let metadata = fs::symlink_metadata(&source_file)?;
          if metadata.file_type().is_symlink() {
            bail!("unsupported symlink in snapshot: {}", entry.path);
          }
          if !metadata.is_file() {
            bail!("unsupported filesystem entry in snapshot: {}", entry.path);
          }
          fs::copy(source_file, &destination)?;
          set_executable(&destination, entry.executable)?;
        }
      }
    }
    atomic_write(
      &temporary.path().join("manifest.json"),
      &serde_json::to_vec(&manifest)?,
    )?;
    match fs::rename(temporary.path(), &target) {
      Ok(()) => {}
      Err(_) if target.exists() => {
        self.verify(&id)?;
      }
      Err(error) => return Err(error.into()),
    }
    self.verify(&id)?;
    Ok(id)
  }

  pub fn materialize(
    &self,
    id: &ContentObjectId,
  ) -> std::result::Result<MaterializedSnapshot, ContentStoreError> {
    let manifest = self.verify(id)?;
    let temp_root = self
      .root
      .parent()
      .ok_or_else(|| ContentStoreError::materialization(id, "store has no parent"))?
      .join("materialized");
    match fs::symlink_metadata(&temp_root) {
      Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
      Ok(metadata) if metadata.file_type().is_symlink() => {
        return Err(ContentStoreError::materialization(
          id,
          "materialization directory is a symlink",
        ));
      }
      Ok(_) => {
        return Err(ContentStoreError::materialization(
          id,
          "materialization path is not a directory",
        ));
      }
      Err(error) if error.kind() == ErrorKind::NotFound => {
        fs::create_dir_all(&temp_root)
          .map_err(|source| ContentStoreError::materialization_io(id, source))?;
      }
      Err(source) => return Err(ContentStoreError::materialization_io(id, source)),
    }
    let directory = tempfile::Builder::new()
      .prefix("snapshot-")
      .tempdir_in(temp_root)
      .map_err(|source| ContentStoreError::materialization_io(id, source))?;
    let tree = self
      .path_for(id)
      .map_err(|error| ContentStoreError::materialization(id, error.to_string()))?
      .join("tree");
    for entry in manifest.entries {
      let destination = directory.path().join(&entry.path);
      match entry.kind {
        EntryKind::Directory => fs::create_dir_all(destination)
          .map_err(|source| ContentStoreError::materialization_io(id, source))?,
        EntryKind::File => {
          fs::create_dir_all(
            destination
              .parent()
              .context("entry has no parent")
              .map_err(|error| ContentStoreError::materialization(id, error.to_string()))?,
          )
          .map_err(|source| ContentStoreError::materialization_io(id, source))?;
          let source_file = tree.join(&entry.path);
          let metadata = fs::symlink_metadata(&source_file)
            .map_err(|source| ContentStoreError::materialization_io(id, source))?;
          if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ContentStoreError::Integrity {
              id: id.0.clone(),
              message: format!("captured file changed: {}", entry.path),
            });
          }
          fs::copy(source_file, &destination)
            .map_err(|source| ContentStoreError::materialization_io(id, source))?;
          set_executable(&destination, entry.executable)
            .map_err(|error| ContentStoreError::materialization(id, error.to_string()))?;
        }
      }
    }
    Ok(MaterializedSnapshot { directory })
  }

  pub fn manifest(
    &self,
    id: &ContentObjectId,
  ) -> std::result::Result<TreeManifest, ContentStoreError> {
    self.verify(id)
  }

  fn verify(&self, id: &ContentObjectId) -> std::result::Result<TreeManifest, ContentStoreError> {
    let path = self
      .path_for(id)
      .map_err(|error| ContentStoreError::integrity(id, error.to_string()))?;
    match fs::symlink_metadata(&path) {
      Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
      Ok(_) => {
        return Err(ContentStoreError::Integrity {
          id: id.0.clone(),
          message: "content object is not a directory".into(),
        });
      }
      Err(error) if error.kind() == ErrorKind::NotFound => {
        return Err(ContentStoreError::Missing { id: id.0.clone() });
      }
      Err(error) => return Err(ContentStoreError::integrity(id, error.to_string())),
    }
    let manifest_path = path.join("manifest.json");
    let manifest_metadata = fs::symlink_metadata(&manifest_path).map_err(|error| {
      ContentStoreError::integrity(id, format!("manifest is unavailable: {error}"))
    })?;
    if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
      return Err(ContentStoreError::integrity(
        id,
        "manifest is not a regular file",
      ));
    }
    let manifest_bytes = fs::read(&manifest_path)
      .map_err(|error| ContentStoreError::integrity(id, format!("read manifest: {error}")))?;
    let manifest: TreeManifest = serde_json::from_slice(&manifest_bytes)
      .map_err(|error| ContentStoreError::integrity(id, format!("manifest is invalid: {error}")))?;
    if manifest.version != 1
      || content_id(
        canonical_digest(&manifest)
          .map_err(|error| ContentStoreError::integrity(id, error.to_string()))?,
      )
      .map_err(|error| ContentStoreError::integrity(id, error.to_string()))?
        != *id
    {
      return Err(ContentStoreError::Integrity {
        id: id.0.clone(),
        message: "manifest digest or version does not match object ID".into(),
      });
    }
    if manifest
      .entries
      .windows(2)
      .any(|entries| entries[0].path >= entries[1].path)
    {
      return Err(ContentStoreError::integrity(
        id,
        "manifest entries are not strictly sorted",
      ));
    }
    let tree = path.join("tree");
    let tree_metadata = fs::symlink_metadata(&tree)
      .map_err(|error| ContentStoreError::integrity(id, format!("tree is unavailable: {error}")))?;
    if tree_metadata.file_type().is_symlink() || !tree_metadata.is_dir() {
      return Err(ContentStoreError::integrity(
        id,
        "content tree is not a regular directory",
      ));
    }
    let mut actual_paths = BTreeSet::new();
    collect_tree_paths(&tree, "", &mut actual_paths).map_err(|error| {
      ContentStoreError::integrity(id, format!("inspect content tree: {error}"))
    })?;
    let expected_paths = manifest
      .entries
      .iter()
      .map(|entry| entry.path.as_str())
      .collect::<BTreeSet<_>>();
    if actual_paths
      .iter()
      .map(String::as_str)
      .collect::<BTreeSet<_>>()
      != expected_paths
    {
      return Err(ContentStoreError::integrity(
        id,
        "content tree contains entries not described by the manifest",
      ));
    }
    let mut object_entries = fs::read_dir(&path)
      .map_err(|error| ContentStoreError::integrity(id, error.to_string()))?
      .collect::<std::result::Result<Vec<_>, _>>()
      .map_err(|error| ContentStoreError::integrity(id, error.to_string()))?;
    object_entries.sort_by_key(|entry| entry.file_name());
    for entry in object_entries {
      let name = entry.file_name();
      if name != "manifest.json" && name != "tree" {
        return Err(ContentStoreError::integrity(
          id,
          format!(
            "unexpected content object entry: {}",
            name.to_string_lossy()
          ),
        ));
      }
      if fs::symlink_metadata(entry.path())
        .map_err(|error| ContentStoreError::integrity(id, error.to_string()))?
        .file_type()
        .is_symlink()
      {
        return Err(ContentStoreError::integrity(
          id,
          format!(
            "content object entry is a symlink: {}",
            name.to_string_lossy()
          ),
        ));
      }
    }
    for entry in &manifest.entries {
      validate_relative(&entry.path)
        .map_err(|error| ContentStoreError::integrity(id, error.to_string()))?;
      let file = path.join("tree").join(&entry.path);
      let metadata = fs::symlink_metadata(&file).map_err(|error| {
        ContentStoreError::integrity(
          id,
          format!("entry `{}` is unavailable: {error}", entry.path),
        )
      })?;
      if metadata.file_type().is_symlink() {
        return Err(ContentStoreError::Integrity {
          id: id.0.clone(),
          message: format!("entry `{}` is a symlink", entry.path),
        });
      }
      match entry.kind {
        EntryKind::Directory
          if metadata.is_dir() && entry.content_id.is_none() && !entry.executable => {}
        EntryKind::File if metadata.is_file() => {
          let content = fs::read(&file).map_err(|error| {
            ContentStoreError::integrity(id, format!("read `{}`: {error}", entry.path))
          })?;
          let content_id = content_id(bytes_digest(&content))
            .map_err(|error| ContentStoreError::integrity(id, error.to_string()))?;
          if entry.content_id.as_ref() != Some(&content_id)
            || executable(&metadata) != entry.executable
          {
            return Err(ContentStoreError::Integrity {
              id: id.0.clone(),
              message: format!("captured file digest or mode mismatch: {}", entry.path),
            });
          }
        }
        _ => {
          return Err(ContentStoreError::Integrity {
            id: id.0.clone(),
            message: format!("entry kind mismatch: {}", entry.path),
          });
        }
      }
    }
    Ok(manifest)
  }

  fn path_for(&self, id: &ContentObjectId) -> Result<PathBuf> {
    let normalized = ContentObjectId::new(id.0.clone()).map_err(anyhow::Error::msg)?;
    if normalized != *id {
      bail!("content object ID must use lowercase canonical form");
    }
    Ok(
      self.root.join(
        id.0
          .strip_prefix("sha256:")
          .context("invalid content object ID")?,
      ),
    )
  }
}
fn collect_tree_paths(directory: &Path, prefix: &str, paths: &mut BTreeSet<String>) -> Result<()> {
  let mut children = fs::read_dir(directory)?.collect::<std::result::Result<Vec<_>, _>>()?;
  children.sort_by_key(|item| item.file_name());
  for child in children {
    let name = child
      .file_name()
      .to_str()
      .context("content tree paths must be UTF-8")?
      .to_owned();
    let relative = if prefix.is_empty() {
      name.to_owned()
    } else {
      format!("{prefix}/{name}")
    };
    validate_relative(&relative)?;
    let metadata = fs::symlink_metadata(child.path())?;
    if metadata.file_type().is_symlink() {
      bail!("content tree contains a symlink: {relative}");
    }
    if metadata.is_dir() {
      paths.insert(relative.clone());
      collect_tree_paths(&child.path(), &relative, paths)?;
    } else if metadata.is_file() {
      paths.insert(relative);
    } else {
      bail!("content tree contains an unsupported entry");
    }
  }
  Ok(())
}

fn collect<F>(
  root: &Path,
  directory: &Path,
  excluded: &F,
  entries: &mut Vec<TreeEntry>,
) -> Result<()>
where
  F: Fn(&str) -> bool,
{
  let mut children = fs::read_dir(directory)?.collect::<std::result::Result<Vec<_>, _>>()?;
  children.sort_by_key(|item| item.file_name());
  for child in children {
    let path = child.path();
    let relative = normalized_relative(path.strip_prefix(root).context("snapshot escaped root")?)?;
    if excluded(&relative) {
      continue;
    }
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() {
      bail!("unsupported symlink in snapshot: {relative}");
    }
    if metadata.is_dir() {
      entries.push(TreeEntry {
        path: relative.clone(),
        kind: EntryKind::Directory,
        content_id: None,
        executable: false,
      });
      collect(root, &path, excluded, entries)?;
    } else if metadata.is_file() {
      entries.push(TreeEntry {
        path: relative,
        kind: EntryKind::File,
        content_id: Some(content_id(bytes_digest(&fs::read(&path)?))?),
        executable: executable(&metadata),
      });
    } else {
      bail!("unsupported filesystem entry in snapshot: {relative}");
    }
  }
  Ok(())
}
fn normalized_relative(path: &Path) -> Result<String> {
  let value = path
    .to_str()
    .context("snapshot paths must be UTF-8")?
    .to_owned();
  #[cfg(windows)]
  let value = value.replace('\\', "/");
  validate_relative(&value)?;
  Ok(value)
}
pub fn validate_relative(value: &str) -> Result<()> {
  let path = Path::new(value);
  if value.trim().is_empty()
    || value.contains('\\')
    || path.is_absolute()
    || path
      .components()
      .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
  {
    bail!("path must be a non-empty safe relative path");
  }
  Ok(())
}
fn content_id(value: String) -> Result<ContentObjectId> {
  ContentObjectId::new(value).map_err(anyhow::Error::msg)
}
#[cfg(unix)]
fn executable(metadata: &fs::Metadata) -> bool {
  use std::os::unix::fs::PermissionsExt;
  metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_: &fs::Metadata) -> bool {
  false
}
#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> Result<()> {
  use std::os::unix::fs::PermissionsExt;
  let mut permissions = fs::metadata(path)?.permissions();
  let mode = permissions.mode();
  permissions.set_mode(if executable {
    mode | 0o111
  } else {
    mode & !0o111
  });
  fs::set_permissions(path, permissions)?;
  Ok(())
}
#[cfg(not(unix))]
fn set_executable(_: &Path, _: bool) -> Result<()> {
  Ok(())
}

pub fn discover_root(cwd: &Path) -> Result<PathBuf> {
  let mut path = cwd.canonicalize()?;
  loop {
    match resolve_relative_path(&path, CONFIG_PATH, ExpectedEntry::File) {
      Ok(_) => return Ok(path),
      Err(PathResolutionError::Missing { .. }) => {}
      Err(error) => return Err(anyhow::Error::new(error)),
    }
    if !path.pop() {
      bail!("no initialized Tenet project encloses the working directory");
    }
  }
}
pub fn prepare_relative_directory(root: &Path, relative: &str) -> Result<PathBuf> {
  validate_relative(relative)?;
  let mut current = root.to_path_buf();
  for component in Path::new(relative).components() {
    let Component::Normal(name) = component else {
      continue;
    };
    let next = current.join(name);
    match fs::symlink_metadata(&next) {
      Ok(metadata) if metadata.file_type().is_symlink() => {
        bail!("unsupported symlink in path: {relative}");
      }
      Ok(metadata) if metadata.is_dir() => current = next,
      Ok(_) => bail!("path component is not a directory: {relative}"),
      Err(error) if error.kind() == ErrorKind::NotFound => {
        return Ok(root.join(relative));
      }
      Err(error) => return Err(error.into()),
    }
  }
  Ok(current)
}
pub fn initialize(root: &Path, spec: &Path) -> Result<(ProjectConfig, String, bool)> {
  let spec_relative = if spec.is_absolute() {
    match spec.strip_prefix(root) {
      Ok(relative) => relative.to_path_buf(),
      Err(_) => {
        let canonical_root = root.canonicalize()?;
        spec
          .strip_prefix(canonical_root)
          .context("specification must be inside the Tenet project")?
          .to_path_buf()
      }
    }
  } else {
    spec.to_path_buf()
  };
  let root = root.canonicalize()?;
  let spec_path = normalized_relative(&spec_relative)?;
  let spec = root.join(&spec_path);
  let tenet_dir = prepare_relative_directory(&root, TENET_DIR)?;
  let agents_dir = prepare_relative_directory(&root, ".agents/skills/tenet")?;
  let config_path = root.join(CONFIG_PATH);
  let created = match fs::symlink_metadata(&config_path) {
    Ok(metadata) if metadata.file_type().is_symlink() => {
      bail!("unsupported symlink in Tenet configuration");
    }
    Ok(metadata) if metadata.is_file() => false,
    Ok(_) => bail!("Tenet configuration is not a regular file"),
    Err(error) if error.kind() == ErrorKind::NotFound => true,
    Err(error) => return Err(error.into()),
  };
  let existing = (!created).then(|| load_policy(&root)).transpose()?;
  if !spec.exists() {
    let parent = Path::new(&spec_path)
      .parent()
      .filter(|path| !path.as_os_str().is_empty())
      .unwrap_or_else(|| Path::new("."));
    let parent = parent
      .to_str()
      .context("specification path must be UTF-8")?;
    let parent_path = prepare_relative_directory(&root, parent)?;
    fs::create_dir_all(parent_path)?;
    atomic_write(&spec, DEFAULT_SPECIFICATION.as_bytes())?;
  }
  resolve_relative_path(&root, &spec_path, ExpectedEntry::File).map_err(anyhow::Error::new)?;
  if let Some(policy) = &existing
    && policy.spec_path != spec_path
  {
    bail!(
      "project is already initialized for specification `{}`",
      policy.spec_path
    );
  }
  fs::create_dir_all(&tenet_dir)?;
  fs::create_dir_all(&agents_dir)?;
  let policy = existing.unwrap_or(ProjectConfig {
    version: 1,
    spec_path,
    candidate: Default::default(),
    verifiers: Vec::new(),
  });
  if created {
    atomic_write(&config_path, toml::to_string_pretty(&policy)?.as_bytes())?;
  }
  atomic_write(
    &root.join(TENET_DIR).join(".gitignore"),
    b"state.json\nproposals/\nstore/\n",
  )?;
  atomic_write(&root.join(SKILL_PATH), SKILL.as_bytes())?;
  initialize_mcp_configuration(&root)?;
  Ok((
    policy.clone(),
    specification_digest(&root, &policy)?,
    created,
  ))
}
fn initialize_mcp_configuration(root: &Path) -> Result<()> {
  let path = root.join(".mcp.json");
  match fs::symlink_metadata(&path) {
    Ok(metadata) if metadata.file_type().is_symlink() => {
      bail!("unsupported symlink in MCP configuration");
    }
    Ok(_) => {}
    Err(error) if error.kind() == ErrorKind::NotFound => {}
    Err(error) => return Err(error.into()),
  }
  let entry = serde_json::json!({"command":"tenet","args":["mcp"]});
  let mut config: serde_json::Value = match fs::read(&path) {
    Ok(value) => serde_json::from_slice(&value)?,
    Err(error) if error.kind() == ErrorKind::NotFound => serde_json::json!({}),
    Err(error) => return Err(error.into()),
  };
  let object = config
    .as_object_mut()
    .context("MCP configuration must be an object")?;
  let servers = object
    .entry("mcpServers")
    .or_insert_with(|| serde_json::json!({}))
    .as_object_mut()
    .context("mcpServers must be an object")?;
  match servers.get("tenet") {
    Some(value) if value == &entry => {}
    Some(_) => bail!("conflicting tenet MCP entry"),
    None => {
      servers.insert("tenet".into(), entry);
    }
  }
  atomic_write(
    &path,
    format!("{}\n", serde_json::to_string_pretty(&config)?).as_bytes(),
  )
}
pub fn load_policy(root: &Path) -> Result<VerificationPolicy> {
  let path =
    resolve_relative_path(root, CONFIG_PATH, ExpectedEntry::File).map_err(anyhow::Error::new)?;
  let policy: VerificationPolicy = toml::from_str(&fs::read_to_string(path)?)?;
  validate_policy(&policy)?;
  Ok(policy)
}
pub fn specification_digest(root: &Path, policy: &VerificationPolicy) -> Result<String> {
  let path = resolve_relative_path(root, &policy.spec_path, ExpectedEntry::File)
    .map_err(anyhow::Error::new)?;
  Ok(bytes_digest(&fs::read(path)?))
}
pub fn policy_digest(policy: &VerificationPolicy) -> Result<String> {
  canonical_digest(policy).context("hash policy")
}
fn ensure_directory_without_symlinks(path: &Path) -> Result<()> {
  let mut current = if path.is_absolute() {
    PathBuf::from(std::path::MAIN_SEPARATOR.to_string())
  } else {
    PathBuf::new()
  };
  for component in path.components() {
    let Component::Normal(name) = component else {
      continue;
    };
    let next = current.join(name);
    match fs::symlink_metadata(&next) {
      Ok(metadata) if metadata.file_type().is_symlink() => {
        bail!("unsupported symlink in directory path: {}", path.display());
      }
      Ok(metadata) if metadata.is_dir() => current = next,
      Ok(_) => bail!("directory path is not a directory: {}", path.display()),
      Err(error) if error.kind() == ErrorKind::NotFound => {
        fs::create_dir(&next)?;
        current = next;
      }
      Err(error) => return Err(error.into()),
    }
  }
  Ok(())
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
  let parent = path.parent().context("output path has no parent")?;
  ensure_directory_without_symlinks(parent)?;
  match fs::symlink_metadata(path) {
    Ok(metadata) if metadata.file_type().is_symlink() => {
      bail!("unsupported symlink in output path: {}", path.display());
    }
    Ok(metadata) if !metadata.is_file() => {
      bail!("output path is not a regular file: {}", path.display());
    }
    Ok(_) => {}
    Err(error) if error.kind() == ErrorKind::NotFound => {}
    Err(error) => return Err(error.into()),
  }
  let temp = parent.join(format!(
    ".{}.tmp-{}",
    path
      .file_name()
      .and_then(|name| name.to_str())
      .unwrap_or("tenet"),
    std::process::id()
  ));
  let mut file = OpenOptions::new()
    .create_new(true)
    .write(true)
    .open(&temp)?;
  file.write_all(bytes)?;
  file.sync_all()?;
  fs::rename(temp, path)?;
  Ok(())
}
#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn snapshots_are_location_independent_and_bind_content() {
    let project = tempfile::tempdir().expect("project");
    let left = tempfile::tempdir().expect("left");
    let right = tempfile::tempdir().expect("right");
    fs::write(left.path().join("file"), "one").expect("left");
    fs::write(right.path().join("file"), "one").expect("right");
    let store = ContentStore::open(project.path()).expect("store");
    let initial = store.capture(left.path(), |_| false).expect("capture left");
    let file = fs::File::open(left.path().join("file")).expect("open file");
    file
      .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
      .expect("change mtime");
    assert_eq!(
      initial,
      store
        .capture(left.path(), |_| false)
        .expect("capture metadata-only change")
    );
    assert_eq!(
      initial,
      store
        .capture(right.path(), |_| false)
        .expect("capture right")
    );
    fs::write(left.path().join("file"), "two").expect("mutate");
    assert_ne!(
      initial,
      store
        .capture(left.path(), |_| false)
        .expect("capture mutation")
    );
  }
  #[cfg(unix)]
  #[test]
  fn executable_state_and_path_change_snapshot_identity() {
    use std::os::unix::fs::PermissionsExt;

    let project = tempfile::tempdir().expect("project");
    let source = tempfile::tempdir().expect("source");
    let file = source.path().join("file");
    fs::write(&file, "same").expect("file");
    let store = ContentStore::open(project.path()).expect("store");
    let initial = store.capture(source.path(), |_| false).expect("capture");
    let mut permissions = fs::metadata(&file).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&file, permissions).expect("chmod");
    assert_ne!(
      initial,
      store
        .capture(source.path(), |_| false)
        .expect("executable capture")
    );
    fs::rename(&file, source.path().join("renamed")).expect("rename");
    assert_ne!(
      initial,
      store
        .capture(source.path(), |_| false)
        .expect("renamed capture")
    );
  }

  #[test]
  fn snapshots_reject_symlinks_and_corruption() {
    let project = tempfile::tempdir().expect("project");
    let source = tempfile::tempdir().expect("source");
    fs::write(source.path().join("file"), "one").expect("file");
    let store = ContentStore::open(project.path()).expect("store");
    let id = store.capture(source.path(), |_| false).expect("capture");
    let object = project
      .path()
      .join(TENET_DIR)
      .join("store")
      .join("snapshots")
      .join(id.0.strip_prefix("sha256:").expect("digest"));
    fs::write(object.join("tree/file"), "corrupt").expect("corrupt");
    assert!(store.materialize(&id).is_err());
    #[cfg(unix)]
    {
      std::os::unix::fs::symlink("/outside", source.path().join("link")).expect("link");
      assert!(store.capture(source.path(), |_| false).is_err());
    }
  }

  #[test]
  fn initialization_accepts_a_new_nested_specification_path() {
    let project = tempfile::tempdir().expect("project");
    let spec = project.path().join("docs/SPEC.md");
    let (policy, _, created) = initialize(project.path(), &spec).expect("initialize");
    assert!(created);
    assert_eq!(policy.spec_path, "docs/SPEC.md");
    assert!(spec.is_file());
  }

  #[test]
  fn content_objects_reject_unmanifested_tree_entries() {
    let project = tempfile::tempdir().expect("project");
    let source = tempfile::tempdir().expect("source");
    fs::write(source.path().join("file"), "one").expect("file");
    let store = ContentStore::open(project.path()).expect("store");
    let id = store.capture(source.path(), |_| false).expect("capture");
    let object = project
      .path()
      .join(TENET_DIR)
      .join("store")
      .join("snapshots")
      .join(id.0.strip_prefix("sha256:").expect("digest"));
    fs::write(object.join("tree/unmanifested"), "unexpected").expect("extra");
    assert!(matches!(
      store.materialize(&id),
      Err(ContentStoreError::Integrity { .. })
    ));
  }

  #[cfg(unix)]
  #[test]
  fn relative_resolution_rejects_symlink_escape() {
    let project = tempfile::tempdir().expect("project");
    let external = tempfile::tempdir().expect("external");
    fs::write(external.path().join("file"), "outside").expect("external file");
    std::os::unix::fs::symlink(external.path(), project.path().join("link")).expect("link");
    assert!(matches!(
      resolve_relative_path(project.path(), "link/file", ExpectedEntry::File),
      Err(PathResolutionError::UnsupportedSymlink { .. })
    ));
  }
}
