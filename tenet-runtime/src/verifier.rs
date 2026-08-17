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
  model::{CommandResult, VerificationReport},
  verification::{VerificationExecutionRequest, VerificationExecutionResult, VerificationSpec},
};

pub async fn run_verification_cancelled(
  cwd: &Path,
  config: &Config,
  cancel: &CancellationToken,
) -> Result<VerificationReport> {
  run_specs(
    cwd,
    config,
    config.verification.gates.iter().map(|gate| &gate.spec),
    cancel,
  )
  .await
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
    .all(|result| result.exit_code == Some(0) && !result.timed_out)
    && (!config.verification.require_project_gate
      || requests
        .iter()
        .any(|request| request.authority.is_trusted()));
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
  let mut specs: Vec<_> = suggested_checks
    .into_iter()
    .filter(|command| !command.trim().is_empty())
    .map(advisory_shell_spec)
    .collect();
  specs.extend(
    config
      .verification
      .gates
      .iter()
      .map(|gate| gate.spec.clone()),
  );
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
  let (program, args) = ("sh".into(), vec!["-lc".into(), command.into()]);

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
  let project_gate_count = config.verification.gates.len();
  let mut warnings = Vec::new();
  if results.is_empty() {
    warnings.push("No deterministic verification gates configured.".into());
  }
  if config.verification.require_project_gate && project_gate_count == 0 {
    warnings.push("Completion requires at least one project-configured gate.".into());
  }
  let passed = results
    .iter()
    .all(|result| result.exit_code == Some(0) && !result.timed_out)
    && (!config.verification.require_project_gate || project_gate_count > 0);
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
  let mut child = Command::new(&spec.program)
    .args(&spec.args)
    .env_clear()
    .envs(&spec.environment)
    .current_dir(working_directory)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .with_context(|| format!("spawn verification specification: {identity}"))?;

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
      terminate(&mut child).await;
      bail!("run cancelled during verification specification: {identity}");
    }
    result = timeout(limit, child.wait()) => match result {
      Ok(status) => (Some(status?), false),
      Err(_) => {
        terminate(&mut child).await;
        (None, true)
      }
    }
  };

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

async fn terminate(child: &mut tokio::process::Child) {
  let _ = child.kill().await;
  let _ = child.wait().await;
}

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
}
