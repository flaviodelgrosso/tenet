use std::{
  collections::BTreeMap,
  io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
  path::{Path, PathBuf},
  process::Stdio,
};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryState {
  pub head: String,
  pub status: String,
}

pub async fn repository_state(cwd: &Path) -> Result<RepositoryState> {
  Ok(RepositoryState {
    head: head(cwd).await?,
    status: run(cwd, &["status", "--porcelain=v1", "--untracked-files=all"]).await?,
  })
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
pub async fn clone_without_checkout(source: &Path, destination: &Path) -> Result<()> {
  let source = path_text(source)?;
  let destination = path_text(destination)?;
  run(
    Path::new("."),
    &[
      "clone",
      "--no-hardlinks",
      "--no-checkout",
      "--",
      source,
      destination,
    ],
  )
  .await
  .map(|_| ())
}

pub async fn checkout_detached(cwd: &Path, revision: &str) -> Result<()> {
  run(cwd, &["checkout", "--detach", revision])
    .await
    .map(|_| ())
}

pub async fn read_blob(cwd: &Path, revision: &str, path: &str) -> Result<String> {
  run(cwd, &["show", &format!("{revision}:{path}")]).await
}

pub async fn parent(cwd: &Path, revision: &str) -> Result<String> {
  run(cwd, &["rev-parse", &format!("{revision}^{{commit}}^")]).await
}

pub async fn has_gitlinks(cwd: &Path, revision: &str) -> Result<bool> {
  Ok(
    run(cwd, &["ls-tree", "-r", revision])
      .await?
      .lines()
      .any(|line| line.starts_with("160000 ")),
  )
}
pub async fn path_exists(cwd: &Path, revision: &str, path: &str) -> Result<bool> {
  Ok(
    !run(cwd, &["ls-tree", "--name-only", revision, "--", path])
      .await?
      .is_empty(),
  )
}

pub async fn repository_blob_hashes(
  cwd: &Path,
  revision: &str,
) -> Result<BTreeMap<String, String>> {
  let output = run_bytes(cwd, &["ls-tree", "-r", "--full-tree", "-z", revision]).await?;
  if !output.is_empty() && output.last() != Some(&0) {
    bail!("Git tree output was not NUL terminated");
  }
  if output.is_empty() {
    return Ok(BTreeMap::new());
  }
  let mut blobs = BTreeMap::new();
  let entries = output
    .get(..output.len().saturating_sub(1))
    .unwrap_or_default();
  for entry in entries.split(|byte| *byte == 0) {
    if entry.is_empty() {
      bail!("Git tree contained an empty dependency entry");
    }
    let separator = entry
      .iter()
      .position(|byte| *byte == b'\t')
      .context("parse Git tree entry path")?;
    let metadata =
      std::str::from_utf8(&entry[..separator]).context("Git tree metadata is not valid UTF-8")?;
    let path = std::str::from_utf8(&entry[separator + 1..])
      .context("Git dependency path is not valid UTF-8")?;
    let mut fields = metadata.split_whitespace();
    let _mode = fields.next().context("parse Git tree entry mode")?;
    let _kind = fields.next().context("parse Git tree entry kind")?;
    let hash = fields.next().context("parse Git tree entry object ID")?;
    if fields.next().is_some()
      || path.is_empty()
      || hash.is_empty()
      || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
      || blobs.insert(path.into(), hash.into()).is_some()
    {
      bail!("Git tree contained a malformed or duplicate dependency entry");
    }
  }
  Ok(blobs)
}

pub async fn archive(
  cwd: &Path,
  revision: &str,
  max_archive_bytes: u64,
  max_tree_bytes: u64,
  max_entries: u32,
) -> Result<(std::fs::File, u64)> {
  let cwd = cwd.to_path_buf();
  let revision = revision.to_owned();
  tokio::task::spawn_blocking(move || {
    raw_tree_archive(
      &cwd,
      &revision,
      max_archive_bytes,
      max_tree_bytes,
      max_entries,
    )
  })
  .await
  .context("join raw Git tree archive builder")?
}

fn raw_tree_archive(
  cwd: &Path,
  revision: &str,
  max_archive_bytes: u64,
  max_tree_bytes: u64,
  max_entries: u32,
) -> Result<(std::fs::File, u64)> {
  let mut stderr = tempfile::tempfile().context("create Git tree error capture")?;
  let mut child = std::process::Command::new("git")
    .env("GIT_CONFIG_NOSYSTEM", "1")
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .arg("--no-replace-objects")
    .args(["ls-tree", "-r", "-l", "--full-tree", "-z", revision])
    .current_dir(cwd)
    .stdout(Stdio::piped())
    .stderr(stderr.try_clone().context("clone Git tree error capture")?)
    .spawn()
    .context("spawn raw Git tree reader")?;
  let stdout = child.stdout.take().context("capture raw Git tree")?;
  let result = build_raw_tree_archive(
    cwd,
    BufReader::new(stdout),
    max_archive_bytes,
    max_tree_bytes,
    max_entries,
  );
  if let Err(error) = result {
    let _ = child.kill();
    let _ = child.wait();
    return Err(error);
  }
  let status = child.wait().context("read raw Git tree")?;
  if !status.success() {
    stderr.rewind().context("rewind Git tree errors")?;
    let mut message = String::new();
    stderr
      .take(64 * 1024)
      .read_to_string(&mut message)
      .context("read Git tree errors")?;
    bail!("git ls-tree failed: {}", message.trim());
  }
  result
}

fn build_raw_tree_archive(
  cwd: &Path,
  mut tree: impl BufRead,
  max_archive_bytes: u64,
  max_tree_bytes: u64,
  max_entries: u32,
) -> Result<(std::fs::File, u64)> {
  const MAX_TREE_RECORD_BYTES: u64 = 1024 * 1024;
  const MAX_SYMLINK_BYTES: u64 = 64 * 1024;

  let file = tempfile::tempfile().context("create anonymous raw Git tree archive")?;
  let writer = BoundedArchiveWriter {
    inner: file,
    written: 0,
    max_bytes: max_archive_bytes,
  };
  let mut archive = tar::Builder::new(writer);
  let mut record = Vec::new();
  let mut entry_count = 0_u32;
  let mut tree_bytes = 0_u64;
  loop {
    record.clear();
    let read = tree
      .by_ref()
      .take(MAX_TREE_RECORD_BYTES + 1)
      .read_until(0, &mut record)
      .context("read raw Git tree entry")?;
    if read == 0 {
      break;
    }
    if record.len() as u64 > MAX_TREE_RECORD_BYTES || record.last() != Some(&0) {
      bail!("Git tree archive entry exceeds controller path metadata limit");
    }
    record.pop();
    entry_count = entry_count
      .checked_add(1)
      .context("Git tree archive entry count overflow")?;
    if entry_count > max_entries {
      bail!("trusted input exceeds entry limit of {max_entries}");
    }
    let separator = record
      .iter()
      .position(|byte| *byte == b'\t')
      .context("parse Git tree archive path")?;
    let metadata =
      std::str::from_utf8(&record[..separator]).context("Git tree metadata is not valid UTF-8")?;
    let path = std::str::from_utf8(&record[separator + 1..])
      .context("Git tree archive path is not valid UTF-8")?;
    let mut fields = metadata.split_whitespace();
    let mode = fields.next().context("parse Git tree archive mode")?;
    let kind = fields.next().context("parse Git tree archive kind")?;
    let object_id = fields.next().context("parse Git tree archive object ID")?;
    let size = fields
      .next()
      .context("parse Git tree archive blob size")?
      .parse::<u64>()
      .context("parse Git tree archive blob size")?;
    if fields.next().is_some()
      || path.is_empty()
      || kind != "blob"
      || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
      bail!("Git tree contained a malformed or unsupported archive entry");
    }
    let mut header = tar::Header::new_gnu();
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    match mode {
      "100644" | "100755" => {
        tree_bytes = tree_bytes
          .checked_add(size)
          .context("trusted input tree size overflow")?;
        if tree_bytes > max_tree_bytes {
          bail!("trusted input exceeds materialized tree limit of {max_tree_bytes} bytes");
        }
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(if mode == "100755" { 0o755 } else { 0o644 });
        header.set_size(size);
        header.set_cksum();
        let stderr = tempfile::tempfile().context("create Git blob error capture")?;
        let mut child = std::process::Command::new("git")
          .env("GIT_CONFIG_NOSYSTEM", "1")
          .env("GIT_CONFIG_GLOBAL", "/dev/null")
          .arg("--no-replace-objects")
          .args(["cat-file", "blob", object_id])
          .current_dir(cwd)
          .stdout(Stdio::piped())
          .stderr(stderr.try_clone().context("clone Git blob error capture")?)
          .spawn()
          .context("spawn raw Git blob reader")?;
        let mut stdout = child.stdout.take().context("capture raw Git blob")?;
        if let Err(error) = archive.append_data(&mut header, path, &mut stdout) {
          let _ = child.kill();
          let _ = child.wait();
          return Err(error.into());
        }
        let status = child.wait().context("read raw Git blob")?;
        if !status.success() {
          bail!("git cat-file blob failed for {object_id}");
        }
      }
      "120000" => {
        if size > MAX_SYMLINK_BYTES {
          bail!("Git symlink exceeds controller target limit of {MAX_SYMLINK_BYTES} bytes");
        }
        let target = std::process::Command::new("git")
          .env("GIT_CONFIG_NOSYSTEM", "1")
          .env("GIT_CONFIG_GLOBAL", "/dev/null")
          .arg("--no-replace-objects")
          .args(["cat-file", "blob", object_id])
          .current_dir(cwd)
          .output()
          .context("read raw Git symlink")?;
        if !target.status.success() {
          bail!(
            "git cat-file blob failed: {}",
            String::from_utf8_lossy(&target.stderr).trim()
          );
        }
        let target =
          std::str::from_utf8(&target.stdout).context("Git symlink target is not valid UTF-8")?;
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_size(0);
        header
          .set_link_name(target)
          .context("encode Git symlink target")?;
        header.set_cksum();
        archive.append_data(&mut header, path, std::io::empty())?;
      }
      _ => bail!("Git tree contained unsupported mode {mode}"),
    }
  }
  archive.finish()?;
  let writer = archive.into_inner()?;
  let total = writer.written;
  let mut file = writer.inner;
  file.seek(SeekFrom::Start(0))?;
  Ok((file, total))
}

