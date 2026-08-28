use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
  name = "tenet",
  about = "Evidence-backed completion authority for immutable content"
)]
pub struct Cli {
  #[arg(long, global = true, value_name = "DIR")]
  pub cwd: Option<PathBuf>,
  #[command(subcommand)]
  pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
  /// Initialize a Tenet project policy and workflow instructions.
  Init {
    #[arg(long, value_name = "PATH")]
    spec: Option<PathBuf>,
    #[arg(long)]
    json: bool,
  },
  /// Run the local Model Context Protocol server over standard input and output.
  #[command(hide = true)]
  Mcp,
}

impl Command {
  pub(crate) fn json_requested(&self) -> bool {
    matches!(self, Self::Init { json: true, .. })
  }
}
