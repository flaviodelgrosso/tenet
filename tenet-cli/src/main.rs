mod agents;
mod app;
mod cli;
mod console;
mod run;

use std::process::ExitCode;

use anyhow::Result;
use app::App;

#[tokio::main]
async fn main() -> Result<ExitCode> {
  let app = App::new()?;
  app.run().await
}
