use std::{
  path::{Component, Path, PathBuf},
  process::Stdio,
  time::Instant,
};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use tokio::{
  io::AsyncReadExt,
  process::Command,
  time::{timeout, Duration},
};
use tokio_util::sync::CancellationToken;

use tenet_domain::{
  config::Config,
  ids::VerificationRunId,
  model::{CommandResult, VerificationReport},
  verification::{
    ProjectCheckResult, ProjectVerificationRun, VerificationExecutionRequest,
    VerificationExecutionResult, VerificationSpec,
  },
};

use crate::{git, workspace::WorkspaceManager};

pub async fn run_project_verification_isolated(
  repository: &Path,
  workspaces: &WorkspaceManager,
  revision: &str,
  config: &Config,
  purpose: &str,
  cancel: &CancellationToken,
) -> Result<ProjectVerificationRun> {
  if config.verification.checks.is_empty() {
    bail!("No trusted project verification checks are configured.\n\nConfigure at least one:\n\n[[verification.checks]]\nname = \"project verification\"\ncommand = [\"./verify\"]");
  }
  let canonical_before = git::repository_state(repository).await?;
  let started_at = Utc::now();
  let mut checks = Vec::new();
  for (index, check) in config.verification.checks.iter().enumerate() {
    let spec = check.verification_spec()?;
    let workspace = workspaces
      .create_disposable(&format!("{purpose}-{index}"), revision)
      .await?;
    let result = run_spec(
      &workspace,
      &spec,
      Duration::from_secs(check.effective_timeout_secs(config.verification.timeout_secs)),
      config.verification.max_output_bytes,
      cancel,
    )
    .await;
    let cleanup = workspaces.remove(&workspace).await;
    let result = match (result, cleanup) {
      (Ok(result), Ok(())) => result,
      (Err(error), Ok(())) => return Err(error),
      (Ok(_), Err(cleanup)) => {
        return Err(cleanup).context("discard project verification worktree")
      }
      (Err(error), Err(cleanup)) => {
        return Err(error).context(format!(
          "project verification failed; cleanup also failed: {cleanup:#}"
        ))
      }
    };
    let failed = result.exit_code != Some(0) || result.timed_out;
    checks.push(ProjectCheckResult {
      name: check.name.clone(),
      spec,
      timeout_secs: check.effective_timeout_secs(config.verification.timeout_secs),
      result,
    });
    if failed {
      break;
    }
  }
  if git::repository_state(repository).await? != canonical_before {
    bail!("project verification command modified canonical repository state");
  }
  let passed = checks.len() == config.verification.checks.len()
    && checks
      .iter()
      .all(|check| check.result.exit_code == Some(0) && !check.result.timed_out);
  Ok(ProjectVerificationRun {
    run_id: VerificationRunId::new(),
    revision: revision.into(),
    suite_hash: config.verification.suite_hash()?,
    checks,
    passed,
    started_at,
    finished_at: Utc::now(),
  })
}

pub async fn run_execution_requests_cancelled(
  cwd: &Path,
  config: &Config,
  requests: &[VerificationExecutionRequest],
  cancel: &CancellationToken,
) -> Result<VerificationReport> {
  let started_at = Utc::now();
  let mut commands = Vec::new();
  let mut executions = Vec::new();
  for request in requests {
    let result = run_spec(
      cwd,
      &request.spec,
      Duration::from_secs(config.verification.timeout_secs),
      config.verification.max_output_bytes,
      cancel,
    )
    .await?;
    let failed = result.exit_code != Some(0) || result.timed_out;
    commands.push(result.clone());
    executions.push(VerificationExecutionResult {
      run_id: request.run_id,
      obligation_id: request.obligation_id.clone(),
      spec: request.spec.clone(),
      authority: request.authority,
      result,
    });
    if failed {
      break;
    }
  }
  let passed = commands
    .iter()
    .all(|result| result.exit_code == Some(0) && !result.timed_out);
  Ok(VerificationReport {
    passed,
    started_at,
    finished_at: Utc::now(),
    commands,
    executions,
    warnings: Vec::new(),
  })
}

