use std::{collections::BTreeSet, path::Path, process::Stdio, time::Instant};

use anyhow::{Context, Result};
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
};

pub async fn verification_commands(config: &Config) -> Result<Vec<String>> {
  deduplicate_commands(config.verification.commands.iter().cloned())
}

pub async fn run_verification(cwd: &Path, config: &Config) -> Result<VerificationReport> {
  run_verification_cancelled(cwd, config, &CancellationToken::new()).await
}

pub async fn run_verification_cancelled(
  cwd: &Path,
  config: &Config,
  cancel: &CancellationToken,
) -> Result<VerificationReport> {
  run_commands(cwd, config, verification_commands(config).await?, cancel).await
}

pub async fn run_verification_with_checks_cancelled(
  cwd: &Path,
  config: &Config,
  suggested_checks: &[String],
  cancel: &CancellationToken,
) -> Result<VerificationReport> {
  let commands = deduplicate_commands(
    suggested_checks
      .iter()
      .cloned()
      .chain(config.verification.commands.iter().cloned()),
  )?;
  run_commands(cwd, config, commands, cancel).await
}

pub async fn run_suggested_checks_cancelled(
  cwd: &Path,
  config: &Config,
  suggested_checks: &[String],
  cancel: &CancellationToken,
) -> Result<VerificationReport> {
  run_commands(
    cwd,
    config,
    deduplicate_commands(suggested_checks.iter().cloned())?,
    cancel,
  )
  .await
}

fn deduplicate_commands(commands: impl Iterator<Item = String>) -> Result<Vec<String>> {
  let mut seen = BTreeSet::new();
  Ok(
    commands
      .filter(|command| !command.trim().is_empty())
      .filter(|command| seen.insert(command.trim().to_owned()))
      .collect(),
  )
}

async fn run_commands(
  cwd: &Path,
  config: &Config,
  commands: Vec<String>,
  cancel: &CancellationToken,
) -> Result<VerificationReport> {
  let started_at = Utc::now();
  let project_gate_count = commands
    .iter()
    .filter(|command| command.as_str() != "git diff --check")
    .count();
  let mut warnings = Vec::new();
  if commands.is_empty() {
    warnings.push("No deterministic verification commands configured.".into());
  }
  if config.verification.require_project_gate && project_gate_count == 0 {
    warnings.push(
      "Completion requires at least one project gate; git diff --check alone is insufficient."
        .into(),
    );
  }

  let mut results = Vec::new();
  for command in commands {
    let result = run_shell(
      cwd,
      &command,
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

  let passed = results
    .iter()
    .all(|result| result.exit_code == Some(0) && !result.timed_out)
    && (!config.verification.require_project_gate || project_gate_count > 0);

  Ok(VerificationReport {
    passed,
    started_at,
    finished_at: Utc::now(),
    commands: results,
    warnings,
  })
}

fn shell_command(command: &str) -> Command {
  #[cfg(windows)]
  {
    // `ComSpec` points to the user's configured Windows command interpreter;
    // `cmd.exe` remains a stable fallback for stripped-down environments.
    let shell = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
    let mut command_process = Command::new(shell);
    command_process.args(["/D", "/C", command]);
    command_process
  }

  #[cfg(not(windows))]
  {
    let mut command_process = Command::new("sh");
    command_process.args(["-lc", command]);
    command_process
  }
}

async fn run_shell(
  cwd: &Path,
  command: &str,
  limit: Duration,
  max_output: usize,
  cancel: &CancellationToken,
) -> Result<CommandResult> {
  let start = Instant::now();
  let mut child = shell_command(command)
    .current_dir(cwd)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .with_context(|| format!("spawn verification command: {command}"))?;

  let mut stdout = child
    .stdout
    .take()
    .context("verification stdout unavailable")?;
  let mut stderr = child
    .stderr
    .take()
    .context("verification stderr unavailable")?;
  let stdout_task = tokio::spawn(async move {
    let mut buf = Vec::new();
    stdout.read_to_end(&mut buf).await.map(|_| buf)
  });
  let stderr_task = tokio::spawn(async move {
    let mut buf = Vec::new();
    stderr.read_to_end(&mut buf).await.map(|_| buf)
  });

  let (status, timed_out) = tokio::select! {
    _ = cancel.cancelled() => {
      terminate(&mut child).await;
      anyhow::bail!("run cancelled during verification command: {command}");
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
    command: command.into(),
    exit_code: status.and_then(|s| s.code()),
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

  #[tokio::test]
  async fn run_shell_executes_command_with_platform_shell() {
    let result = run_shell(
      Path::new("."),
      "echo tenet-shell-test",
      Duration::from_secs(5),
      1024,
      &CancellationToken::new(),
    )
    .await
    .expect("platform shell should start");

    assert_eq!(result.exit_code, Some(0));
    assert!(!result.timed_out);
    assert!(result.stdout.contains("tenet-shell-test"));
  }
  #[tokio::test]
  async fn run_shell_marks_timeout_and_terminates_child() {
    let result = run_shell(
      Path::new("."),
      "sleep 1",
      Duration::from_millis(10),
      1024,
      &CancellationToken::new(),
    )
    .await
    .expect("platform shell should start");

    assert!(result.timed_out);
  }
}
