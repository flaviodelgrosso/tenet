use std::{io::IsTerminal, path::PathBuf, process::ExitCode, sync::Arc};

use anyhow::{Context, Result};
use clap::Parser;
use tenet_acp::acp::AcpRuntime;
use tenet_domain::model::RunStatus;
use tenet_runtime::{backend::AgentBackend, controller::manual_verify, store};
use tenet_tui::tui;

use crate::{
  agents,
  cli::{Cli, Command},
  headless::{self, ConsoleOptions},
};

pub(crate) struct App {
  cwd: PathBuf,
  backend: Arc<dyn AgentBackend>,
  command: Option<Command>,
}

impl App {
  pub(crate) fn new() -> Result<Self> {
    let cli = Cli::parse();
    let cur_dir = std::env::current_dir().context("current directory")?;

    Ok(Self {
      cwd: cli.cwd.unwrap_or(cur_dir),
      backend: Arc::new(AcpRuntime),
      command: cli.command,
    })
  }

  pub(crate) async fn run(mut self) -> Result<ExitCode> {
    let exit_code = match self.command.take() {
      None => {
        let _ = tui::idle(self.cwd, self.backend).await?;
        ExitCode::SUCCESS
      }
      Some(Command::Init) => {
        self.initialize().await?;
        ExitCode::SUCCESS
      }
      Some(
        Command::Run {
          headless: is_headless,
          quiet,
          verbose,
        }
        | Command::Resume {
          headless: is_headless,
          quiet,
          verbose,
        },
      ) => {
        let state = if is_headless {
          headless::run(self.cwd, self.backend, ConsoleOptions { quiet, verbose }).await?
        } else {
          tui::run(self.cwd, self.backend).await?
        };
        if matches!(
          state.status,
          RunStatus::Blocked | RunStatus::Failed | RunStatus::Stopped
        ) {
          ExitCode::from(2)
        } else {
          ExitCode::SUCCESS
        }
      }
      Some(Command::Agent { command }) => {
        if agents::handle(&self.cwd, command).await? {
          ExitCode::SUCCESS
        } else {
          ExitCode::from(2)
        }
      }
      Some(Command::Status { json }) => {
        self.print_status(json).await?;
        ExitCode::SUCCESS
      }
      Some(Command::Verify { json }) => self.verify(json).await?,
    };
    Ok(exit_code)
  }

  async fn initialize(&self) -> Result<()> {
    let config = tenet_domain::config::ensure_config(&self.cwd).await?;
    store::ensure_layout(&self.cwd).await?;
    store::ensure_spec(&self.cwd, &config).await?;
    self.print_initialization();
    Ok(())
  }

  fn print_initialization(&self) {
    println!("Initialized tenet in {}", self.cwd.display());
    if std::io::stdout().is_terminal() {
      println!("Run `tenet agents` to browse ACP Registry agents, then `tenet agents select <id>`");
    } else {
      println!("Set exactly one of agent.id (Registry) or [agent.custom] in tenet.toml; no agent was selected automatically");
    }
  }

  async fn print_status(&self, json: bool) -> Result<()> {
    let state = store::read_state(&self.cwd).await?;
    if json {
      println!("{}", serde_json::to_string_pretty(&state)?);
      return Ok(());
    }
    println!("status: {:?}", state.status);
    println!("phase: {:?}", state.phase);
    println!("cycle: {}", state.cycle);
    println!(
      "requirements: {}/{} verified ({} stale, {} contradicted)",
      state.requirement_counts.verified,
      state.requirement_counts.total,
      state.requirement_counts.stale,
      state.requirement_counts.contradicted
    );
    for lease in state.active_leases.values() {
      println!(
        "active work: {} · {} ({})",
        lease.work_unit.id, lease.work_unit.title, lease.worker_id
      );
    }
    println!("summary: {}", state.last_summary);
    if let Some(reason) = state.blocked_reason {
      println!("blocked: {reason}");
    }
    if let Some(error) = state.last_error {
      println!("error: {error}");
    }
    Ok(())
  }

  async fn verify(&self, json: bool) -> Result<ExitCode> {
    let report = manual_verify(&self.cwd).await?;
    if json {
      println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
      for command in &report.commands {
        let mark = if command.exit_code == Some(0) && !command.timed_out {
          "PASS"
        } else {
          "FAIL"
        };
        println!("[{mark}] {} ({} ms)", command.command, command.duration_ms);
        if mark == "FAIL" {
          if !command.stderr.trim().is_empty() {
            eprintln!("{}", command.stderr.trim());
          }
          if !command.stdout.trim().is_empty() {
            eprintln!("{}", command.stdout.trim());
          }
        }
      }
      for warning in &report.warnings {
        eprintln!("warning: {warning}");
      }
      println!(
        "verification: {}",
        if report.passed { "PASS" } else { "FAIL" }
      );
    }
    Ok(if report.passed {
      ExitCode::SUCCESS
    } else {
      ExitCode::from(1)
    })
  }
}
