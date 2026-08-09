use std::{collections::BTreeSet, path::Path, process::Stdio, time::Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::{
    io::AsyncReadExt,
    process::Command,
    time::{timeout, Duration},
};

use crate::{
    config::Config,
    model::{CommandResult, VerificationReport},
};

pub async fn verification_commands(cwd: &Path, config: &Config) -> Result<Vec<String>> {
    let mut out = Vec::new();
    out.extend(config.verification.commands.iter().cloned());
    if config.verification.auto_detect {
        out.extend(auto_detect(cwd).await?);
    }
    let mut seen = BTreeSet::new();
    out.retain(|cmd| !cmd.trim().is_empty() && seen.insert(cmd.trim().to_owned()));
    Ok(out)
}

async fn auto_detect(cwd: &Path) -> Result<Vec<String>> {
    let mut commands = Vec::new();
    if is_git_repo(cwd).await {
        commands.push("git diff --check".into());
    }

    let package_json = cwd.join("package.json");
    if package_json.exists() {
        if let Ok(text) = tokio::fs::read_to_string(&package_json).await {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                let scripts = value.get("scripts").and_then(|v| v.as_object());
                let manager = if cwd.join("pnpm-lock.yaml").exists() {
                    "pnpm"
                } else if cwd.join("yarn.lock").exists() {
                    "yarn"
                } else if cwd.join("bun.lock").exists() || cwd.join("bun.lockb").exists() {
                    "bun"
                } else {
                    "npm"
                };
                for name in ["test", "typecheck", "lint", "build"] {
                    if scripts.and_then(|s| s.get(name)).is_some() {
                        let cmd = match manager {
                            "yarn" => format!("yarn {name}"),
                            "bun" => format!("bun run {name}"),
                            _ => format!("{manager} run {name}"),
                        };
                        commands.push(cmd);
                    }
                }
            }
        }
    }

    if (cwd.join("pyproject.toml").exists()
        || cwd.join("setup.py").exists()
        || cwd.join("requirements.txt").exists())
        && cwd.join("tests").exists()
    {
        commands.push("python -m pytest -q".into());
    }
    if cwd.join("go.mod").exists() {
        commands.push("go test ./...".into());
    }
    if cwd.join("Cargo.toml").exists() {
        commands.push("cargo build".into());
        commands.push("cargo fmt --check".into());
        commands.push("cargo clippy --all-targets --all-features -- -D warnings".into());
        commands.push("cargo test --all-targets".into());
    }
    Ok(commands)
}

async fn is_git_repo(cwd: &Path) -> bool {
    if cwd.join(".git").exists() {
        return true;
    }
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

pub async fn run_verification(cwd: &Path, config: &Config) -> Result<VerificationReport> {
    let started_at = Utc::now().to_rfc3339();
    let commands = verification_commands(cwd, config).await?;
    let project_gate_count = commands
        .iter()
        .filter(|cmd| cmd.as_str() != "git diff --check")
        .count();
    let mut warnings = Vec::new();
    if commands.is_empty() {
        warnings.push("No deterministic verification commands detected or configured.".into());
    }
    if config.verification.require_project_gate && project_gate_count == 0 {
        warnings.push("Completion requires at least one project gate; git diff --check alone is insufficient.".into());
    }

    let mut results = Vec::new();
    for command in commands {
        let result = run_shell(
            cwd,
            &command,
            Duration::from_secs(config.verification.timeout_secs),
            config.verification.max_output_bytes,
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
        .all(|r| r.exit_code == Some(0) && !r.timed_out)
        && (!config.verification.require_project_gate || project_gate_count > 0);

    Ok(VerificationReport {
        passed,
        started_at,
        finished_at: Utc::now().to_rfc3339(),
        commands: results,
        warnings,
    })
}

async fn run_shell(
    cwd: &Path,
    command: &str,
    limit: Duration,
    max_output: usize,
) -> Result<CommandResult> {
    let start = Instant::now();
    let mut child = Command::new("sh")
        .arg("-lc")
        .arg(command)
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

    let (status, timed_out) = match timeout(limit, child.wait()).await {
        Ok(status) => (Some(status?), false),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (None, true)
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

fn truncate_utf8(bytes: &[u8], max: usize) -> String {
    let slice = if bytes.len() > max {
        &bytes[..max]
    } else {
        bytes
    };
    let mut text = String::from_utf8_lossy(slice).into_owned();
    if bytes.len() > max {
        text.push_str("\n… output truncated by loops");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn detects_rust_gates() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .await
        .unwrap();
        let cfg = Config::default();
        let cmds = verification_commands(dir.path(), &cfg).await.unwrap();
        assert!(cmds.iter().any(|v| v == "cargo fmt --check"));
        assert!(cmds.iter().any(|v| v.starts_with("cargo clippy")));
        assert!(cmds.iter().any(|v| v.starts_with("cargo test")));
    }
}
