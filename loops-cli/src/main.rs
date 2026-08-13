mod agents;
mod app;
mod cli;
mod headless;

use std::process::ExitCode;

use anyhow::Result;
use app::App;

#[tokio::main]
async fn main() -> Result<ExitCode> {
  let app = App::new()?;
  app.run().await
}
