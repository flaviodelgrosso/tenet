use std::{
  collections::BTreeMap,
  path::{Path, PathBuf},
};

use anyhow::Result;
use tokio::fs;

#[derive(Debug, Clone)]
pub struct Snapshot {
  files: BTreeMap<PathBuf, Option<Vec<u8>>>,
}

pub async fn snapshot(cwd: &Path, paths: &[String]) -> Result<Snapshot> {
  let mut files = BTreeMap::new();
  for rel in paths {
    let path = cwd.join(rel);
    let value = match fs::read(&path).await {
      Ok(bytes) => Some(bytes),
      Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
      Err(err) => return Err(err.into()),
    };
    files.insert(PathBuf::from(rel), value);
  }
  Ok(Snapshot { files })
}

pub async fn restore_changes(cwd: &Path, snapshot: &Snapshot) -> Result<Vec<String>> {
  let mut changed = Vec::new();
  for (rel, before) in &snapshot.files {
    let path = cwd.join(rel);
    let after = match fs::read(&path).await {
      Ok(bytes) => Some(bytes),
      Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
      Err(err) => return Err(err.into()),
    };
    if &after == before {
      continue;
    }
    changed.push(rel.to_string_lossy().to_string());
    match before {
      Some(bytes) => {
        if let Some(parent) = path.parent() {
          fs::create_dir_all(parent).await?;
        }
        fs::write(&path, bytes).await?;
      }
      None => {
        let _ = fs::remove_file(&path).await;
      }
    }
  }
  Ok(changed)
}
