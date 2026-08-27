mod agents;
mod app;
mod cli;
mod console;
mod run;

use std::process::ExitCode;

use anyhow::Result;
use app::App;

fn main() -> Result<ExitCode> {
  let app = App::new()?;
  tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()?
    .block_on(app.run())
}
