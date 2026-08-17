use std::{
  collections::{BTreeMap, BTreeSet},
  path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use tenet_domain::config::normalize_protected_path;

#[derive(Debug, Clone, PartialEq, Eq)]
enum EntryState {
  Missing,
  Directory,
  File { bytes: Vec<u8>, executable: bool },
  Symlink(PathBuf),
}

#[derive(Debug, Clone)]
pub struct Snapshot {
  entries: BTreeMap<PathBuf, EntryState>,
  roots: Vec<PathBuf>,
}

pub async fn snapshot(cwd: &Path, paths: &[String]) -> Result<Snapshot> {
  let mut entries = BTreeMap::new();
  let mut roots = Vec::with_capacity(paths.len());
  for configured in paths {
    let rel = normalize_protected_path(configured)?;
    roots.push(rel.clone());
    collect_entry(cwd, &rel, &mut entries)?;
  }
  Ok(Snapshot { entries, roots })
}

pub async fn restore_changes(cwd: &Path, snapshot: &Snapshot) -> Result<Vec<String>> {
  let mut current = BTreeMap::new();
  for root in &snapshot.roots {
    collect_entry(cwd, root, &mut current)?;
  }
  let paths: BTreeSet<_> = snapshot
    .entries
    .keys()
    .chain(current.keys())
    .cloned()
    .collect();
  Ok(
    paths
      .into_iter()
      .filter(|path| snapshot.entries.get(path) != current.get(path))
      .map(|path| path.to_string_lossy().into_owned())
      .collect(),
  )
}

fn collect_entry(
  cwd: &Path,
  rel: &Path,
  entries: &mut BTreeMap<PathBuf, EntryState>,
) -> Result<()> {
  let path = cwd.join(rel);
  let metadata = match std::fs::symlink_metadata(&path) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
      entries.insert(rel.to_path_buf(), EntryState::Missing);
      return Ok(());
    }
    Err(error) => {
      return Err(error).with_context(|| format!("inspect protected path {}", rel.display()))
    }
  };
  let file_type = metadata.file_type();
  if file_type.is_symlink() {
    entries.insert(
      rel.to_path_buf(),
      EntryState::Symlink(std::fs::read_link(&path)?),
    );
  } else if file_type.is_dir() {
    entries.insert(rel.to_path_buf(), EntryState::Directory);
    for child in std::fs::read_dir(&path)? {
      let child = child?;
      collect_entry(cwd, &rel.join(child.file_name()), entries)?;
    }
  } else if file_type.is_file() {
    entries.insert(
      rel.to_path_buf(),
      EntryState::File {
        bytes: std::fs::read(&path)?,
        executable: is_executable(&metadata),
      },
    );
  } else {
    bail!("unsupported protected repository object {}", rel.display());
  }
  Ok(())
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
  use std::os::unix::fs::PermissionsExt;
  metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
  false
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn unsafe_protected_paths_are_rejected() {
    assert!(normalize_protected_path("../outside").is_err());
    assert!(normalize_protected_path("/absolute").is_err());
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn recursive_snapshot_detects_mode_and_symlink_changes() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let root = std::env::temp_dir().join(format!("tenet-protection-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("protected")).expect("protected directory");
    std::fs::write(root.join("protected/script"), "script").expect("protected script");
    symlink("script", root.join("protected/link")).expect("protected symlink");
    let snapshot = snapshot(&root, &["protected".into()])
      .await
      .expect("snapshot protected tree");
    let mut permissions = std::fs::metadata(root.join("protected/script"))
      .expect("script metadata")
      .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(root.join("protected/script"), permissions).expect("change mode");
    std::fs::remove_file(root.join("protected/link")).expect("remove old symlink");
    symlink("other", root.join("protected/link")).expect("change symlink");

    let changed = restore_changes(&root, &snapshot)
      .await
      .expect("detect protected changes");
    std::fs::remove_dir_all(root).expect("cleanup protected fixture");

    assert_eq!(
      changed,
      vec!["protected/link".to_owned(), "protected/script".to_owned()]
    );
  }
}
