use std::{io::IsTerminal, path::PathBuf, process::ExitCode, sync::Arc};

use anyhow::{Context, Result};
use clap::Parser;
use tenet_acp::acp::AcpRuntime;
use tenet_controller::{controller::manual_verify, AgentBackend};
use tenet_domain::model::RunStatus;
use tenet_runtime::store;
use tenet_storage::{DatabaseHealth, Storage};

use crate::{
  agents,
  cli::{Cli, Command, DbCommand, DumpCommand, EvidenceCommand},
  run::{self, RunOptions},
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
      command: Some(cli.command),
    })
  }

  pub(crate) async fn run(mut self) -> Result<ExitCode> {
    let exit_code = match self.command.take() {
      None => unreachable!("Clap requires an explicit subcommand"),
      Some(Command::Init) => {
        self.initialize().await?;
        ExitCode::SUCCESS
      }
      Some(Command::Run { quiet, verbose } | Command::Resume { quiet, verbose }) => {
        let state = run::run(self.cwd, self.backend, RunOptions { quiet, verbose }).await?;
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
      Some(Command::Db {
        command: DbCommand::Check,
      }) => {
        self.check_database().await?;
        ExitCode::SUCCESS
      }
      Some(Command::State {
        command: DumpCommand::Dump { json },
      }) => {
        self.dump_state(json).await?;
        ExitCode::SUCCESS
      }
      Some(Command::Requirements {
        command: DumpCommand::Dump { json },
      }) => {
        self.dump_requirements(json).await?;
        ExitCode::SUCCESS
      }
      Some(Command::Evidence {
        command: EvidenceCommand::Dump { json, requirement },
      }) => {
        self.dump_evidence(json, requirement.as_deref()).await?;
        ExitCode::SUCCESS
      }
      Some(Command::Roadmap {
        command: DumpCommand::Dump { json },
      }) => {
        self.dump_roadmap(json).await?;
        ExitCode::SUCCESS
      }
    };
    Ok(exit_code)
  }

  async fn initialize(&self) -> Result<()> {
    store::ensure_layout(&self.cwd).await?;
    tenet_domain::config::ensure_config_schema(&self.cwd).await?;
    let config = tenet_domain::config::ensure_config(&self.cwd).await?;
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
    let state = Storage::open_existing(&self.cwd)
      .await?
      .load_current_state()
      .await?;
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
    println!(
      "project checks: {}/{} {}",
      state.verification_layers.project_checks_passed,
      state.verification_layers.project_checks_total,
      if state.verification_layers.project_passed {
        "PASS"
      } else {
        "NOT PASSING"
      }
    );
    println!(
      "semantic obligations: {}/{} SATISFIED ({} gaps, {} uncertain)",
      state.verification_layers.semantic_satisfied,
      state.verification_layers.semantic_obligations_total,
      state.verification_layers.semantic_gaps,
      state.verification_layers.semantic_uncertain
    );
    println!(
      "contradictions: {}",
      state.verification_layers.contradictions
    );
    println!(
      "completion: {}",
      if state.verification_layers.completion_eligible {
        "ELIGIBLE"
      } else {
        "BLOCKED"
      }
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

  async fn check_database(&self) -> Result<()> {
    let storage = Storage::open_existing(&self.cwd).await?;
    let quick = storage.quick_check().await?;
    let foreign_keys = storage.foreign_key_check().await?;
    if quick != DatabaseHealth::Ok || foreign_keys != DatabaseHealth::Ok {
      anyhow::bail!("database check failed: quick={quick:?}, foreign_keys={foreign_keys:?}");
    }
    println!("database: {}", storage.path().display());
    println!("quick_check: ok");
    println!("foreign_key_check: ok");
    Ok(())
  }

  async fn dump_state(&self, _json: bool) -> Result<()> {
    let value = Storage::open_existing(&self.cwd)
      .await?
      .load_current_state()
      .await?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
  }

  async fn dump_requirements(&self, _json: bool) -> Result<()> {
    let value = Storage::open_existing(&self.cwd)
      .await?
      .load_active_catalog()
      .await?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
  }

  async fn dump_evidence(&self, _json: bool, requirement: Option<&str>) -> Result<()> {
    let storage = Storage::open_existing(&self.cwd).await?;
    let catalog = storage
      .load_active_catalog()
      .await?
      .context("no active requirement catalog")?;
    let graph = storage.load_evidence_graph(&catalog).await?;
    if let Some(requirement) = requirement {
      let evidence: Vec<_> = graph
        .evidence
        .values()
        .filter(|item| item.requirement_id.as_str() == requirement)
        .collect();
      println!("{}", serde_json::to_string_pretty(&evidence)?);
    } else {
      println!("{}", serde_json::to_string_pretty(&graph)?);
    }
    Ok(())
  }

  async fn dump_roadmap(&self, _json: bool) -> Result<()> {
    let storage = Storage::open_existing(&self.cwd).await?;
    let state = storage.load_current_state().await?;
    let value = match state.run_id {
      Some(run_id) => storage.load_latest_reconcile_result(&run_id).await?,
      None => None,
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
  }

  async fn verify(&self, json: bool) -> Result<ExitCode> {
    let report = manual_verify(&self.cwd).await?;
    if json {
      println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
      println!("revision: {}", report.revision);
      println!("suite: {}", report.suite_hash);
      for check in &report.checks {
        let mark = if check.result.exit_code == Some(0) && !check.result.timed_out {
          "PASS"
        } else if check.result.timed_out {
          "TIMEOUT"
        } else {
          "FAIL"
        };
        println!(
          "[{mark}] {}: {} ({} ms)",
          check.name, check.result.command, check.result.duration_ms
        );
        if mark != "PASS" {
          if !check.result.stderr.trim().is_empty() {
            eprintln!("{}", check.result.stderr.trim());
          }
          if !check.result.stdout.trim().is_empty() {
            eprintln!("{}", check.result.stdout.trim());
          }
        }
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
