use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use tokio::{
  fs::{self, OpenOptions},
  io::AsyncWriteExt,
  sync::{mpsc, Mutex},
};

use crate::model::{
  Discovery, ReconcileResult, RepositoryChange, RequirementCatalog, State, VerificationReport,
  WorkExecution, WorkLease, WorkUnit, WorkerEvent,
};
use crate::{
  evidence::{SemanticAssessmentReport, VerificationState},
  ids::{ArtifactId, CriterionId, ObligationId, RequirementId},
  verification::ProjectVerificationRun,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompletionGateOutcome {
  Satisfied,
  Unsatisfied,
  Unknown,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompletionGateItem {
  pub label: String,
  pub outcome: CompletionGateOutcome,
  pub detail: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompletionGate {
  pub revision: String,
  pub earned: bool,
  pub items: Vec<CompletionGateItem>,
  pub blockers: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceAcquisitionStage {
  GapDetected,
  Planned,
  Admitted,
  Rejected,
  IssuerSelected,
  Started,
  Completed,
  Failed,
  HumanActionRequired,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvidenceIssuer {
  TrustedVerifier { name: String },
  Falsifier { name: String },
  HumanAttestor { statement_hash: String },
  SourceInspection,
}
impl EvidenceIssuer {
  pub fn from_acquisition_kind(kind: &crate::proof::EvidenceAcquisitionKind) -> Self {
    match kind {
      crate::proof::EvidenceAcquisitionKind::TrustedVerifierCheck { name, .. } => {
        Self::TrustedVerifier { name: name.clone() }
      }
      crate::proof::EvidenceAcquisitionKind::FalsifierCheck { name, .. } => {
        Self::Falsifier { name: name.clone() }
      }
      crate::proof::EvidenceAcquisitionKind::HumanAttestation { statement_hash } => {
        Self::HumanAttestor {
          statement_hash: statement_hash.clone(),
        }
      }
      crate::proof::EvidenceAcquisitionKind::InspectSource { .. } => Self::SourceInspection,
    }
  }
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum RunEvent {
  State(State),
  Catalog(RequirementCatalog),
  Message(String),
  Worker(WorkerEvent),
  Reconcile(ReconcileResult),
  ReadyFrontier(Vec<WorkUnit>),
  LeaseIssued(WorkLease),
  WorkerStarted {
    worker_id: String,
    lease_id: String,
    work_unit_id: String,
  },
  CandidateProduced(WorkExecution),
  IntegrationStarted {
    work_unit_id: String,
    candidate_revision: String,
  },
  IntegrationAccepted {
    work_unit_id: String,
    revision: String,
  },
  IntegrationRejected {
    work_unit_id: String,
    reason: String,
  },
  DependencyDiscovered {
    lease_id: String,
    discovery: Discovery,
  },
  WorkspaceCreated {
    lease_id: String,
    path: PathBuf,
  },
  WorkspaceRemoved {
    lease_id: String,
    path: PathBuf,
  },
  Verification(VerificationReport),
  ProjectVerification(ProjectVerificationRun),
  SemanticAssessment(SemanticAssessmentReport),
  RepositoryChanges(Vec<RepositoryChange>),
  ArtifactReused {
    artifact_id: ArtifactId,
    from_revision: String,
    to_revision: String,
  },
  ArtifactBecameStale {
    artifact_id: ArtifactId,
    revision: String,
  },
  EvidenceAcquisition {
    stage: EvidenceAcquisitionStage,
    revision: String,
    issuer: EvidenceIssuer,
    obligation_ids: Vec<ObligationId>,
  },
  ArtifactIssued {
    artifact_id: ArtifactId,
    revision: String,
    obligation_ids: Vec<ObligationId>,
  },
  ObligationProofChanged {
    obligation_id: ObligationId,
    revision: String,
    previous: crate::proof::ProofState,
    current: crate::proof::ProofState,
  },
  CriterionVerificationChanged {
    criterion_id: CriterionId,
    previous: VerificationState,
    current: VerificationState,
  },
  RequirementVerificationChanged {
    requirement_id: RequirementId,
    previous: VerificationState,
    current: VerificationState,
  },
  CompletionGate(CompletionGate),
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

  pub async fn emit(&self, event: RunEvent) -> Result<()> {
    if let Some(logger) = &self.logger {
      logger.write_event(&event).await?;
    }
    if let Some(tx) = &self.tx {
      let _ = tx.send(event);
    }
    Ok(())
  }

  pub async fn worker(&self, event: WorkerEvent) -> Result<()> {
    if let Some(logger) = &self.logger {
      logger.write_worker(&event).await?;
    }
    if let Some(tx) = &self.tx {
      let _ = tx.send(RunEvent::Worker(event));
    }
    Ok(())
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
      RunEvent::Catalog(v) => serde_json::json!({"type":"catalog","value":v}),
      RunEvent::Message(v) => serde_json::json!({"type":"message","value":v}),
      RunEvent::Worker(v) => serde_json::json!({"type":"worker","value":v}),
      RunEvent::Reconcile(v) => serde_json::json!({"type":"reconcile","value":v}),
      RunEvent::ReadyFrontier(v) => serde_json::json!({"type":"ready_frontier","value":v}),
      RunEvent::LeaseIssued(v) => serde_json::json!({"type":"lease_issued","value":v}),
      RunEvent::WorkerStarted {
        worker_id,
        lease_id,
        work_unit_id,
      } => {
        serde_json::json!({"type":"worker_started","workerId":worker_id,"leaseId":lease_id,"workUnitId":work_unit_id})
      }
      RunEvent::CandidateProduced(v) => serde_json::json!({"type":"candidate_produced","value":v}),
      RunEvent::IntegrationStarted {
        work_unit_id,
        candidate_revision,
      } => {
        serde_json::json!({"type":"integration_started","workUnitId":work_unit_id,"candidateRevision":candidate_revision})
      }
      RunEvent::IntegrationAccepted {
        work_unit_id,
        revision,
      } => {
        serde_json::json!({"type":"integration_accepted","workUnitId":work_unit_id,"revision":revision})
      }
      RunEvent::IntegrationRejected {
        work_unit_id,
        reason,
      } => {
        serde_json::json!({"type":"integration_rejected","workUnitId":work_unit_id,"reason":reason})
      }
      RunEvent::DependencyDiscovered {
        lease_id,
        discovery,
      } => serde_json::json!({"type":"dependency_discovered","leaseId":lease_id,"value":discovery}),
      RunEvent::WorkspaceCreated { lease_id, path } => {
        serde_json::json!({"type":"workspace_created","leaseId":lease_id,"path":path})
      }
      RunEvent::WorkspaceRemoved { lease_id, path } => {
        serde_json::json!({"type":"workspace_removed","leaseId":lease_id,"path":path})
      }
      RunEvent::Verification(v) => serde_json::json!({"type":"verification","value":v}),
      RunEvent::ProjectVerification(v) => {
        serde_json::json!({"type":"project_verification","value":v})
      }
      RunEvent::SemanticAssessment(v) => {
        serde_json::json!({"type":"semantic_assessment","value":v})
      }
      RunEvent::RepositoryChanges(v) => {
        serde_json::json!({"type":"repository_changes","value":v})
      }
      RunEvent::ArtifactReused {
        artifact_id,
        from_revision,
        to_revision,
      } => serde_json::json!({
        "type":"artifact_reused",
        "artifactId":artifact_id,
        "fromRevision":from_revision,
        "toRevision":to_revision
      }),
      RunEvent::ArtifactBecameStale {
        artifact_id,
        revision,
      } => serde_json::json!({
        "type":"artifact_became_stale",
        "artifactId":artifact_id,
        "revision":revision
      }),
      RunEvent::CriterionVerificationChanged {
        criterion_id,
        previous,
        current,
      } => {
        serde_json::json!({"type":"criterion_verification_changed","criterionId":criterion_id,"previous":previous,"current":current})
      }
      RunEvent::RequirementVerificationChanged {
        requirement_id,
        previous,
        current,
      } => {
        serde_json::json!({"type":"requirement_verification_changed","requirementId":requirement_id,"previous":previous,"current":current})
      }
      RunEvent::CompletionGate(v) => {
        serde_json::json!({"type":"completion_gate","value":v})
      }
      RunEvent::EvidenceAcquisition {
        stage,
        revision,
        issuer,
        obligation_ids,
      } => serde_json::json!({
        "type":"evidence_acquisition",
        "stage":stage,
        "revision":revision,
        "issuer":issuer,
        "obligationIds":obligation_ids
      }),
      RunEvent::ArtifactIssued {
        artifact_id,
        revision,
        obligation_ids,
      } => serde_json::json!({
        "type":"artifact_issued",
        "artifactId":artifact_id,
        "revision":revision,
        "obligationIds":obligation_ids
      }),
      RunEvent::ObligationProofChanged {
        obligation_id,
        revision,
        previous,
        current,
      } => serde_json::json!({
        "type":"obligation_proof_changed",
        "obligationId":obligation_id,
        "revision":revision,
        "previous":previous,
        "current":current
      }),
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
    Start { role, .. } => format!("\n=== {}\n", role.as_str().to_uppercase(),),
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
  use super::*;

  #[tokio::test]
  async fn event_log_write_failure_is_returned_to_controller() {
    let directory = tempfile::tempdir().expect("temporary log directory");
    let read_only_path = directory.path().join("read-only-events.jsonl");
    std::fs::write(&read_only_path, "seed").expect("seed read-only log");
    let read_only = tokio::fs::File::open(read_only_path)
      .await
      .expect("open read-only event log");
    let worker_events = open_append(directory.path().join("worker.jsonl"))
      .await
      .expect("worker log");
    let transcript = open_append(directory.path().join("transcript.log"))
      .await
      .expect("transcript log");
    let logger = Arc::new(RunLogger {
      events: Mutex::new(read_only),
      worker_events: Mutex::new(worker_events),
      transcript: Mutex::new(transcript),
    });
    let sink = EventSink::new(None).with_logger(logger);

    let error = sink
      .emit(RunEvent::Message("required evidence".into()))
      .await
      .expect_err("log failure must propagate");

    assert!(!error.to_string().is_empty());
  }

  #[tokio::test]
  async fn acquisition_lifecycle_is_logged_without_executable_payloads() {
    let directory = tempfile::tempdir().expect("temporary log directory");
    let logger = Arc::new(
      RunLogger::create(directory.path().to_path_buf())
        .await
        .expect("run logger"),
    );
    let sink = EventSink::new(None).with_logger(logger);
    sink
      .emit(RunEvent::EvidenceAcquisition {
        stage: EvidenceAcquisitionStage::Started,
        revision: "abc123".into(),
        issuer: EvidenceIssuer::TrustedVerifier {
          name: "boundary".into(),
        },
        obligation_ids: vec![ObligationId::from("REQ-1/AC-1/VO-1")],
      })
      .await
      .expect("emit acquisition event");
    let text = tokio::fs::read_to_string(directory.path().join("events.jsonl"))
      .await
      .expect("read event log");
    let value: serde_json::Value = serde_json::from_str(text.trim()).expect("event JSON");
    assert_eq!(value["type"], "evidence_acquisition");
    assert_eq!(value["stage"], "started");
    assert_eq!(value["issuer"]["type"], "trusted_verifier");
    assert!(value.get("program").is_none());
  }
}
