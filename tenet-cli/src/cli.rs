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
  pub(crate) command: Option<Command>,
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
    /// Run without the interactive TUI and stream progress to the console.
    #[arg(long)]
    headless: bool,
    /// Only print outcome-changing work, verification, errors, and the final state.
    #[arg(long, short)]
    quiet: bool,
    /// Include worker narrative and diagnostic tool details.
    #[arg(long, short)]
    verbose: bool,
  },
  /// Alias for run; state is always resumed from .tenet/.
  Resume {
    /// Run without the interactive TUI and stream progress to the console.
    #[arg(long)]
    headless: bool,
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
  fn commandless_invocation_selects_tui_launcher() {
    let cli = Cli::try_parse_from(["tenet"]).unwrap();
    assert!(cli.command.is_none());
  }

  #[test]
  fn headless_and_hidden_compatibility_alias_select_console_mode() {
    let cli = Cli::try_parse_from(["tenet", "run", "--headless"]).unwrap();
    assert!(matches!(
      cli.command,
      Some(Command::Run { headless: true, .. })
    ));
  }
}
