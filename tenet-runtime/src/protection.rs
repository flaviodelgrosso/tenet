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
  let mut first_error = None;
  for (rel, before) in &snapshot.files {
    let path = cwd.join(rel);
    let after = match fs::read(&path).await {
      Ok(bytes) => Some(bytes),
      Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
      Err(err) => {
        if first_error.is_none() {
          first_error = Some(
            anyhow::Error::from(err).context(format!("reading protected path {}", rel.display())),
          );
        }
        continue;
      }
    };
    if &after == before {
      continue;
    }
    changed.push(rel.to_string_lossy().to_string());
    let restore_result = match before {
      Some(bytes) => {
        async {
          if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
          }
          fs::write(&path, bytes).await
        }
        .await
      }
      None => match fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
      },
    };
    if let Err(err) = restore_result {
      if first_error.is_none() {
        first_error = Some(
          anyhow::Error::from(err).context(format!("restoring protected path {}", rel.display())),
        );
      }
    }
  }
  if let Some(error) = first_error {
    return Err(error);
  }
  Ok(changed)
}
