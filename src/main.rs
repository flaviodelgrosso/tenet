mod app;
mod audit;
mod cli;
mod repository;
mod response;
mod verifier;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
  let cli = cli::Cli::parse();
  let json = cli.json_requested();
  match app::run(cli) {
    Ok(exit_code) => exit_code,
    Err(error) => {
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
      ExitCode::from(1)
    }
  }
}
