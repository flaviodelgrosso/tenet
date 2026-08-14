use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use tokio::process::Command;

use tenet_domain::model::RepositoryChange;

pub async fn head(cwd: &Path) -> Result<String> {
  run(cwd, &["rev-parse", "--verify", "HEAD"])
    .await
    .context("worktree execution requires an existing Git repository with at least one commit")
}

pub async fn is_clean(cwd: &Path) -> Result<bool> {
  Ok(
    run(cwd, &["status", "--porcelain", "--untracked-files=all"])
      .await?
      .is_empty(),
  )
}

pub async fn repository_changes(cwd: &Path) -> Result<Vec<RepositoryChange>> {
  let output = run(cwd, &["status", "--porcelain", "--untracked-files=all"]).await?;
  Ok(parse_status(&output))
}

pub async fn changed_paths(cwd: &Path, base: &str, candidate: &str) -> Result<Vec<String>> {
  let output = run(cwd, &["diff", "--name-only", base, candidate]).await?;
  let mut paths: Vec<_> = output.lines().map(str::to_owned).collect();
  paths.sort();
  paths.dedup();
  Ok(paths)
}

pub async fn add_worktree(repository: &Path, workspace: &Path, revision: &str) -> Result<()> {
  let workspace = path_text(workspace)?;
  run(
    repository,
    &["worktree", "add", "--detach", workspace, revision],
  )
  .await
  .map(|_| ())
}

pub async fn remove_worktree(repository: &Path, workspace: &Path) -> Result<()> {
  let workspace = path_text(workspace)?;
  run(repository, &["worktree", "remove", "--force", workspace])
    .await
    .map(|_| ())
}

pub async fn commit_all(cwd: &Path, message: &str) -> Result<String> {
  run(cwd, &["add", "-A"]).await?;
  if is_clean(cwd).await? {
    bail!("worker produced no repository changes");
  }
  run(
    cwd,
    &[
      "-c",
      "user.name=Tenet Controller",
      "-c",
      "user.email=tenet@localhost",
      "commit",
      "-m",
      message,
    ],
  )
  .await?;
  head(cwd).await
}

pub async fn is_ancestor(cwd: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
  let status = Command::new("git")
    .args(["merge-base", "--is-ancestor", ancestor, descendant])
    .current_dir(cwd)
    .status()
    .await
    .context("run git merge-base")?;
  match status.code() {
    Some(0) => Ok(true),
    Some(1) => Ok(false),
    _ => Err(anyhow!("git merge-base failed with {status}")),
  }
}

pub async fn cherry_pick(cwd: &Path, revision: &str) -> Result<bool> {
  let output = Command::new("git")
    .args(["cherry-pick", revision])
    .current_dir(cwd)
    .output()
    .await
    .context("run git cherry-pick")?;
  Ok(output.status.success())
}

pub async fn conflict_paths(cwd: &Path) -> Result<Vec<String>> {
  let output = run(cwd, &["diff", "--name-only", "--diff-filter=U"]).await?;
  Ok(output.lines().map(str::to_owned).collect())
}

pub async fn abort_cherry_pick(cwd: &Path) -> Result<()> {
  run(cwd, &["cherry-pick", "--abort"]).await.map(|_| ())
}

pub async fn reset_hard(cwd: &Path, revision: &str) -> Result<()> {
  run(cwd, &["reset", "--hard", revision]).await.map(|_| ())
}

pub async fn fast_forward(cwd: &Path, revision: &str) -> Result<()> {
  run(cwd, &["merge", "--ff-only", revision])
    .await
    .map(|_| ())
}

async fn run(cwd: &Path, args: &[&str]) -> Result<String> {
  let output = Command::new("git")
    .args(args)
    .current_dir(cwd)
    .output()
    .await
    .with_context(|| format!("run git {}", args.join(" ")))?;
  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    bail!("git {} failed: {stderr}", args.join(" "));
  }
  Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn path_text(path: &Path) -> Result<&str> {
  path
    .to_str()
    .ok_or_else(|| anyhow!("Git workspace path is not valid UTF-8: {}", path.display()))
}

fn parse_status(output: &str) -> Vec<RepositoryChange> {
  output
    .lines()
    .filter_map(|line| {
      let status = line.chars().find(|character| !character.is_whitespace())?;
      let path = line
        .get(3..)?
        .rsplit_once(" -> ")
        .map_or_else(|| line.get(3..).unwrap_or_default(), |(_, path)| path)
        .to_owned();
      Some(RepositoryChange { path, status })
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use uuid::Uuid;

  use super::head;

  #[tokio::test]
  async fn head_requires_an_existing_repository_with_a_commit() {
    let path = std::env::temp_dir().join(format!("tenet-non-repository-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();

    let error = head(&path).await.unwrap_err().to_string();
    std::fs::remove_dir_all(path).unwrap();

    assert!(error
      .contains("worktree execution requires an existing Git repository with at least one commit"));
  }
}
