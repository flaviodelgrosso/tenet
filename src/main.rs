mod application;
mod audit;
mod cli;
mod mcp;
mod repository;
mod response;
mod verifier;

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use crate::{
  application::{InitializeRequest, Tenet},
  cli::{Cli, Command},
};

fn main() -> ExitCode {
  let cli = Cli::parse();
  let json = cli.command.json_requested();
  if let Err(error) = run_command(cli) {
    report_error(error, json);
    return ExitCode::FAILURE;
  }
  ExitCode::SUCCESS
}

fn run_command(cli: Cli) -> Result<()> {
  let cwd = cli.cwd.unwrap_or(std::env::current_dir()?);
  match cli.command {
    Command::Init { spec, json } => {
      let result = Tenet::new(cwd).initialize(&InitializeRequest { spec_path: spec })?;
      if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
      } else {
        println!("initialized: {}", result.initialized);
        println!(
          "specification: {} ({})",
          result.spec_path, result.spec_digest
        );
        println!("contract: {:?}", result.contract_state);
        println!("skill: {}", result.skill_path);
      }
      Ok(())
    }
    Command::Mcp => mcp::run(cwd),
  }
}

fn report_error(error: anyhow::Error, json: bool) {
  let error = response::ErrorResult {
    schema_version: 1,
    code: "command_error".into(),
    message: format!("{error:#}"),
  };
  if json {
    match serde_json::to_string_pretty(&error) {
      Ok(encoded) => println!("{encoded}"),
      Err(encoding_error) => eprintln!(
        "error: {}; additionally failed to encode JSON error: {encoding_error}",
        error.message
      ),
    }
  } else {
    eprintln!("error: {}", error.message);
  }
}