pub async fn run_verification_with_checks_cancelled<'a>(
  cwd: &Path,
  config: &Config,
  suggested_checks: impl IntoIterator<Item = &'a str>,
  cancel: &CancellationToken,
) -> Result<VerificationReport> {
  let specs: Vec<_> = suggested_checks
    .into_iter()
    .filter(|command| !command.trim().is_empty())
    .map(advisory_shell_spec)
    .collect();
  run_specs(cwd, config, specs.iter(), cancel).await
}

pub async fn run_suggested_checks_cancelled<'a>(
  cwd: &Path,
  config: &Config,
  suggested_checks: impl IntoIterator<Item = &'a str>,
  cancel: &CancellationToken,
) -> Result<VerificationReport> {
  let specs: Vec<_> = suggested_checks
    .into_iter()
    .filter(|command| !command.trim().is_empty())
    .map(advisory_shell_spec)
    .collect();
  run_specs(cwd, config, specs.iter(), cancel).await
}

fn advisory_shell_spec(command: &str) -> VerificationSpec {
  #[cfg(windows)]
  let (program, args) = (
    std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into()),
    vec!["/D".into(), "/C".into(), command.into()],
  );
  #[cfg(not(windows))]
  let (program, args) = ("sh".into(), vec!["-c".into(), command.into()]);

  VerificationSpec {
    program,
    args,
    working_directory: ".".into(),
    environment: Default::default(),
  }
}

async fn run_specs<'a>(
  cwd: &Path,
  config: &Config,
  specs: impl IntoIterator<Item = &'a VerificationSpec>,
  cancel: &CancellationToken,
) -> Result<VerificationReport> {
  let started_at = Utc::now();
  let specs: Vec<_> = specs.into_iter().cloned().collect();
  let mut results = Vec::new();
  for spec in &specs {
    let result = run_spec(
      cwd,
      spec,
      Duration::from_secs(config.verification.timeout_secs),
      config.verification.max_output_bytes,
      cancel,
    )
    .await?;
    let failed = result.exit_code != Some(0) || result.timed_out;
    results.push(result);
    if failed {
      break;
    }
  }
  let warnings = Vec::new();
  let passed = results
    .iter()
    .all(|result| result.exit_code == Some(0) && !result.timed_out);
  Ok(VerificationReport {
    passed,
    started_at,
    finished_at: Utc::now(),
    commands: results,
    executions: Vec::new(),
    warnings,
  })
}

fn resolve_working_directory(workspace: &Path, relative: &str) -> Result<PathBuf> {
  let relative = Path::new(relative);
  if relative.as_os_str().is_empty()
    || relative.is_absolute()
    || relative
      .components()
      .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
  {
    bail!("verification working directory must be a relative path within the workspace");
  }
  let workspace = workspace
    .canonicalize()
    .context("canonicalize verification workspace")?;
  let directory = workspace.join(relative);
  let directory = directory.canonicalize().with_context(|| {
    format!(
      "canonicalize verification working directory {}",
      directory.display()
    )
  })?;
  if !directory.starts_with(&workspace) {
    bail!("verification working directory escapes the workspace");
  }
  Ok(directory)
}

async fn run_spec(
  workspace: &Path,
  spec: &VerificationSpec,
  limit: Duration,
  max_output: usize,
  cancel: &CancellationToken,
) -> Result<CommandResult> {
  if spec.program.trim().is_empty() {
    bail!("verification program must not be blank");
  }
  let working_directory = resolve_working_directory(workspace, &spec.working_directory)?;
  let identity = spec.identity();
  let start = Instant::now();
  let mut command = Command::new(&spec.program);
  command
    .args(&spec.args)
    .envs(&spec.environment)
    .current_dir(working_directory)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
  #[cfg(unix)]
  {
    use std::os::unix::process::CommandExt;
    command.as_std_mut().process_group(0);
  }
  let mut child = command
    .spawn()
    .with_context(|| format!("spawn verification specification: {identity}"))?;
  let process_id = child.id();

  let mut stdout = child
    .stdout
    .take()
    .context("verification stdout unavailable")?;
  let mut stderr = child
    .stderr
    .take()
    .context("verification stderr unavailable")?;
  let stdout_task = tokio::spawn(async move {
    let mut buffer = Vec::new();
    stdout.read_to_end(&mut buffer).await.map(|_| buffer)
  });
  let stderr_task = tokio::spawn(async move {
    let mut buffer = Vec::new();
    stderr.read_to_end(&mut buffer).await.map(|_| buffer)
  });

  let (status, timed_out) = tokio::select! {
    _ = cancel.cancelled() => {
      terminate(&mut child, process_id).await;
      bail!("run cancelled during verification specification: {identity}");
    }
    result = timeout(limit, child.wait()) => match result {
      Ok(status) => (Some(status?), false),
      Err(_) => {
        terminate(&mut child, process_id).await;
        (None, true)
      }
    }
  };
  terminate_process_group(process_id);

  let stdout = stdout_task.await??;
  let stderr = stderr_task.await??;
  Ok(CommandResult {
    command: identity,
    exit_code: status.and_then(|status| status.code()),
    timed_out,
    duration_ms: start.elapsed().as_millis(),
    stdout: truncate_utf8(&stdout, max_output),
    stderr: truncate_utf8(&stderr, max_output),
  })
}

