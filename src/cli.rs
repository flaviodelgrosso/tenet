use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
  name = "tenet",
  about = "Evidence-backed completion authority for exact Git revisions"
)]
pub struct Cli {
  #[arg(long, global = true, value_name = "DIR")]
  pub cwd: Option<PathBuf>,
  #[command(subcommand)]
  pub command: Command,
}
impl Cli {
  pub fn json_requested(&self) -> bool {
    match &self.command {
      Command::Init { json, .. }
      | Command::Status { json }
      | Command::Gate { json, .. }
      | Command::Evidence { json, .. } => *json,
      Command::Contract { command } => match command {
        ContractCommand::Schema { json }
        | ContractCommand::Propose { json, .. }
        | ContractCommand::Approve { json, .. } => *json,
      },
    }
  }
}

#[derive(Subcommand)]
pub enum Command {
  /// Initialize repository-local Tenet policy and workflow instructions, creating a missing specification.
  Init {
    #[arg(long, value_name = "PATH")]
    spec: PathBuf,
    #[arg(long)]
    json: bool,
  },
  /// Report repository, contract, and last-gate state without running verifiers.
  Status {
    #[arg(long)]
    json: bool,
  },
  /// Propose, inspect, or admit a completion contract.
  Contract {
    #[command(subcommand)]
    command: ContractCommand,
  },
  /// Evaluate one exact candidate commit under one exact authority commit.
  Gate {
    /// Exact trusted commit defining specification, policy, and admitted contract.
    #[arg(long)]
    authority_revision: String,
    /// Exact candidate commit containing the software to evaluate.
    #[arg(long)]
    revision: String,
    #[arg(long)]
    json: bool,
  },
  /// Explain persisted observations and gate decisions.
  Evidence {
    #[arg(long)]
    revision: Option<String>,
    #[arg(long)]
    json: bool,
  },
}

#[derive(Subcommand)]
pub enum ContractCommand {
  /// Emit the proposal JSON Schema generated from Tenet's Rust request type.
  Schema {
    #[arg(long)]
    json: bool,
  },
  /// Validate and store a proposal pending operator admission.
  Propose {
    #[arg(long, value_name = "PATH")]
    file: PathBuf,
    #[arg(long)]
    json: bool,
  },
  /// Admit the exact identified proposal as the canonical contract.
  Approve {
    #[arg(long)]
    proposal: String,
    #[arg(long)]
    digest: String,
    #[arg(long)]
    json: bool,
  },
}