struct BoundedArchiveWriter {
  inner: std::fs::File,
  written: u64,
  max_bytes: u64,
}

impl Write for BoundedArchiveWriter {
  fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
    let next = self
      .written
      .checked_add(bytes.len() as u64)
      .ok_or_else(|| std::io::Error::other("Git archive size overflow"))?;
    if next > self.max_bytes {
      return Err(std::io::Error::other(format!(
        "Git archive exceeds trusted input limit of {} bytes",
        self.max_bytes
      )));
    }
    let written = self.inner.write(bytes)?;
    self.written += written as u64;
    Ok(written)
  }

  fn flush(&mut self) -> std::io::Result<()> {
    self.inner.flush()
  }
}

pub async fn remove_worktree(repository: &Path, workspace: &Path) -> Result<()> {
  let workspace = path_text(workspace)?;
  run(repository, &["worktree", "remove", "--force", workspace])
    .await
    .map(|_| ())
}

pub async fn is_worktree_registered(repository: &Path, workspace: &Path) -> Result<bool> {
  let workspace = normalized_path(workspace);
  let output = run(repository, &["worktree", "list", "--porcelain"]).await?;
  Ok(
    output
      .lines()
      .filter_map(|line| line.strip_prefix("worktree "))
      .any(|registered| normalized_path(Path::new(registered)) == workspace),
  )
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

pub async fn update_ref(cwd: &Path, reference: &str, revision: &str) -> Result<()> {
  run(cwd, &["update-ref", reference, revision])
    .await
    .map(|_| ())
}

pub async fn delete_ref(cwd: &Path, reference: &str) -> Result<()> {
  run(cwd, &["update-ref", "-d", reference]).await.map(|_| ())
}

pub async fn resolve_ref(cwd: &Path, reference: &str) -> Result<String> {
  run(cwd, &["rev-parse", "--verify", reference]).await
}

pub async fn is_ancestor(cwd: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
  let status = Command::new("git")
    .env("GIT_CONFIG_NOSYSTEM", "1")
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .arg("--no-replace-objects")
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
    .env("GIT_CONFIG_NOSYSTEM", "1")
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .arg("--no-replace-objects")
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

pub async fn reset_soft(cwd: &Path, revision: &str) -> Result<()> {
  run(cwd, &["reset", "--soft", revision]).await.map(|_| ())
}

pub async fn reset_hard(cwd: &Path, revision: &str) -> Result<()> {
  run(cwd, &["reset", "--hard", revision]).await.map(|_| ())
}

pub async fn fast_forward(cwd: &Path, revision: &str) -> Result<()> {
  run(cwd, &["merge", "--ff-only", revision])
    .await
    .map(|_| ())
}

fn normalized_path(path: &Path) -> PathBuf {
  std::fs::canonicalize(path).unwrap_or_else(|_| {
    path
      .parent()
      .and_then(|parent| std::fs::canonicalize(parent).ok())
      .and_then(|parent| path.file_name().map(|name| parent.join(name)))
      .unwrap_or_else(|| path.to_path_buf())
  })
}
async fn run(cwd: &Path, args: &[&str]) -> Result<String> {
  Ok(
    String::from_utf8_lossy(&run_bytes(cwd, args).await?)
      .trim()
      .to_owned(),
  )
}

async fn run_bytes(cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
  let output = Command::new("git")
    .env("GIT_CONFIG_NOSYSTEM", "1")
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .arg("--no-replace-objects")
    .args(args)
    .current_dir(cwd)
    .output()
    .await
    .with_context(|| format!("run git {}", args.join(" ")))?;
  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    bail!("git {} failed: {stderr}", args.join(" "));
  }
  Ok(output.stdout)
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
  use std::process::Command as StdCommand;

  use uuid::Uuid;

  use super::{archive, head, repository_blob_hashes};

  #[tokio::test]
  async fn head_requires_an_existing_repository_with_a_commit() {
    let path = std::env::temp_dir().join(format!("tenet-non-repository-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();

    let error = head(&path).await.unwrap_err().to_string();
    std::fs::remove_dir_all(path).unwrap();

    assert!(error
      .contains("worktree execution requires an existing Git repository with at least one commit"));
  }

  #[tokio::test]
  async fn archive_streams_the_exact_commit_tree() {
    let repository = tempfile::tempdir().expect("temporary Git repository");
    let run = |arguments: &[&str]| {
      let output = StdCommand::new("git")
        .arg("-C")
        .arg(repository.path())
        .args(arguments)
        .output()
        .expect("run fixture Git command");
      assert!(
        output.status.success(),
        "git {:?} failed: {}",
        arguments,
        String::from_utf8_lossy(&output.stderr)
      );
      String::from_utf8(output.stdout)
        .expect("fixture Git output is UTF-8")
        .trim()
        .to_owned()
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "tenet@example.invalid"]);
    run(&["config", "user.name", "Tenet Test"]);
    std::fs::write(repository.path().join("exact.txt"), "revision-bound\n").expect("write fixture");
    std::fs::write(repository.path().join("substituted.txt"), "$Format:%H$\n")
      .expect("write export-subst fixture");
    std::fs::write(
      repository.path().join(".gitattributes"),
      "substituted.txt export-subst\nexact.txt export-ignore\n",
    )
    .expect("write archive attributes");
    run(&["add", "-A"]);
    run(&["commit", "-q", "-m", "fixture"]);
    let revision = head(repository.path()).await.expect("fixture revision");
    std::fs::write(
      repository.path().join("replacement.txt"),
      "forged-replacement\n",
    )
    .expect("write replacement blob");
    let original_blob = run(&["rev-parse", "HEAD:exact.txt"]);
    let replacement_blob = run(&["hash-object", "-w", "replacement.txt"]);
    run(&["replace", &original_blob, &replacement_blob]);
    let error = archive(repository.path(), &revision, 1, 1024 * 1024, 100)
      .await
      .expect_err("archive must respect controller byte limit");
    assert!(error.to_string().contains("exceeds trusted input limit"));
    let error = archive(repository.path(), &revision, 1024 * 1024, 1024 * 1024, 1)
      .await
      .expect_err("archive must enforce entry limit before reading every blob");
    assert!(error.to_string().contains("exceeds entry limit"));

    let (file, bytes) = archive(repository.path(), &revision, 1024 * 1024, 1024 * 1024, 100)
      .await
      .expect("Git archive");
    assert!(bytes > 0);
    assert_eq!(file.metadata().expect("archive metadata").len(), bytes);
    let export = tempfile::tempdir().expect("temporary export directory");
    tar::Archive::new(file)
      .unpack(export.path())
      .expect("unpack raw Git tree archive");
    assert_eq!(
      std::fs::read_to_string(export.path().join("substituted.txt"))
        .expect("read untransformed blob"),
      "$Format:%H$\n"
    );
    assert_eq!(
      std::fs::read_to_string(export.path().join("exact.txt")).expect("read export-ignored blob"),
      "revision-bound\n"
    );
  }

  #[tokio::test]
  async fn archive_supports_an_empty_commit_tree() {
    let repository = tempfile::tempdir().expect("temporary Git repository");
    let run = |arguments: &[&str]| {
      let output = StdCommand::new("git")
        .arg("-C")
        .arg(repository.path())
        .args(arguments)
        .output()
        .expect("run fixture Git command");
      assert!(
        output.status.success(),
        "git {:?} failed: {}",
        arguments,
        String::from_utf8_lossy(&output.stderr)
      );
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "tenet@example.invalid"]);
    run(&["config", "user.name", "Tenet Test"]);
    run(&["commit", "--allow-empty", "-q", "-m", "empty fixture"]);
    let revision = head(repository.path()).await.expect("empty revision");

    let (file, bytes) = archive(repository.path(), &revision, 4096, 0, 1)
      .await
      .expect("empty raw Git tree archive");
    assert!(bytes > 0);
    assert!(tar::Archive::new(file)
      .entries()
      .expect("empty archive entries")
      .next()
      .is_none());
  }
  #[cfg(unix)]
  #[tokio::test]
  async fn dependency_materialization_preserves_quoted_path_characters() {
    let repository = tempfile::tempdir().expect("temporary Git repository");
    let run = |arguments: &[&str]| {
      let status = StdCommand::new("git")
        .arg("-C")
        .arg(repository.path())
        .args(arguments)
        .status()
        .expect("run fixture Git command");
      assert!(status.success());
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "tenet@example.invalid"]);
    run(&["config", "user.name", "Tenet Test"]);
    std::fs::create_dir(repository.path().join("src")).expect("create source directory");
    let path = "src/quoted\tline\n.rs";
    std::fs::write(repository.path().join(path), "bound\n").expect("write unusual path");
    run(&["add", "-A"]);
    run(&["commit", "-q", "-m", "fixture"]);
    let revision = head(repository.path()).await.expect("fixture revision");
    let blobs = repository_blob_hashes(repository.path(), &revision)
      .await
      .expect("materialize exact paths");
    assert!(blobs.contains_key(path));
  }
}
