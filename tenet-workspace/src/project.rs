use std::{
  collections::BTreeSet,
  fs,
  io::{ErrorKind, Write},
  path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use tempfile::TempDir;
use tenet_application::ports::{ContentStoreError, ExpectedEntry, PathResolutionError};
use tenet_domain::{
  evidence::ContentObjectId,
  paths::{CONFIG_PATH, SKILL_PATH, TENET_DIR},
  policy::{ProjectConfig, VerificationPolicy},
  snapshot::{
    AUTHORITY_SNAPSHOT_SEMANTICS_V1, CANDIDATE_SEMANTICS_V1, EntryKind, SnapshotSemanticsId,
    TreeEntry, TreeManifest,
  },
};
use tenet_kernel::{
  digest::{bytes_digest, canonical_digest},
  policy::{candidate_path_is_reserved, validate_policy},
};

const DEFAULT_SPECIFICATION: &str = "# Tenet completion specification\n\nDescribe the required behavior and acceptance criteria for this project.\n";
const FORMAT_VERSION: &str = "1\n";

pub const SKILL: &str = r#"---
name: tenet
description: Use when completion is governed by the four-operation Tenet protocol.
compatibility: Requires Tenet MCP tools.
metadata:
  tenet-skill-version: "1"
---

# Tenet protocol

Call `tenet_context` first. It derives phase from immutable objects and refs; phase is never persisted.

Authority construction uses `tenet_authority_submit` with the exact staged lifecycle:

```text
PROPOSAL → RECONCILIATION → CLARIFICATION (when needed) → ADMISSION
```

Each transition binds exact content identities. Existence, a mutable ref, MCP user input, or an agent assertion is not admission. `ADMISSION` additionally requires a trusted admission grant bound to the exact proposal and authority; the producer cannot mint it, and a process without the trusted admission secret cannot admit.

During implementation, call `tenet_requirement_check` for one requirement. Its Candidate-specific Evaluation is development evidence only and cannot establish terminal completion.

Call `tenet_verify` for final verification. It captures one Candidate, reruns every required verifier with a fresh Candidate materialization per verifier, persists one Final Evaluation, and alone may return `DONE`. If the repository changes after successful verification, the successful Evaluation remains historical for the verified Candidate and the protocol returns `INCONCLUSIVE` with `CANDIDATE_CHANGED_DURING_VERIFICATION`.

The complete lifecycle is also available through the `tenet` CLI with identical kernel semantics; MCP is an optional adapter.

Trust boundaries:

- `LOCAL_V1` is not same-user tamper resistance.
- `PROTECTED_V1` means the runner enforced read-only Candidate/Authority views, separate writable scratch, and controlled output; the runner fails closed with an infrastructure result when the platform cannot enforce it and never downgrades to `LOCAL_V1`.
- `AuthorityBound` is not independent authorship.
- fresh materialization is not sandboxing.
- content addressing is not writer authentication.
- MCP user input is not cryptographic human identity.
- verifier `Pass` is not task completion; only kernel evaluation of the full admitted contract can produce `DONE`.
"#;

pub struct MaterializedSnapshot {
  directory: TempDir,
}

pub(crate) fn retain_snapshot(directory: TempDir) -> MaterializedSnapshot {
  MaterializedSnapshot { directory }
}

impl tenet_application::ports::SnapshotHandle for MaterializedSnapshot {
  fn path(&self) -> &Path {
    self.directory.path()
  }
}

pub struct ContentStore {
  project_root: PathBuf,
}

impl ContentStore {
  pub fn open(project_root: &Path) -> Result<Self> {
    let project_root = project_root.canonicalize()?;
    for relative in [
      TENET_DIR,
      ".tenet/objects",
      ".tenet/blobs",
      ".tenet/refs",
      ".tenet/refs/requirements",
      ".tenet/tmp",
    ] {
      let path = project_root.join(relative);
      ensure_directory_without_symlinks(&path)?;
    }
    Ok(Self { project_root })
  }

  pub fn capture(&self, source: &Path) -> Result<ContentObjectId> {
    let source = capture_root(source)?;
    let mut paths = BTreeSet::new();
    collect_paths(&source, &source, true, &mut paths)?;
    self.capture_paths(&source, paths, AUTHORITY_SNAPSHOT_SEMANTICS_V1)
  }

  pub fn capture_selected(
    &self,
    source: &Path,
    includes: &[String],
    excludes: &[String],
  ) -> Result<ContentObjectId> {
    let source = capture_root(source)?;
    let mut paths = BTreeSet::new();
    for selector in includes {
      collect_selector(&source, selector, &mut paths)?;
    }
    paths.retain(|path| {
      !candidate_path_is_reserved(path) && !excludes.iter().any(|rule| selector_matches(rule, path))
    });
    self.capture_paths(&source, paths, CANDIDATE_SEMANTICS_V1)
  }

  fn capture_paths(
    &self,
    source: &Path,
    paths: BTreeSet<String>,
    semantics: &str,
  ) -> Result<ContentObjectId> {
    let mut entries = Vec::with_capacity(paths.len());
    for path in paths {
      let absolute = source.join(&path);
      let metadata = fs::symlink_metadata(&absolute)?;
      if metadata.file_type().is_symlink() {
        bail!("unsupported symlink in snapshot: {path}");
      }
      if metadata.is_dir() {
        if semantics == AUTHORITY_SNAPSHOT_SEMANTICS_V1 {
          entries.push(TreeEntry {
            path,
            kind: EntryKind::Directory,
            content_id: None,
            executable: false,
          });
        }
      } else if metadata.is_file() {
        let bytes = fs::read(&absolute)?;
        let content_id = self.store_blob(&bytes)?;
        entries.push(TreeEntry {
          path,
          kind: EntryKind::File,
          content_id: Some(content_id),
          executable: executable(&metadata),
        });
      } else {
        bail!("unsupported filesystem entry in snapshot: {path}");
      }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest = TreeManifest {
      version: 1,
      semantics: SnapshotSemanticsId(semantics.into()),
      entries,
    };
    store_object(&self.project_root, &serde_json::to_vec(&manifest)?)
  }

  fn store_blob(&self, bytes: &[u8]) -> Result<ContentObjectId> {
    store_content(&self.project_root, "blobs", bytes)
  }

  pub fn manifest(
    &self,
    id: &ContentObjectId,
  ) -> std::result::Result<TreeManifest, ContentStoreError> {
    let bytes = load_object(&self.project_root, id)?;
    let manifest: TreeManifest = serde_json::from_slice(&bytes)
      .map_err(|error| ContentStoreError::integrity(id, format!("invalid manifest: {error}")))?;
    if manifest.version != 1
      || !matches!(
        manifest.semantics.0.as_str(),
        CANDIDATE_SEMANTICS_V1 | AUTHORITY_SNAPSHOT_SEMANTICS_V1
      )
    {
      return Err(ContentStoreError::integrity(
        id,
        "unsupported manifest version or semantics",
      ));
    }
    if canonical_digest(&manifest)
      .map_err(|error| ContentStoreError::integrity(id, error.to_string()))?
      != id.0
    {
      return Err(ContentStoreError::integrity(
        id,
        "manifest identity mismatch",
      ));
    }
    if manifest
      .entries
      .windows(2)
      .any(|pair| pair[0].path >= pair[1].path)
    {
      return Err(ContentStoreError::integrity(
        id,
        "manifest entries are not strictly sorted",
      ));
    }
    for entry in &manifest.entries {
      validate_relative(&entry.path)
        .map_err(|error| ContentStoreError::integrity(id, error.to_string()))?;
      match (&entry.kind, &entry.content_id, entry.executable) {
        (EntryKind::Directory, None, false) => {}
        (EntryKind::File, Some(blob), _) => {
          load_content(&self.project_root, "blobs", blob)?;
        }
        _ => return Err(ContentStoreError::integrity(id, "invalid manifest entry")),
      }
    }
    Ok(manifest)
  }

  pub fn materialize(
    &self,
    id: &ContentObjectId,
  ) -> std::result::Result<MaterializedSnapshot, ContentStoreError> {
    let manifest = self.manifest(id)?;
    let temp_root = self.project_root.join(".tenet/tmp/materialized");
    ensure_directory_without_symlinks(&temp_root)
      .map_err(|error| ContentStoreError::materialization(id, error.to_string()))?;
    let directory = tempfile::Builder::new()
      .prefix("snapshot-")
      .tempdir_in(temp_root)
      .map_err(|source| ContentStoreError::materialization_io(id, source))?;
    for entry in manifest.entries {
      let destination = directory.path().join(&entry.path);
      match entry.kind {
        EntryKind::Directory => fs::create_dir_all(&destination)
          .map_err(|source| ContentStoreError::materialization_io(id, source))?,
        EntryKind::File => {
          let blob = entry
            .content_id
            .as_ref()
            .ok_or_else(|| ContentStoreError::integrity(id, "file has no blob"))?;
          let bytes = load_content(&self.project_root, "blobs", blob)?;
          if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
              .map_err(|source| ContentStoreError::materialization_io(id, source))?;
          }
          fs::write(&destination, bytes)
            .map_err(|source| ContentStoreError::materialization_io(id, source))?;
          set_executable(&destination, entry.executable)
            .map_err(|error| ContentStoreError::materialization(id, error.to_string()))?;
        }
      }
    }
    Ok(MaterializedSnapshot { directory })
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
    let display = next.strip_prefix(&root).map_or_else(
      |_| next.display().to_string(),
      |path| path.display().to_string(),
    );
    let metadata = match fs::symlink_metadata(&next) {
      Ok(metadata) => metadata,
      Err(source) if source.kind() == ErrorKind::NotFound => {
        return Err(PathResolutionError::Missing { path: display });
      }
      Err(source) => {
        return Err(PathResolutionError::Io {
          path: display,
          source,
        });
      }
    };
    if metadata.file_type().is_symlink() {
      return Err(PathResolutionError::UnsupportedSymlink { path: display });
    }
    if index + 1 != components.len() && !metadata.is_dir() {
      return Err(PathResolutionError::NotDirectory { path: display });
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

pub fn discover_root(cwd: &Path) -> Result<PathBuf> {
  let mut path = cwd.canonicalize()?;
  loop {
    match resolve_relative_path(&path, ".tenet/format", ExpectedEntry::File) {
      Ok(_) => return Ok(path),
      Err(PathResolutionError::Missing { .. }) => {}
      Err(error) => return Err(error.into()),
    }
    if !path.pop() {
      bail!("no initialized Tenet project encloses the working directory");
    }
  }
}

pub fn initialize(root: &Path, spec: &Path) -> Result<(ProjectConfig, String, bool)> {
  let root = root.canonicalize()?;
  let spec_relative = if spec.is_absolute() {
    spec.strip_prefix(&root)?.to_path_buf()
  } else {
    spec.to_path_buf()
  };
  let spec_path = normalized_relative(&spec_relative)?;
  let config_path = root.join(CONFIG_PATH);
  let created = !config_path.exists();
  for relative in [
    TENET_DIR,
    ".tenet/objects",
    ".tenet/blobs",
    ".tenet/refs",
    ".tenet/refs/requirements",
    ".tenet/tmp",
    ".agents/skills/tenet",
  ] {
    ensure_directory_without_symlinks(&root.join(relative))?;
  }
  let spec = root.join(&spec_path);
  if !spec.exists() {
    if let Some(parent) = spec.parent() {
      fs::create_dir_all(parent)?;
    }
    atomic_write(&root, &spec, DEFAULT_SPECIFICATION.as_bytes())?;
  }
  resolve_relative_path(&root, &spec_path, ExpectedEntry::File)?;
  let existing = (!created).then(|| load_policy(&root)).transpose()?;
  if let Some(policy) = &existing
    && policy.spec_path != spec_path
  {
    bail!(
      "project is already initialized for specification `{}`",
      policy.spec_path
    );
  }
  let policy = existing.unwrap_or(ProjectConfig {
    version: 1,
    spec_path,
    candidate: Default::default(),
    verifiers: Vec::new(),
  });
  if created {
    atomic_write(
      &root,
      &config_path,
      toml::to_string_pretty(&policy)?.as_bytes(),
    )?;
  }
  let format_path = root.join(".tenet/format");
  match fs::read_to_string(&format_path) {
    Ok(value) if value == FORMAT_VERSION => {}
    Ok(value) => bail!("unsupported Tenet repository format `{}`", value.trim()),
    Err(error) if error.kind() == ErrorKind::NotFound => {
      atomic_write(&root, &format_path, FORMAT_VERSION.as_bytes())?;
    }
    Err(error) => return Err(error.into()),
  }
  atomic_write(&root, &root.join(".tenet/.gitignore"), b"tmp/\nlock\n")?;
  atomic_write(&root, &root.join(".tenet/lock"), b"")?;
  atomic_write(&root, &root.join(SKILL_PATH), SKILL.as_bytes())?;
  initialize_mcp_configuration(&root)?;
  Ok((
    policy.clone(),
    specification_digest(&root, &policy)?,
    created,
  ))
}

fn initialize_mcp_configuration(root: &Path) -> Result<()> {
  let path = root.join(".mcp.json");
  let entry = serde_json::json!({"command":"tenet","args":["mcp"]});
  let mut config: serde_json::Value = match fs::read(&path) {
    Ok(value) => serde_json::from_slice(&value)?,
    Err(error) if error.kind() == ErrorKind::NotFound => serde_json::json!({}),
    Err(error) => return Err(error.into()),
  };
  let servers = config
    .as_object_mut()
    .context("MCP configuration must be an object")?
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
    root,
    &path,
    format!("{}\n", serde_json::to_string_pretty(&config)?).as_bytes(),
  )
}

pub fn load_policy(root: &Path) -> Result<VerificationPolicy> {
  let path = resolve_relative_path(root, CONFIG_PATH, ExpectedEntry::File)?;
  let policy: VerificationPolicy = toml::from_str(&fs::read_to_string(path)?)?;
  validate_policy(&policy)?;
  Ok(policy)
}

pub fn specification_digest(root: &Path, policy: &VerificationPolicy) -> Result<String> {
  let path = resolve_relative_path(root, &policy.spec_path, ExpectedEntry::File)?;
  Ok(bytes_digest(&fs::read(path)?))
}

pub fn store_object(root: &Path, bytes: &[u8]) -> Result<ContentObjectId> {
  store_content(root, "objects", bytes)
}

pub fn load_object(
  root: &Path,
  id: &ContentObjectId,
) -> std::result::Result<Vec<u8>, ContentStoreError> {
  load_content(root, "objects", id)
}

fn store_content(root: &Path, namespace: &str, bytes: &[u8]) -> Result<ContentObjectId> {
  let root = root.canonicalize()?;
  let id = ContentObjectId::new(bytes_digest(bytes)).map_err(anyhow::Error::msg)?;
  let directory = root.join(".tenet").join(namespace);
  ensure_directory_without_symlinks(&directory)?;
  let target = directory.join(&id.0[7..]);
  let temporary_root = root.join(".tenet/tmp/objects");
  ensure_directory_without_symlinks(&temporary_root)?;
  let mut temporary = tempfile::NamedTempFile::new_in(temporary_root)?;
  temporary.write_all(bytes)?;
  temporary.as_file().sync_all()?;
  match temporary.persist_noclobber(&target) {
    Ok(_) => {}
    Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {
      load_content(&root, namespace, &id).map_err(anyhow::Error::new)?;
    }
    Err(error) => return Err(error.error.into()),
  }
  Ok(id)
}

fn load_content(
  root: &Path,
  namespace: &str,
  id: &ContentObjectId,
) -> std::result::Result<Vec<u8>, ContentStoreError> {
  let root = root
    .canonicalize()
    .map_err(|error| ContentStoreError::integrity(id, error.to_string()))?;
  let id = canonical_id(id).map_err(|error| ContentStoreError::integrity(id, error.to_string()))?;
  let relative = format!(".tenet/{namespace}/{}", &id.0[7..]);
  let path = match resolve_relative_path(&root, &relative, ExpectedEntry::File) {
    Ok(path) => path,
    Err(PathResolutionError::Missing { .. }) => {
      return Err(ContentStoreError::Missing { id: id.0 });
    }
    Err(error) => return Err(ContentStoreError::integrity(&id, error.to_string())),
  };
  let bytes =
    fs::read(path).map_err(|error| ContentStoreError::integrity(&id, error.to_string()))?;
  if bytes_digest(&bytes) != id.0 {
    return Err(ContentStoreError::integrity(
      &id,
      "content digest does not match identity",
    ));
  }
  Ok(bytes)
}

pub fn atomic_write(root: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
  let root = root.canonicalize()?;
  let path = if path.is_absolute() {
    path.to_path_buf()
  } else {
    root.join(path)
  };
  let relative = normalized_relative(
    path
      .strip_prefix(&root)
      .context("Tenet-owned write escapes repository root")?,
  )?;
  let parent = Path::new(&relative)
    .parent()
    .unwrap_or_else(|| Path::new("."));
  ensure_directory_without_symlinks(&root.join(parent))?;
  if let Ok(metadata) = fs::symlink_metadata(&path)
    && (metadata.file_type().is_symlink() || !metadata.is_file())
  {
    bail!("output path is not a regular file: {}", path.display());
  }
  let temporary_root = root.join(".tenet/tmp/atomic");
  ensure_directory_without_symlinks(&temporary_root)?;
  let mut temporary = tempfile::NamedTempFile::new_in(temporary_root)?;
  temporary.write_all(bytes)?;
  temporary.as_file().sync_all()?;
  temporary.persist(&path).map_err(|error| error.error)?;
  Ok(())
}

pub fn validate_relative(value: &str) -> Result<()> {
  if value.is_empty() || Path::new(value).is_absolute() {
    bail!("path must be a non-empty relative path");
  }
  if Path::new(value)
    .components()
    .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
  {
    bail!("path contains an unsafe component");
  }
  Ok(())
}

fn canonical_id(id: &ContentObjectId) -> Result<ContentObjectId> {
  let normalized = ContentObjectId::new(id.0.clone()).map_err(anyhow::Error::msg)?;
  if normalized != *id {
    bail!("content object ID must use lowercase canonical form");
  }
  Ok(normalized)
}

fn capture_root(source: &Path) -> Result<PathBuf> {
  let metadata = fs::symlink_metadata(source)?;
  if metadata.file_type().is_symlink() || !metadata.is_dir() {
    bail!("snapshot source must be a regular directory");
  }
  source.canonicalize().map_err(Into::into)
}

fn collect_selector(root: &Path, selector: &str, paths: &mut BTreeSet<String>) -> Result<()> {
  if selector == "**" {
    return collect_paths(root, root, false, paths);
  }
  if let Some(prefix) = selector.strip_suffix("/**") {
    let directory = root.join(prefix);
    match fs::symlink_metadata(&directory) {
      Ok(metadata) if metadata.file_type().is_symlink() => {
        bail!("unsupported symlink in snapshot selector: {selector}")
      }
      Ok(metadata) if metadata.is_dir() => collect_paths(root, &directory, false, paths),
      Ok(_) => bail!("recursive selector is not a directory: {selector}"),
      Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
      Err(error) => Err(error.into()),
    }
  } else {
    let path = root.join(selector);
    match fs::symlink_metadata(&path) {
      Ok(metadata) if metadata.file_type().is_symlink() => {
        bail!("unsupported symlink in snapshot selector: {selector}")
      }
      Ok(metadata) if metadata.is_file() => {
        paths.insert(normalized_relative(path.strip_prefix(root)?)?);
        Ok(())
      }
      Ok(metadata) if metadata.is_dir() => Ok(()),
      Ok(_) => bail!("unsupported filesystem entry: {selector}"),
      Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
      Err(error) => Err(error.into()),
    }
  }
}

fn collect_paths(
  root: &Path,
  directory: &Path,
  include_directories: bool,
  paths: &mut BTreeSet<String>,
) -> Result<()> {
  let mut entries = fs::read_dir(directory)?.collect::<std::result::Result<Vec<_>, _>>()?;
  entries.sort_by_key(|entry| entry.file_name());
  for entry in entries {
    let path = entry.path();
    let relative = normalized_relative(path.strip_prefix(root)?)?;
    if !include_directories && candidate_path_is_reserved(&relative) && root == directory {
      continue;
    }
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() {
      bail!("unsupported symlink in snapshot: {relative}");
    }
    if metadata.is_dir() {
      if include_directories {
        paths.insert(relative.clone());
      }
      collect_paths(root, &path, include_directories, paths)?;
    } else if metadata.is_file() {
      paths.insert(relative);
    } else {
      bail!("unsupported filesystem entry in snapshot: {relative}");
    }
  }
  Ok(())
}

fn selector_matches(selector: &str, path: &str) -> bool {
  selector == "**"
    || selector == path
    || selector
      .strip_suffix("/**")
      .is_some_and(|prefix| path == prefix || path.starts_with(&format!("{prefix}/")))
}

fn normalized_relative(path: &Path) -> Result<String> {
  let mut parts = Vec::new();
  for component in path.components() {
    match component {
      Component::Normal(value) => parts.push(value.to_str().context("path must be UTF-8")?),
      Component::CurDir => {}
      _ => bail!("path must contain only normal relative components"),
    }
  }
  if parts.is_empty() {
    Ok(".".into())
  } else {
    Ok(parts.join("/"))
  }
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
        bail!("unsupported symlink in directory path: {}", path.display())
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
