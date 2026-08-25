use std::path::{Path, PathBuf};
use uuid::Uuid;

use anyhow::{Context, Result};
use tokio::{fs, sync::Mutex};

use tenet_domain::config::normalize_protected_path;

use crate::git;

// Git worktrees share mutable metadata under the canonical repository. Serialize the
// add/list/remove sequence so parallel workers cannot corrupt that registry.
static WORKTREE_REGISTRY_LOCK: Mutex<()> = Mutex::const_new(());

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

  pub async fn materialize_repository_file(
    &self,
    workspace: &Path,
    relative_path: &str,
  ) -> Result<()> {
    let relative_path = normalize_protected_path(relative_path)?;
    let destination = workspace.join(&relative_path);
    if destination.exists() {
      return Ok(());
    }
    let source = self.repository.join(&relative_path);
    let contents = fs::read(&source)
      .await
      .with_context(|| format!("read repository file {}", source.display()))?;
    if let Some(parent) = destination.parent() {
      fs::create_dir_all(parent).await?;
    }
    fs::write(&destination, contents)
      .await
      .with_context(|| format!("materialize repository file {}", destination.display()))
  }
  pub async fn remove(&self, path: &Path) -> Result<()> {
    let _guard = WORKTREE_REGISTRY_LOCK.lock().await;
    self.remove_locked(path).await
  }

  async fn remove_locked(&self, path: &Path) -> Result<()> {
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
    let _guard = WORKTREE_REGISTRY_LOCK.lock().await;
    if path.exists() {
      self.remove_locked(path).await?;
    }
    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent).await?;
    }
    git::add_worktree(&self.repository, path, revision)
      .await
      .with_context(|| format!("create worktree {} at {revision}", path.display()))
  }
}

#[cfg(test)]
mod tests {
  use std::process::Command;

  use tempfile::tempdir;
  use tokio::task::JoinSet;

  use super::WorkspaceManager;
  use crate::git;

  fn run_git(repository: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
      .args(args)
      .current_dir(repository)
      .status()
      .expect("run git");
    assert!(status.success(), "git {} failed", args.join(" "));
  }

  #[tokio::test]
  async fn parallel_workspace_lifecycles_preserve_git_registry() {
    let repository = tempdir().expect("temporary repository");
    run_git(repository.path(), &["init", "-q"]);
    std::fs::write(repository.path().join("README.txt"), "initial").expect("write fixture");
    run_git(repository.path(), &["add", "README.txt"]);
    run_git(
      repository.path(),
      &[
        "-c",
        "user.name=Tenet Test",
        "-c",
        "user.email=tenet@example.test",
        "commit",
        "-q",
        "-m",
        "initial",
      ],
    );
    let revision = git::head(repository.path()).await.expect("repository head");
    let manager = WorkspaceManager::new(repository.path().to_path_buf(), "parallel");
    let mut tasks = JoinSet::new();
    for index in 0..8 {
      let manager = manager.clone();
      let revision = revision.clone();
      tasks.spawn(async move {
        let workspace = manager
          .create_disposable(&format!("worker-{index}"), &revision)
          .await?;
        manager.remove(&workspace).await
      });
    }

    while let Some(result) = tasks.join_next().await {
      result
        .expect("workspace task")
        .expect("workspace lifecycle");
    }
    manager.cleanup_run().await.expect("cleanup run");
  }
}
