use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
  name = "tenet",
  about = "Autonomous spec-driven development using vendor-neutral ACP agents"
)]
pub(crate) struct Cli {
  #[arg(long, global = true, value_name = "DIR")]
  pub(crate) cwd: Option<PathBuf>,

  #[command(subcommand)]
  pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
  /// Initialize tenet.toml and .tenet/ in the current project.
  Init,
  /// Discover and configure agents from the canonical ACP Registry.
  #[command(name = "agents")]
  Agent {
    #[command(subcommand)]
    command: Option<AgentCommand>,
  },
  /// Start or continue autonomous development.
  Run {
    /// Only print outcome-changing work, verification, errors, and the final state.
    #[arg(long, short)]
    quiet: bool,
    /// Include worker narrative and diagnostic tool details.
    #[arg(long, short)]
    verbose: bool,
  },
  /// Alias for run; state is always resumed from .tenet/.
  Resume {
    /// Only print outcome-changing work, verification, errors, and the final state.
    #[arg(long, short)]
    quiet: bool,
    /// Include worker narrative and diagnostic tool details.
    #[arg(long, short)]
    verbose: bool,
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
  /// Check the authoritative SQLite database.
  Db {
    #[command(subcommand)]
    command: DbCommand,
  },
  /// Export the current run-state projection.
  State {
    #[command(subcommand)]
    command: DumpCommand,
  },
  /// Export the active requirement catalog.
  Requirements {
    #[command(subcommand)]
    command: DumpCommand,
  },
  /// Export semantic and project evidence.
  Evidence {
    #[command(subcommand)]
    command: EvidenceCommand,
  },
  /// Export the latest reconciliation roadmap.
  Roadmap {
    #[command(subcommand)]
    command: DumpCommand,
  },
}

#[derive(Subcommand)]
pub(crate) enum DbCommand {
  /// Run quick, foreign-key, and migration diagnostics.
  Check,
}

#[derive(Subcommand)]
pub(crate) enum DumpCommand {
  /// Emit deterministic JSON to stdout.
  Dump {
    #[arg(long)]
    json: bool,
  },
}

#[derive(Subcommand)]
pub(crate) enum EvidenceCommand {
  /// Emit deterministic JSON evidence to stdout.
  Dump {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    requirement: Option<String>,
  },
}

#[derive(Subcommand)]
pub(crate) enum AgentCommand {
  /// List Registry agents by display name and authoritative ID.
  List,
  /// Search Registry agents by display name or ID.
  Search { query: String },
  /// Select a Registry agent for this project.
  Select { id: String },
  /// Download and install a Registry binary agent with explicit confirmation.
  #[command(visible_alias = "install")]
  Setup {
    /// Registry ID to install; defaults to this project's selected Registry agent.
    id: Option<String>,
    /// Confirm that the Registry binary distribution may be installed.
    #[arg(long)]
    yes: bool,
  },
  /// Report source configuration and generic ACP launch readiness.
  Doctor,
  /// List ACP authentication methods or invoke an advertised agent-owned method.
  Login { method: Option<String> },
}

#[cfg(test)]
mod tests {
  use clap::Parser;

  use super::{Cli, Command};

  #[test]
  fn commandless_invocation_requires_an_explicit_command() {
    assert!(Cli::try_parse_from(["tenet"]).is_err());
  }

  #[test]
  fn run_uses_console_mode_without_a_mode_flag() {
    let cli = Cli::try_parse_from(["tenet", "run"]).unwrap();
    assert!(matches!(
      cli.command,
      Command::Run {
        quiet: false,
        verbose: false,
      }
    ));
  }

  #[test]
  fn database_check_command_parses() {
    let cli = Cli::try_parse_from(["tenet", "db", "check"]).expect("parse db check");
    assert!(matches!(
      cli.command,
      Command::Db {
        command: super::DbCommand::Check
      }
    ));
  }

  #[test]
  fn evidence_dump_accepts_requirement_filter() {
    let cli = Cli::try_parse_from([
      "tenet",
      "evidence",
      "dump",
      "--json",
      "--requirement",
      "REQ-001",
    ])
    .expect("parse evidence dump");
    assert!(matches!(
      cli.command,
      Command::Evidence {
        command: super::EvidenceCommand::Dump {
          json: true,
          requirement: Some(_)
        }
      }
    ));
  }
}
