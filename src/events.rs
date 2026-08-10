use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use tokio::{
  fs::{self, OpenOptions},
  io::AsyncWriteExt,
  sync::{mpsc, Mutex},
};

use crate::model::{ReconcileResult, State, VerificationReport, WorkerEvent};

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum RunEvent {
  State(State),
  Message(String),
  Worker(WorkerEvent),
  Reconcile(ReconcileResult),
  Verification(VerificationReport),
  Finished(State),
}

#[derive(Clone)]
pub struct EventSink {
  tx: Option<mpsc::UnboundedSender<RunEvent>>,
  logger: Option<Arc<RunLogger>>,
}

impl EventSink {
  pub fn new(tx: Option<mpsc::UnboundedSender<RunEvent>>) -> Self {
    Self { tx, logger: None }
  }

  pub fn with_logger(mut self, logger: Arc<RunLogger>) -> Self {
    self.logger = Some(logger);
    self
  }

  pub async fn emit(&self, event: RunEvent) {
    if let Some(logger) = &self.logger {
      let _ = logger.write_event(&event).await;
    }
    if let Some(tx) = &self.tx {
      let _ = tx.send(event);
    }
  }

  pub async fn worker(&self, event: WorkerEvent) {
    if let Some(logger) = &self.logger {
      let _ = logger.write_worker(&event).await;
    }
    if let Some(tx) = &self.tx {
      let _ = tx.send(RunEvent::Worker(event));
    }
  }
}

pub struct RunLogger {
  events: Mutex<tokio::fs::File>,
  worker_events: Mutex<tokio::fs::File>,
  transcript: Mutex<tokio::fs::File>,
}

impl RunLogger {
  pub async fn create(dir: PathBuf) -> Result<Self> {
    fs::create_dir_all(&dir).await?;
    Ok(Self {
      events: Mutex::new(open_append(dir.join("events.jsonl")).await?),
      worker_events: Mutex::new(open_append(dir.join("worker-events.jsonl")).await?),
      transcript: Mutex::new(open_append(dir.join("transcript.log")).await?),
    })
  }

  async fn write_event(&self, event: &RunEvent) -> Result<()> {
    let value = match event {
      RunEvent::State(v) => serde_json::json!({"type":"state","value":v}),
      RunEvent::Message(v) => serde_json::json!({"type":"message","value":v}),
      RunEvent::Worker(v) => serde_json::json!({"type":"worker","value":v}),
      RunEvent::Reconcile(v) => serde_json::json!({"type":"reconcile","value":v}),
      RunEvent::Verification(v) => serde_json::json!({"type":"verification","value":v}),
      RunEvent::Finished(v) => serde_json::json!({"type":"finished","value":v}),
    };
    let mut file = self.events.lock().await;
    file
      .write_all(serde_json::to_string(&value)?.as_bytes())
      .await?;
    file.write_all(b"\n").await?;
    file.flush().await?;
    Ok(())
  }

  async fn write_worker(&self, event: &WorkerEvent) -> Result<()> {
    {
      let mut file = self.worker_events.lock().await;
      file
        .write_all(serde_json::to_string(event)?.as_bytes())
        .await?;
      file.write_all(b"\n").await?;
      file.flush().await?;
    }
    let mut transcript = self.transcript.lock().await;
    transcript
      .write_all(render_worker(event).as_bytes())
      .await?;
    transcript.flush().await?;
    Ok(())
  }
}

async fn open_append(path: PathBuf) -> Result<tokio::fs::File> {
  Ok(
    OpenOptions::new()
      .create(true)
      .append(true)
      .open(path)
      .await?,
  )
}

fn render_worker(event: &WorkerEvent) -> String {
  use WorkerEvent::*;
  match event {
    Start { role, skills, .. } => format!(
      "\n=== {} · skills: {} ===\n",
      role.as_str().to_uppercase(),
      skills.join(", ")
    ),
    Text { delta, .. } => delta.clone(),
    ToolStart {
      tool_name, args, ..
    } => format!("\n> {tool_name} {}\n", compact_json(args)),
    ToolEnd {
      tool_name,
      is_error,
      output,
      ..
    } => {
      let mark = if *is_error { "ERR" } else { "OK" };
      format!(
        "[{mark}] {tool_name}{}\n",
        output
          .as_deref()
          .map(|v| format!("\n{v}"))
          .unwrap_or_default()
      )
    }
    End {
      role, ok, message, ..
    } => format!(
      "\n--- {} {}{} ---\n",
      role.as_str(),
      if *ok { "done" } else { "failed" },
      message
        .as_deref()
        .map(|v| format!(": {v}"))
        .unwrap_or_default()
    ),
  }
}

fn compact_json(value: &serde_json::Value) -> String {
  let text = serde_json::to_string(value).unwrap_or_else(|_| "{}".into());
  if text.len() > 240 {
    let end = utf8_floor(&text, 240);
    format!("{}…", &text[..end])
  } else {
    text
  }
}

fn utf8_floor(text: &str, max_bytes: usize) -> usize {
  let mut end = max_bytes.min(text.len());
  while end > 0 && !text.is_char_boundary(end) {
    end -= 1;
  }
  end
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use tempfile::tempdir;

  use super::{EventSink, RunLogger};
  use crate::model::{WorkerEvent, WorkerRole};

  #[tokio::test]
  async fn worker_metadata_records_active_skills() {
    let run = tempdir().unwrap();
    let logger = Arc::new(RunLogger::create(run.path().to_path_buf()).await.unwrap());
    EventSink::new(None)
      .with_logger(logger)
      .worker(WorkerEvent::Start {
        role: WorkerRole::Implement,
        at: "now".into(),
        skills: vec!["implementation".into(), "rust".into()],
      })
      .await;

    let events = tokio::fs::read_to_string(run.path().join("worker-events.jsonl"))
      .await
      .unwrap();
    assert!(events.contains(r#""skills":["implementation","rust"]"#));
  }
}
