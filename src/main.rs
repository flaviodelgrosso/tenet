mod backend;
mod config;
mod controller;
mod events;
mod model;
mod prompts;
mod protection;
mod store;
mod tui;
mod verifier;

use std::{
  io::{self, Write},
  path::PathBuf,
  sync::Arc,
};

use anyhow::{bail, Context, Result};
use backend::omp_rpc::OmpRpcBackend;
use clap::{CommandFactory, Parser, Subcommand};
use controller::Controller;
use events::{EventSink, RunEvent};
use model::{RunStatus, WorkerEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
  name = "loops",
  version,
  about = "Autonomous spec-driven development using a headless coding-agent backend"
)]
struct Cli {
  #[arg(long, global = true, value_name = "DIR")]
  cwd: Option<PathBuf>,

  #[command(subcommand)]
  command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
  /// Initialize spec.md and .loops/ in the current project.
  Init,
  /// Start or continue autonomous development.
  Run {
    /// Print a linear transcript instead of opening the full-screen TUI.
    #[arg(long)]
    no_tui: bool,
  },
  /// Alias for run; state is always resumed from .loops/.
  Resume {
    #[arg(long)]
    no_tui: bool,
  },
  /// Show persisted controller state.
  Status {
    #[arg(long)]
    json: bool,
  },
  /// Run deterministic verification without invoking an LLM.
  Verify {
    #[arg(long)]
    json: bool,
  },
}

#[tokio::main]
async fn main() -> Result<()> {
  let cli = Cli::parse();

  let Some(command) = cli.command else {
    Cli::command().print_help()?;
    println!();
    return Ok(());
  };

  let cwd = cli
    .cwd
    .unwrap_or(std::env::current_dir().context("current directory")?);

  let backend: Arc<dyn backend::AgentBackend> = Arc::new(OmpRpcBackend);

  match command {
    Command::Init => {
      let controller = Controller::new(cwd.clone(), backend, EventSink::new(None));
      let state = controller.initialize().await?;
      println!("Initialized loops in {}", cwd.display());
      println!("  spec: {}/spec.md", cwd.display());
      println!("  state: {}/.loops/state.json", cwd.display());
      if !command_exists("omp").await {
        eprintln!("warning: `omp` was not found in PATH; install Oh My Pi before `loops run`");
      }
      println!("status: {:?}", state.status);
    }
    Command::Run { no_tui } | Command::Resume { no_tui } => {
      if !command_exists("omp").await {
        bail!("`omp` is not available in PATH; loops uses `omp --mode rpc --no-session` as its default coding-agent backend");
      }
      let state = if no_tui {
        run_plain(cwd, backend).await?
      } else {
        tui::run(cwd, backend).await?
      };
      if matches!(
        state.status,
        RunStatus::Blocked | RunStatus::Failed | RunStatus::Stopped
      ) {
        std::process::exit(2);
      }
    }
    Command::Status { json } => {
      let state = store::read_state(&cwd).await?;
      if json {
        println!("{}", serde_json::to_string_pretty(&state)?);
      } else {
        println!("status: {:?}", state.status);
        println!("phase: {:?}", state.phase);
        println!("cycle: {}", state.cycle);
        println!(
          "requirements: {}/{} satisfied",
          state.requirement_counts.satisfied, state.requirement_counts.total
        );
        if let Some(work) = state.current_work_unit {
          println!("work unit: {} · {}", work.id, work.title);
        }
        println!("summary: {}", state.last_summary);
        if let Some(reason) = state.blocked_reason {
          println!("blocked: {reason}");
        }
        if let Some(error) = state.last_error {
          println!("error: {error}");
        }
      }
    }
    Command::Verify { json } => {
      let report = controller::manual_verify(&cwd).await?;
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
      if !report.passed {
        std::process::exit(1);
      }
    }
  }
  Ok(())
}

async fn run_plain(cwd: PathBuf, backend: Arc<dyn backend::AgentBackend>) -> Result<model::State> {
  let (tx, mut rx) = mpsc::unbounded_channel();
  let controller = Controller::new(cwd, backend, EventSink::new(Some(tx)));
  let cancel = CancellationToken::new();
  let task_cancel = cancel.clone();
  let mut task = tokio::spawn(async move { controller.run(task_cancel).await });

  loop {
    tokio::select! {
        event = rx.recv() => {
            if let Some(event) = event { print_plain_event(event)?; }
        }
        result = &mut task => return result?,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\nstop requested; aborting active worker...");
            cancel.cancel();
        }
    }
  }
}

fn print_plain_event(event: RunEvent) -> Result<()> {
  match event {
    RunEvent::State(state) => eprintln!(
      "\n[loops] {:?}/{:?} cycle={} {}/{} · {}",
      state.status,
      state.phase,
      state.cycle,
      state.requirement_counts.satisfied,
      state.requirement_counts.total,
      state.last_summary
    ),
    RunEvent::Message(message) => eprintln!("[loops] {message}"),
    RunEvent::Reconcile(result) => eprintln!("[reconcile] {}", result.summary),
    RunEvent::Verification(report) => {
      for command in report.commands {
        eprintln!(
          "[verify:{}] {}",
          if command.exit_code == Some(0) && !command.timed_out {
            "pass"
          } else {
            "fail"
          },
          command.command
        );
      }
    }
    RunEvent::Worker(event) => match event {
      WorkerEvent::Start { role, .. } => {
        eprintln!("\n=== {} ===", role.as_str().to_uppercase())
      }
      WorkerEvent::Text { delta, .. } => {
        print!("{}", sanitize_terminal_text(&delta));
        io::stdout().flush()?;
      }
      WorkerEvent::ToolStart {
        tool_name, args, ..
      } => eprintln!("\n> {tool_name} {}", serde_json::to_string(&args)?),
      WorkerEvent::ToolEnd {
        tool_name,
        is_error,
        ..
      } => eprintln!("{} {tool_name}", if is_error { "ERR" } else { "OK" }),
      WorkerEvent::End {
        role, ok, message, ..
      } => eprintln!(
        "\n--- {} {} {} ---",
        role.as_str(),
        if ok { "done" } else { "failed" },
        message.unwrap_or_default()
      ),
    },
    RunEvent::Finished(state) => eprintln!(
      "\n[loops] finished: {:?} · {}",
      state.status, state.last_summary
    ),
  }
  Ok(())
}

async fn command_exists(command: &str) -> bool {
  tokio::process::Command::new(command)
    .arg("--version")
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status()
    .await
    .map(|s| s.success())
    .unwrap_or(false)
}

fn sanitize_terminal_text(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut chars = text.chars().peekable();
  while let Some(ch) = chars.next() {
    if ch == '\u{1b}' {
      if chars.peek() == Some(&'[') {
        chars.next();
        for next in chars.by_ref() {
          if ('@'..='~').contains(&next) {
            break;
          }
        }
      } else {
        let _ = chars.next();
      }
      continue;
    }
    if ch == '\n' || ch == '\t' || !ch.is_control() {
      out.push(ch);
    }
  }
  out
}
