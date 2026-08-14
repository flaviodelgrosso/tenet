use std::{
  io::{self, Write},
  path::PathBuf,
  sync::Arc,
};

use anyhow::Result;
use loops_domain::{
  events::EventSink,
  model::{RunStatus, State},
};
use loops_projection::{status_label, Activity, ActivityCategory, RunProjection};
use loops_runtime::{backend::AgentBackend, controller::Controller, store};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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
  let catalog = store::read_catalog(&cwd)
    .await?
    .map_or_else(Vec::new, |catalog| catalog.requirements);
  let mut presentation = RunProjection::new(initial, catalog);
  presentation.begin_run();
  let mut console = ConsoleRenderer::new(options);
  console.activity(
    presentation
      .activities()
      .back()
      .expect("run start activity"),
  )?;

  let (tx, mut rx) = mpsc::unbounded_channel();
  let controller = Controller::new(cwd, backend, EventSink::new(Some(tx)));
  let cancel = CancellationToken::new();
  let task_cancel = cancel.clone();
  let mut task = tokio::spawn(async move { controller.run(task_cancel).await });
  let mut events_closed = false;
  loop {
    tokio::select! {
      event = rx.recv(), if !events_closed => match event {
        Some(event) => {
          let previous = presentation.activities().len();
          presentation.apply(event);
          for activity in presentation.activities().iter().skip(previous) { console.activity(activity)?; }
        }
        None => events_closed = true,
      },
      result = &mut task => {
        let state = result??;
        console.summary(&presentation, &state)?;
        return Ok(state);
      },
      _ = tokio::signal::ctrl_c(), if !cancel.is_cancelled() => {
        console.line("STOP", "Cancellation requested · stopping the active worker gracefully")?;
        cancel.cancel();
      }
    }
  }
}

struct ConsoleRenderer {
  options: ConsoleOptions,
  stdout: io::Stdout,
}

impl ConsoleRenderer {
  fn new(options: ConsoleOptions) -> Self {
    Self {
      options,
      stdout: io::stdout(),
    }
  }

  fn line(&mut self, label: &str, message: &str) -> Result<()> {
    writeln!(
      self.stdout,
      "{}  {:<10} {}",
      chrono::Local::now().format("%H:%M:%S"),
      label,
      message
    )?;
    self.stdout.flush()?;
    Ok(())
  }

  fn activity(&mut self, item: &Activity) -> Result<()> {
    if !should_render(self.options, item) {
      return Ok(());
    }
    let at = timestamp(&item.at);
    let (label, text) = if item.title == "CHANGES" {
      ("CHANGES", item.summary.as_str())
    } else if item.title == "RECONCILE" {
      ("WORK", item.summary.as_str())
    } else if item.title.starts_with("CHECKS") {
      ("VERIFY", item.summary.as_str())
    } else if item.title.contains("TOOL") {
      ("TOOL", item.summary.as_str())
    } else {
      (item.title.as_str(), item.summary.as_str())
    };
    writeln!(self.stdout, "{at}  {label:<10} {text}")?;

    if item.title == "CHANGES" {
      for path in item.detail.lines().take(8) {
        writeln!(self.stdout, "                    {path}")?;
      }
      let extra = item.detail.lines().count().saturating_sub(8);
      if extra > 0 {
        writeln!(self.stdout, "                    … {extra} more")?;
      }
    }

    if item.title.starts_with("CHECKS") {
      for line in item
        .detail
        .lines()
        .filter(|line| {
          line.starts_with("PASS ") || line.starts_with("FAIL ") || line.starts_with("TIMEOUT ")
        })
        .take(12)
      {
        writeln!(self.stdout, "                    {line}")?;
      }
    }

    if matches!(item.category, ActivityCategory::Error) {
      if let Some(detail) = item.detail.lines().find(|line| !line.trim().is_empty()) {
        writeln!(self.stdout, "                    {detail}")?;
      }
    }

    if self.options.verbose && item.category == ActivityCategory::Worker && !item.detail.is_empty()
    {
      writeln!(
        self.stdout,
        "                    {}",
        item.detail.replace('\n', "\n                    ")
      )?;
    }
    self.stdout.flush()?;
    Ok(())
  }

