//! Compatibility entry points for the Tenet terminal console.

#[path = "action.rs"]
mod action;
#[path = "app.rs"]
mod app;
#[path = "keymap.rs"]
mod keymap;
#[path = "layout.rs"]
mod layout;
#[path = "render.rs"]
mod render;
#[path = "terminal.rs"]
mod terminal;
#[path = "theme.rs"]
mod theme;

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::Result;
use crossterm::event::{self, Event};
use tenet_controller::{AgentBackend, Controller};
use tenet_domain::{
  events::{EventSink, RunEvent},
  model::State,
};
use tenet_runtime::store;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use action::Effect;
use app::Application;

/// Opens the operations console without starting a controller run.
pub async fn idle(cwd: PathBuf, backend: Arc<dyn AgentBackend>) -> Result<State> {
  open(cwd, backend, false).await
}

/// Opens the operations console and starts the controller immediately.
pub async fn run(cwd: PathBuf, backend: Arc<dyn AgentBackend>) -> Result<State> {
  open(cwd, backend, true).await
}

async fn open(
  cwd: PathBuf,
  backend: Arc<dyn AgentBackend>,
  start_immediately: bool,
) -> Result<State> {
  let project_name = cwd
    .file_name()
    .and_then(|name| name.to_str())
    .unwrap_or("project")
    .to_owned();
  let state = store::read_state(&cwd).await?;
  let catalog = store::read_catalog(&cwd)
    .await?
    .map_or_else(Vec::new, |catalog| catalog.requirements);
  let (tx, mut rx) = mpsc::unbounded_channel();
  let cancel = CancellationToken::new();
  let mut app = Application::new(project_name, state, catalog);
  let mut task = None;
  if start_immediately {
    app.begin_run();
    task = Some(spawn_run(&cwd, &backend, &tx, &cancel));
  }

  let mut terminal = terminal::TerminalGuard::enter()?;
  let result = async {
    loop {
      while let Ok(event) = rx.try_recv() {
        app.apply(event);
      }
      terminal.draw(|frame| render::render(frame, &app))?;

      if event::poll(Duration::from_millis(60))? {
        if let Event::Key(key) = event::read()? {
          let action = keymap::map_key(&app, key);
          match app.dispatch(action) {
            Effect::None => {}
            Effect::Start => {
              if task.is_none() {
                app.begin_run();
                task = Some(spawn_run(&cwd, &backend, &tx, &cancel));
              }
            }
            Effect::Stop => cancel.cancel(),
            Effect::Exit => return Ok(app.state().clone()),
          }
        }
      }

      if task.as_ref().is_some_and(|handle| handle.is_finished()) {
        let handle = task.take().expect("finished run task is present");
        match handle.await? {
          Ok(state) => app.apply(RunEvent::Finished(state)),
          Err(_) => {
            while let Ok(event) = rx.try_recv() {
              app.apply(event);
            }
            app.apply(RunEvent::Finished(store::read_state(&cwd).await?));
          }
        }
      }
    }
  }
  .await;

  let restore = terminal.restore();
  match (result, restore) {
    (Err(error), _) => Err(error),
    (Ok(_), Err(error)) => Err(error),
    (Ok(state), Ok(())) => Ok(state),
  }
}

fn spawn_run(
  cwd: &std::path::Path,
  backend: &Arc<dyn AgentBackend>,
  tx: &mpsc::UnboundedSender<RunEvent>,
  cancel: &CancellationToken,
) -> tokio::task::JoinHandle<Result<State>> {
  let controller = Controller::new(
    cwd.to_path_buf(),
    backend.clone(),
    EventSink::new(Some(tx.clone())),
  );
  let cancel = cancel.clone();
  tokio::spawn(async move { controller.run(cancel).await })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