async fn terminate(child: &mut tokio::process::Child, process_id: Option<u32>) {
  terminate_process_group(process_id);
  let _ = child.kill().await;
  let _ = child.wait().await;
}

#[cfg(unix)]
fn terminate_process_group(process_id: Option<u32>) {
  if let Some(process_id) = process_id.and_then(|id| i32::try_from(id).ok()) {
    // SAFETY: the spawned command owns a dedicated process group whose id is its process id.
    unsafe {
      libc::killpg(process_id, libc::SIGKILL);
    }
  }
}

#[cfg(not(unix))]
fn terminate_process_group(_process_id: Option<u32>) {}

fn truncate_utf8(bytes: &[u8], max: usize) -> String {
  let slice = if bytes.len() > max {
    &bytes[..max]
  } else {
    bytes
  };
  let mut text = String::from_utf8_lossy(slice).into_owned();
  if bytes.len() > max {
    text.push_str("\n… output truncated by tenet");
  }
  text
}

#[cfg(test)]
mod tests {
  use super::*;
  #[cfg(unix)]
  fn project_check(name: &str, script: &str) -> tenet_domain::config::ProjectVerificationCheck {
    tenet_domain::config::ProjectVerificationCheck {
      name: name.into(),
      command: vec!["sh".into(), "-c".into(), script.into()],
      working_directory: ".".into(),
      environment: Default::default(),
      timeout_secs: None,
    }
  }