  fn summary(&mut self, presentation: &RunProjection, state: &State) -> Result<()> {
    let elapsed = format_duration(presentation.elapsed_seconds());
    let state_label = status_label(&state.status);
    writeln!(
      self.stdout,
      "\n{}  {state_label:<10} {elapsed}",
      if state.status == RunStatus::Done {
        "✓"
      } else {
        "✕"
      }
    )?;

    writeln!(
      self.stdout,
      "  Requirements   {}/{} satisfied",
      state.requirement_counts.satisfied, state.requirement_counts.total
    )?;

    writeln!(
      self.stdout,
      "  Work units     {} completed",
      state.completed_work_units.len()
    )?;

    writeln!(self.stdout, "  Cycle          {}", state.cycle)?;
    for lease in state.active_leases.values() {
      writeln!(
        self.stdout,
        "  Active work    {} · {} ({})",
        lease.work_unit.id, lease.work_unit.title, lease.worker_id
      )?;
    }

    if let Some(reason) = state.blocked_reason.as_ref().or(state.last_error.as_ref()) {
      writeln!(self.stdout, "\n  Reason\n  {reason}")?;
    }

    if !presentation.checks().is_empty() {
      writeln!(
        self.stdout,
        "  Checks         {}",
        if presentation
          .checks()
          .last()
          .is_some_and(|report| report.passed)
        {
          "PASS"
        } else {
          "FAIL"
        }
      )?;
    }

    if matches!(
      state.status,
      RunStatus::Blocked | RunStatus::Failed | RunStatus::Stopped
    ) {
      writeln!(
        self.stdout,
        "\n  State is preserved. Run `loops resume --headless` after addressing the blocker."
      )?;
    }
    self.stdout.flush()?;
    Ok(())
  }
}

fn should_render(options: ConsoleOptions, item: &Activity) -> bool {
  if options.quiet {
    return matches!(
      item.category,
      ActivityCategory::Error | ActivityCategory::Check
    );
  }

  if !options.verbose
    && item.category == ActivityCategory::Worker
    && item.title.ends_with("TOOL COMPLETE")
  {
    return false;
  }

  options.verbose
    || item.category != ActivityCategory::Worker
    || item.title.contains("TOOL")
    || item.title.contains("STARTED")
    || item.title.contains("COMPLETE")
    || item.title.contains("FAILED")
}

fn timestamp(value: &str) -> &str {
  value
    .rsplit('T')
    .next()
    .unwrap_or(value)
    .split('+')
    .next()
    .unwrap_or(value)
    .split('.')
    .next()
    .unwrap_or(value)
}
fn format_duration(seconds: u64) -> String {
  if seconds >= 60 {
    format!("{}m {:02}s", seconds / 60, seconds % 60)
  } else {
    format!("{seconds}s")
  }
}

#[cfg(test)]
mod tests {
  use super::{format_duration, should_render, timestamp, ConsoleOptions};
  use loops_projection::{Activity, ActivityCategory};
  #[test]
  fn formats_log_time_and_duration() {
    assert_eq!(timestamp("2026-08-13T16:42:08.123Z"), "16:42:08");
    assert_eq!(format_duration(62), "1m 02s");
  }

  #[test]
  fn quiet_and_default_filter_worker_narrative() {
    let narrative = Activity {
      at: "10:00:00".into(),
      category: ActivityCategory::Worker,
      title: "IMPLEMENT".into(),
      summary: "thinking".into(),
      detail: String::new(),
    };
    let tool = Activity {
      title: "IMPLEMENT · TOOL".into(),
      ..narrative.clone()
    };
    assert!(!should_render(
      ConsoleOptions {
        quiet: false,
        verbose: false
      },
      &narrative
    ));
    assert!(should_render(
      ConsoleOptions {
        quiet: false,
        verbose: false
      },
      &tool
    ));
    assert!(should_render(
      ConsoleOptions {
        quiet: false,
        verbose: true
      },
      &narrative
    ));
    assert!(!should_render(
      ConsoleOptions {
        quiet: true,
        verbose: false
      },
      &tool
    ));
  }

  #[test]
  fn default_renders_tool_lifecycle_once_and_keeps_failures() {
    let options = ConsoleOptions {
      quiet: false,
      verbose: false,
    };
    let activity = |title: &str, category| Activity {
      at: "10:00:00".into(),
      category,
      title: title.into(),
      summary: "Inspecting repository root".into(),
      detail: String::new(),
    };
    let rendered = [
      activity("IMPLEMENT · TOOL", ActivityCategory::Worker),
      activity("IMPLEMENT · TOOL COMPLETE", ActivityCategory::Worker),
      activity("IMPLEMENT · TOOL FAILED", ActivityCategory::Error),
    ]
    .map(|item| should_render(options, &item));

    assert_eq!(rendered, [true, false, true]);
  }
}
