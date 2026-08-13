use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use loops_domain::{
  events::{EventSink, RunEvent},
  model::{State, WorkerEvent},
};
use loops_runtime::{backend::AgentBackend, controller::Controller};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(crate) async fn run(cwd: PathBuf, backend: Arc<dyn AgentBackend>) -> Result<State> {
  let (tx, mut rx) = mpsc::unbounded_channel();
  let controller = Controller::new(cwd, backend, EventSink::new(Some(tx)));
  let cancel = CancellationToken::new();
  let task_cancel = cancel.clone();
  let mut task = tokio::spawn(async move { controller.run(task_cancel).await });
  let mut events_closed = false;

  loop {
    tokio::select! {
      event = rx.recv(), if !events_closed => match event {
        Some(event) => print_event(event)?,
        None => events_closed = true,
      },
      result = &mut task => return result?,
      _ = tokio::signal::ctrl_c(), if !cancel.is_cancelled() => {
        eprintln!("\nstop requested; aborting active worker...");
        cancel.cancel();
      }
    }
  }
}

fn print_event(event: RunEvent) -> Result<()> {
  match event {
    RunEvent::State(state) => eprintln!(
      "\n[loops] {:?}/{:?} cycle={} {}/{} · {}",
      state.status,
      state.phase,
      state.cycle,
      state.requirement_counts.satisfied,
      state.requirement_counts.total,
      state.last_summary
    ),
    RunEvent::Catalog(catalog) => eprintln!(
      "[requirements] {} catalog entries",
      catalog.requirements.len()
    ),
    RunEvent::Message(message) => eprintln!("[loops] {message}"),
    RunEvent::Reconcile(result) => eprintln!("[reconcile] {}", result.summary),
    RunEvent::Verification(report) => {
      for command in report.commands {
        eprintln!(
          "[verify:{}] {}",
          if command.exit_code == Some(0) && !command.timed_out {
            "pass"
          } else {
            "fail"
          },
          command.command
        );
      }
    }
    RunEvent::RepositoryChanges(changes) => {
      eprintln!("[repository] {} changed files", changes.len())
    }
    RunEvent::Worker(event) => match event {
      WorkerEvent::Start { role, .. } => eprintln!("\n=== {} ===", role.as_str().to_uppercase()),
      WorkerEvent::Text { delta, .. } => {
        print!("{}", sanitize_terminal_text(&delta));
        io::stdout().flush()?;
      }
      WorkerEvent::ToolStart {
        tool_name, args, ..
      } => eprintln!("\n> {tool_name} {}", serde_json::to_string(&args)?),
      WorkerEvent::ToolEnd {
        tool_name,
        is_error,
        ..
      } => eprintln!("{} {tool_name}", if is_error { "ERR" } else { "OK" }),
      WorkerEvent::End {
        role, ok, message, ..
      } => eprintln!(
        "\n--- {} {} {} ---",
        role.as_str(),
        if ok { "done" } else { "failed" },
        message.unwrap_or_default()
      ),
    },
    RunEvent::Finished(state) => eprintln!(
      "\n[loops] finished: {:?} · {}",
      state.status, state.last_summary
    ),
  }
  Ok(())
}

fn sanitize_terminal_text(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut chars = text.chars().peekable();
  while let Some(ch) = chars.next() {
    if ch == '\u{1b}' {
      if chars.peek() == Some(&'[') {
        chars.next();
        for next in chars.by_ref() {
          if ('@'..='~').contains(&next) {
            break;
          }
        }
      } else {
        let _ = chars.next();
      }
      continue;
    }
    if ch == '\n' || ch == '\t' || !ch.is_control() {
      out.push(ch);
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::sanitize_terminal_text;

  #[test]
  fn sanitize_terminal_text_removes_control_sequences() {
    assert_eq!(
      sanitize_terminal_text("before\u{1b}[31mred\u{1b}[0m\0after"),
      "beforeredafter"
    );
  }

  #[test]
  fn sanitize_terminal_text_preserves_tabs_and_newlines() {
    assert_eq!(
      sanitize_terminal_text("first\tsecond\nthird"),
      "first\tsecond\nthird"
    );
  }
}
