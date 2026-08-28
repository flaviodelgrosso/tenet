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

pub const TENET_DIR: &str = ".tenet";
pub const CONFIG_PATH: &str = ".tenet/tenet.toml";
pub const CONTRACT_PATH: &str = ".tenet/contract.json";
pub const STATE_PATH: &str = ".tenet/state.json";
pub const SKILL_PATH: &str = ".agents/skills/tenet/SKILL.md";
const STORE_DIR: &str = ".tenet/store";
const DEFAULT_SPECIFICATION: &str = "# Tenet completion specification\n\nDescribe the required behavior and acceptance criteria for this project.\n";

pub const SKILL: &str = "---\nname: tenet\ndescription: Use when completion is governed by a Tenet contract.\ncompatibility: Requires Tenet MCP tools.\nmetadata:\n  tenet-skill-version: \"1\"\n---\n\n# Tenet workflow\n\nTenet judges immutable Candidate Snapshot R under independently sealed Authority Capsule A. Propose a contract, obtain explicit human approval, seal authority, present authorityId for explicit human selection, capture candidateId, and gate that exact pair. Project verifiers execute in R. authority_snapshot verifiers execute from A-owned oracle_path; argv[0] directly names a bundled executable, cwd is bundle-relative, and TENET_CANDIDATE_ROOT exposes R.\n";

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
    let root = project_root.join(STORE_DIR).join("snapshots");
    fs::create_dir_all(&root)?;
    Ok(Self { root })
  }
  pub fn capture(&self, source: &Path, excluded: &BTreeSet<&str>) -> Result<ContentObjectId> {
    let source = source.canonicalize()?;
    if !source.is_dir() {
      bail!("snapshot capture root must be a directory");
    }
    let mut entries = Vec::new();
    collect(&source, &source, excluded, &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = TreeManifest {
      version: 1,
      entries,
    };
    let id = content_id(canonical_digest(&manifest)?)?;
    let target = self.path_for(&id)?;
    if target.exists() {
      self.verify(&id)?;
      return Ok(id);
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
          fs::copy(source.join(&entry.path), &destination)?;
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
  pub fn materialize(&self, id: &ContentObjectId) -> Result<MaterializedSnapshot> {
    let manifest = self.verify(id)?;
    let temp_root = self
      .root
      .parent()
      .context("store has no parent")?
      .join("materialized");
    fs::create_dir_all(&temp_root)?;
    let directory = tempfile::Builder::new()
      .prefix("snapshot-")
      .tempdir_in(temp_root)?;
    let tree = self.path_for(id)?.join("tree");
    for entry in manifest.entries {
      let destination = directory.path().join(&entry.path);
      match entry.kind {
        EntryKind::Directory => fs::create_dir_all(destination)?,
        EntryKind::File => {
          fs::create_dir_all(destination.parent().context("entry has no parent")?)?;
          fs::copy(tree.join(&entry.path), &destination)?;
          set_executable(&destination, entry.executable)?;
        }
      }
    }
    Ok(MaterializedSnapshot { directory })
  }
  pub fn manifest(&self, id: &ContentObjectId) -> Result<TreeManifest> {
    self.verify(id)
  }
  fn verify(&self, id: &ContentObjectId) -> Result<TreeManifest> {
    let path = self.path_for(id)?;
    let manifest: TreeManifest = serde_json::from_slice(
      &fs::read(path.join("manifest.json"))
        .with_context(|| format!("load content object {}", id.0))?,
    )?;
    if manifest.version != 1 || content_id(canonical_digest(&manifest)?)? != *id {
      bail!("content object integrity failure for {}", id.0);
    }
    for entry in &manifest.entries {
      validate_relative(&entry.path)?;
      let file = path.join("tree").join(&entry.path);
      let metadata = fs::symlink_metadata(&file)?;
      match entry.kind {
        EntryKind::Directory if metadata.is_dir() => {}
        EntryKind::File
          if metadata.is_file()
            && entry.content_id.as_ref() == Some(&content_id(bytes_digest(&fs::read(&file)?))?)
            && executable(&metadata) == entry.executable => {}
        _ => bail!("content object integrity failure for {}", id.0),
      }
    }
    Ok(manifest)
  }
  fn path_for(&self, id: &ContentObjectId) -> Result<PathBuf> {
    Ok(
      self.root.join(
        id.0
          .strip_prefix("sha256:")
          .context("invalid content object ID")?,
      ),
    )
  }
}
fn collect(
  root: &Path,
  directory: &Path,
  excluded: &BTreeSet<&str>,
  entries: &mut Vec<TreeEntry>,
) -> Result<()> {
  let mut children = fs::read_dir(directory)?.collect::<std::result::Result<Vec<_>, _>>()?;
  children.sort_by_key(|item| item.file_name());
  for child in children {
    let name = child.file_name();
    let name = name.to_str().context("snapshot paths must be UTF-8")?;
    if directory == root && excluded.contains(name) {
      continue;
    }
    let path = child.path();
    let relative = normalized_relative(path.strip_prefix(root).context("snapshot escaped root")?)?;
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() {
      bail!("unsupported symlink in snapshot: {relative}");
    }
    if metadata.is_dir() {
      entries.push(TreeEntry {
        path: relative,
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
    .replace('\\', "/");
  validate_relative(&value)?;
  Ok(value)
}
pub fn validate_relative(value: &str) -> Result<()> {
  let path = Path::new(value);
  if value.trim().is_empty()
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
    if path.join(CONFIG_PATH).is_file() {
      return Ok(path);
    }
    if !path.pop() {
      bail!("no initialized Tenet project encloses the working directory");
    }
  }
}
pub fn initialize(root: &Path, spec: &Path) -> Result<(ProjectConfig, String, bool)> {
  let root = root.canonicalize()?;
  let spec = if spec.is_absolute() {
    spec.to_path_buf()
  } else {
    root.join(spec)
  };
  let config_path = root.join(CONFIG_PATH);
  let created = !config_path.exists();
  let existing = (!created).then(|| load_policy(&root)).transpose()?;
  if !spec.exists() {
    let relative = spec
      .strip_prefix(&root)
      .context("specification must be inside the Tenet project")?;
    validate_relative(relative.to_str().context("specification must be UTF-8")?)?;
    fs::create_dir_all(spec.parent().context("specification has no parent")?)?;
    atomic_write(&spec, DEFAULT_SPECIFICATION.as_bytes())?;
  }
  let spec_path = normalized_relative(
    spec
      .canonicalize()?
      .strip_prefix(&root)
      .context("specification must be inside project")?,
  )?;
  if let Some(policy) = &existing
    && policy.spec_path != spec_path
  {
    bail!(
      "project is already initialized for specification `{}`",
      policy.spec_path
    );
  }
  fs::create_dir_all(root.join(TENET_DIR))?;
  fs::create_dir_all(root.join(".agents/skills/tenet"))?;
  let policy = existing.unwrap_or(ProjectConfig {
    version: 1,
    spec_path,
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
  let path = root.join(CONFIG_PATH);
  let policy: VerificationPolicy = toml::from_str(&fs::read_to_string(path)?)?;
  validate_policy(&policy)?;
  Ok(policy)
}
pub fn specification_digest(root: &Path, policy: &VerificationPolicy) -> Result<String> {
  Ok(bytes_digest(&fs::read(root.join(&policy.spec_path))?))
}
pub fn policy_digest(policy: &VerificationPolicy) -> Result<String> {
  canonical_digest(policy).context("hash policy")
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
  let parent = path.parent().context("output path has no parent")?;
  fs::create_dir_all(parent)?;
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
    let excluded = BTreeSet::new();
    let initial = store.capture(left.path(), &excluded).expect("capture left");
    let file = fs::File::open(left.path().join("file")).expect("open file");
    file
      .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
      .expect("change mtime");
    assert_eq!(
      initial,
      store
        .capture(left.path(), &excluded)
        .expect("capture metadata-only change")
    );
    assert_eq!(
      initial,
      store
        .capture(right.path(), &excluded)
        .expect("capture right")
    );
    fs::write(left.path().join("file"), "two").expect("mutate");
    assert_ne!(
      initial,
      store
        .capture(left.path(), &excluded)
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
    let initial = store
      .capture(source.path(), &BTreeSet::new())
      .expect("capture");
    let mut permissions = fs::metadata(&file).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&file, permissions).expect("chmod");
    assert_ne!(
      initial,
      store
        .capture(source.path(), &BTreeSet::new())
        .expect("executable capture")
    );
    fs::rename(&file, source.path().join("renamed")).expect("rename");
    assert_ne!(
      initial,
      store
        .capture(source.path(), &BTreeSet::new())
        .expect("renamed capture")
    );
  }

  #[test]
  fn snapshots_reject_symlinks_and_corruption() {
    let project = tempfile::tempdir().expect("project");
    let source = tempfile::tempdir().expect("source");
    fs::write(source.path().join("file"), "one").expect("file");
    let store = ContentStore::open(project.path()).expect("store");
    let id = store
      .capture(source.path(), &BTreeSet::new())
      .expect("capture");
    let object = project
      .path()
      .join(STORE_DIR)
      .join("snapshots")
      .join(id.0.strip_prefix("sha256:").expect("digest"));
    fs::write(object.join("tree/file"), "corrupt").expect("corrupt");
    assert!(store.materialize(&id).is_err());
    #[cfg(unix)]
    {
      std::os::unix::fs::symlink("/outside", source.path().join("link")).expect("link");
      assert!(store.capture(source.path(), &BTreeSet::new()).is_err());
    }
  }
}