  #[cfg(unix)]
  fn git_repository() -> tempfile::TempDir {
    use std::process::Command as StdCommand;

    let repository = tempfile::tempdir().expect("repository");
    let run = |args: &[&str]| {
      let status = StdCommand::new("git")
        .args([
          "-c",
          "user.name=Tenet Test",
          "-c",
          "user.email=tenet@example.com",
        ])
        .args(args)
        .current_dir(repository.path())
        .status()
        .expect("run git");
      assert!(status.success(), "git command failed: {args:?}");
    };
    run(&["init", "-q"]);
    std::fs::write(repository.path().join("tracked"), "base\n").expect("tracked file");
    run(&["add", "tracked"]);
    run(&["commit", "-q", "-m", "base"]);
    repository
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn project_checks_run_in_order_in_independent_workspaces() {
    let repository = git_repository();
    let revision = git::head(repository.path()).await.expect("head");
    let workspaces = WorkspaceManager::new(repository.path().to_path_buf(), "suite-order");
    let mut config = Config::default();
    config.verification.checks = vec![
      project_check("first", "test ! -e mutation && touch mutation"),
      project_check("second", "test ! -e mutation && touch mutation"),
    ];

    let report = run_project_verification_isolated(
      repository.path(),
      &workspaces,
      &revision,
      &config,
      "project-check",
      &CancellationToken::new(),
    )
    .await
    .expect("project verification");

    assert!(report.passed);
    assert_eq!(
      report
        .checks
        .iter()
        .map(|check| check.name.as_str())
        .collect::<Vec<_>>(),
      ["first", "second"]
    );
    assert!(!repository.path().join("mutation").exists());
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn project_checks_fail_fast_after_first_failure() {
    let repository = git_repository();
    let revision = git::head(repository.path()).await.expect("head");
    let workspaces = WorkspaceManager::new(repository.path().to_path_buf(), "suite-fail-fast");
    let mut config = Config::default();
    config.verification.checks = vec![
      project_check("first", "true"),
      project_check("failure", "false"),
      project_check("not-run", "true"),
    ];

    let report = run_project_verification_isolated(
      repository.path(),
      &workspaces,
      &revision,
      &config,
      "project-check",
      &CancellationToken::new(),
    )
    .await
    .expect("project verification");

    assert!(!report.passed);
    assert_eq!(
      report
        .checks
        .iter()
        .map(|check| check.name.as_str())
        .collect::<Vec<_>>(),
      ["first", "failure"]
    );
  }

  #[test]
  fn working_directory_rejects_parent_traversal() {
    let workspace = tempfile::tempdir().expect("workspace");
    let error = resolve_working_directory(workspace.path(), "../outside")
      .expect_err("parent traversal rejected");
    assert!(error.to_string().contains("relative path"));
  }

  #[test]
  fn truncation_preserves_utf8_boundary() {
    assert_eq!(
      truncate_utf8("éé".as_bytes(), 3),
      "é�\n… output truncated by tenet"
    );
  }

  #[cfg(not(windows))]
  #[test]
  fn advisory_shell_does_not_load_user_profile() {
    let spec = advisory_shell_spec("true");

    assert_eq!(spec.program, "sh");
    assert_eq!(spec.args, ["-c", "true"]);
  }

  #[cfg(not(windows))]
  #[tokio::test]
  async fn verification_inherits_host_tool_environment() {
    let workspace = tempfile::tempdir().expect("workspace");
    let expected_path = std::env::var("PATH").expect("test process PATH");
    let spec = VerificationSpec {
      program: "/bin/sh".into(),
      args: vec!["-c".into(), "printf %s \"$PATH\"".into()],
      working_directory: ".".into(),
      environment: Default::default(),
    };

    let result = run_spec(
      workspace.path(),
      &spec,
      Duration::from_secs(5),
      4096,
      &CancellationToken::new(),
    )
    .await
    .expect("run verification");

    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout, expected_path);
  }

  #[cfg(not(windows))]
  #[tokio::test]
  async fn verification_spec_environment_overrides_host_environment() {
    let workspace = tempfile::tempdir().expect("workspace");
    let spec = VerificationSpec {
      program: "/bin/sh".into(),
      args: vec!["-c".into(), "printf %s \"$TENET_ENV_TEST\"".into()],
      working_directory: ".".into(),
      environment: [("TENET_ENV_TEST".into(), "specified".into())]
        .into_iter()
        .collect(),
    };

    let result = run_spec(
      workspace.path(),
      &spec,
      Duration::from_secs(5),
      4096,
      &CancellationToken::new(),
    )
    .await
    .expect("run verification");

    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout, "specified");
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn timed_out_command_terminates_descendants_that_hold_output_open() {
    let workspace = tempfile::tempdir().expect("workspace");
    let spec = VerificationSpec {
      program: "/bin/sh".into(),
      args: vec!["-c".into(), "sleep 30".into()],
      working_directory: ".".into(),
      environment: Default::default(),
    };

    let result = timeout(
      Duration::from_secs(2),
      run_spec(
        workspace.path(),
        &spec,
        Duration::from_millis(10),
        4096,
        &CancellationToken::new(),
      ),
    )
    .await
    .expect("descendant must not keep timed-out verification pending")
    .expect("run verification");

    assert!(result.timed_out);
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn completed_command_terminates_background_processes_that_hold_output_open() {
    let workspace = tempfile::tempdir().expect("workspace");
    let spec = VerificationSpec {
      program: "/bin/sh".into(),
      args: vec!["-c".into(), "sleep 30 &".into()],
      working_directory: ".".into(),
      environment: Default::default(),
    };

    let result = timeout(
      Duration::from_secs(2),
      run_spec(
        workspace.path(),
        &spec,
        Duration::from_secs(5),
        4096,
        &CancellationToken::new(),
      ),
    )
    .await
    .expect("background process must not keep verification pending")
    .expect("run verification");

    assert_eq!(result.exit_code, Some(0));
  }
}
