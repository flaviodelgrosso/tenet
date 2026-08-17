use std::path::{Path, PathBuf};
use uuid::Uuid;

use anyhow::{Context, Result};
use tokio::fs;

use crate::git;

/// Creates disposable Git worktrees that isolate repository mutations.
///
/// Worktrees are not security sandboxes: workers retain the host process's filesystem,
/// network, and credential access.
#[derive(Clone)]
pub struct WorkspaceManager {
  repository: PathBuf,
  run_id: String,
}

impl WorkspaceManager {
  pub fn new(repository: PathBuf, run_id: impl Into<String>) -> Self {
    Self {
      repository,
      run_id: run_id.into(),
    }
  }

  pub fn run_id(&self) -> &str {
    &self.run_id
  }

  pub fn worker_path(&self, lease_id: &str) -> PathBuf {
    self
      .repository
      .join(".tenet")
      .join("worktrees")
      .join(&self.run_id)
      .join(lease_id)
  }

  pub fn integration_path(&self) -> PathBuf {
    self
      .repository
      .join(".tenet")
      .join("integration")
      .join(&self.run_id)
  }

  pub async fn create_worker(&self, lease_id: &str, revision: &str) -> Result<PathBuf> {
    let path = self.worker_path(lease_id);
    self.create(&path, revision).await?;
    Ok(path)
  }

  pub async fn create_disposable(&self, purpose: &str, revision: &str) -> Result<PathBuf> {
    let safe_purpose = purpose.replace(['/', '\\'], "-");
    let id = format!("{safe_purpose}-{}", &Uuid::new_v4().to_string()[..8]);
    let path = self.worker_path(&id);
    self.create(&path, revision).await?;
    Ok(path)
  }

  pub async fn create_integration(&self, revision: &str) -> Result<PathBuf> {
    let path = self.integration_path();
    self.create(&path, revision).await?;
    Ok(path)
  }

  pub async fn remove(&self, path: &Path) -> Result<()> {
    let registered = git::is_worktree_registered(&self.repository, path).await?;
    if !path.exists() && !registered {
      return Ok(());
    }
    if registered {
      if let Err(error) = git::remove_worktree(&self.repository, path).await {
        if git::is_worktree_registered(&self.repository, path).await? {
          return Err(error).with_context(|| format!("remove worktree {}", path.display()));
        }
      }
    }
    if path.exists() {
      fs::remove_dir_all(path)
        .await
        .with_context(|| format!("remove residual workspace {}", path.display()))?;
    }
    Ok(())
  }

  pub async fn cleanup_run(&self) -> Result<()> {
    let workers_root = self
      .repository
      .join(".tenet")
      .join("worktrees")
      .join(&self.run_id);
    if workers_root.exists() {
      let mut entries = fs::read_dir(&workers_root).await?;
      while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.is_dir() {
          self.remove(&path).await?;
        }
      }
      if workers_root.exists() {
        fs::remove_dir_all(&workers_root).await?;
      }
    }

    let integration = self.integration_path();
    if integration.exists() {
      self.remove(&integration).await?;
    }
    Ok(())
  }

  async fn create(&self, path: &Path, revision: &str) -> Result<()> {
    if path.exists() {
      self.remove(path).await?;
    }
    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent).await?;
    }
    git::add_worktree(&self.repository, path, revision)
      .await
      .with_context(|| format!("create worktree {} at {revision}", path.display()))
  }
}
