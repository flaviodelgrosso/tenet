use std::{path::PathBuf, sync::Arc, time::Instant};

use anyhow::Result;
use tenet_controller::{AgentBackend, Controller};
use tenet_domain::{config::read_config, events::EventSink, model::State};
use tenet_runtime::{git, store};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::console::{ConsoleEvent, ConsolePresenter, ConsoleRenderer, InformationMode, RunHeader};

#[derive(Debug, Clone, Copy)]
pub(crate) struct ConsoleOptions {
  pub quiet: bool,
  pub verbose: bool,
}

pub(crate) async fn run(
  cwd: PathBuf,
  backend: Arc<dyn AgentBackend>,
  options: ConsoleOptions,
) -> Result<State> {
  let initial = store::read_state(&cwd).await?;
  let catalog = store::read_catalog(&cwd).await?;
  let config = read_config(&cwd).await.ok();
  let mode = InformationMode::from_flags(options.quiet, options.verbose);
  let mut console = ConsoleRenderer::stdout(mode);
  if let (Some(config), Ok(initial_revision)) = (config.as_ref(), git::head(&cwd).await) {
    console.header(&RunHeader {
      repository: cwd
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned),
      revision: initial_revision,
      specification: config.spec_file.clone(),
      agent: config.agent.id.clone().or_else(|| {
        config
          .agent
          .custom
          .as_ref()
          .map(|custom| custom.command.clone())
      }),
      requirements: catalog.as_ref().map(|catalog| catalog.requirements.len()),
      verification_checks: config.verification.checks.len(),
      max_cycles: config.max_cycles,
    })?;
  }
  let mut presenter = ConsolePresenter::new(
    config
      .as_ref()
      .map_or(0, |config| config.max_repair_attempts),
  );

  let (tx, mut rx) = mpsc::unbounded_channel();
  let controller = Controller::new(cwd.clone(), backend, EventSink::new(Some(tx)));
  let cancel = CancellationToken::new();
  let task_cancel = cancel.clone();
  let started = Instant::now();
  let mut task = tokio::spawn(async move { controller.run(task_cancel).await });
  let mut events_closed = false;

  loop {
    tokio::select! {
      event = rx.recv(), if !events_closed => match event {
        Some(event) => render_event(&mut presenter, &mut console, &event)?,
        None => events_closed = true,
      },
      result = &mut task => {
        while let Ok(event) = rx.try_recv() {
          render_event(&mut presenter, &mut console, &event)?;
        }
        match result? {
          Ok(state) => {
            console.summary(&state, started.elapsed().as_secs())?;
            return Ok(state);
          }
          Err(error) => {
            let state = store::read_state(&cwd).await.unwrap_or(initial);
            console.summary(&state, started.elapsed().as_secs())?;
            return Err(error);
          }
        }
      },
      _ = tokio::signal::ctrl_c(), if !cancel.is_cancelled() => {
        console.render(&ConsoleEvent::Milestone {
          at: chrono::Local::now().format("%H:%M:%S").to_string(),
          label: "STOP",
          summary: "Cancellation requested · stopping active work gracefully".into(),
        })?;
        cancel.cancel();
      }
    }
  }
}

fn render_event(
  presenter: &mut ConsolePresenter,
  renderer: &mut ConsoleRenderer<std::io::Stdout>,
  event: &tenet_domain::events::RunEvent,
) -> Result<()> {
  for event in presenter.present(event) {
    renderer.render(&event)?;
  }
  Ok(())
}
