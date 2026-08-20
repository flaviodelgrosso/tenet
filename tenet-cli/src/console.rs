use std::{
  collections::{BTreeMap, BTreeSet},
  io::{self, IsTerminal, Write},
  time::Instant,
};

use anyhow::Result;
use colored::Colorize;
use tenet_domain::{
  events::{CompletionGate, CompletionGateOutcome, RunEvent},
  evidence::{
    Evidence, EvidenceResult, EvidenceSource, ObligationAssessment, SemanticAssessmentReport,
  },
  model::{
    Phase, RepositoryChange, RunStatus, State, WorkExecution, WorkLease, WorkUnit, WorkerEvent,
    WorkerRole,
  },
  verification::{ProjectVerificationRun, VerificationReport},
};

const MAX_CHANGE_PATHS: usize = 8;
const MAX_DEFAULT_OUTPUT_CHARS: usize = 480;
const MAX_VERBOSE_OUTPUT_CHARS: usize = 2_048;

fn failure_preview(report: &VerificationReport) -> String {
  report
    .commands
    .iter()
    .find(|item| item.exit_code != Some(0) || item.timed_out)
    .map(|item| {
      let output = if item.stderr.trim().is_empty() {
        &item.stdout
      } else {
        &item.stderr
      };
      format!(
        "{} · {}",
        item.command,
        compact(&sanitize_terminal_text(output), 240)
      )
    })
    .unwrap_or_else(|| "verification failed".into())
}

fn sanitize_terminal_text(text: &str) -> String {
  let mut output = String::with_capacity(text.len());
  let mut characters = text.chars().peekable();
  while let Some(character) = characters.next() {
    if character == '\u{1b}' {
      if characters.peek() == Some(&'[') {
        characters.next();
        for next in characters.by_ref() {
          if ('@'..='~').contains(&next) {
            break;
          }
        }
      } else {
        let _ = characters.next();
      }
      continue;
    }
    if character == '\n' || character == '\t' || !character.is_control() {
      output.push(character);
    }
  }
  output
}

fn compact(text: &str, max: usize) -> String {
  if text.len() <= max {
    return text.into();
  }
  let mut end = max;
  while !text.is_char_boundary(end) {
    end -= 1;
  }
  format!("{}…", &text[..end])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InformationMode {
  Quiet,
  Default,
  Verbose,
}

impl InformationMode {
  pub(crate) fn from_flags(quiet: bool, verbose: bool) -> Self {
    if verbose {
      Self::Verbose
    } else if quiet {
      Self::Quiet
    } else {
      Self::Default
    }
  }

  pub(crate) fn includes_diagnostics(self) -> bool {
    self == Self::Verbose
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
  Active,
  Success,
  Warning,
  Failure,
  Milestone,
  Secondary,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SemanticStyle {
  color: bool,
}

impl SemanticStyle {
  pub(crate) fn auto(stdout_is_terminal: bool) -> Self {
    Self {
      color: stdout_is_terminal && std::env::var_os("NO_COLOR").is_none(),
    }
  }

  #[cfg(test)]
  pub(crate) const fn plain() -> Self {
    Self { color: false }
  }

  pub(crate) fn marker(self, tone: Tone) -> String {
    let marker = match tone {
      Tone::Active => "*",
      Tone::Success => "+",
      Tone::Warning => "!",
      Tone::Failure => "x",
      Tone::Milestone => ">",
      Tone::Secondary => "-",
    };
    self.paint(marker, tone, true)
  }

  pub(crate) fn label(self, label: &str, tone: Tone) -> String {
    self.paint(label, tone, true)
  }

  pub(crate) fn metadata(self, text: &str) -> String {
    self.paint(text, Tone::Secondary, false)
  }

  fn paint(self, text: &str, tone: Tone, bold: bool) -> String {
    if !self.color {
      return text.to_owned();
    }
    let styled = match tone {
      Tone::Active => text.cyan(),
      Tone::Success => text.green(),
      Tone::Warning => text.yellow(),
      Tone::Failure => text.red(),
      Tone::Milestone => text.blue(),
      Tone::Secondary => text.dimmed(),
    };
    if bold {
      styled.bold().to_string()
    } else {
      styled.to_string()
    }
  }
}

#[derive(Debug, Clone)]
pub(crate) struct RunHeader {
  pub repository: Option<String>,
  pub revision: String,
  pub specification: String,
  pub agent: Option<String>,
  pub requirements: Option<usize>,
  pub verification_checks: usize,
  pub max_cycles: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgressSnapshot {
  pub requirements_verified: usize,
  pub requirements_total: usize,
  pub semantic_satisfied: usize,
  pub semantic_total: usize,
  pub work_completed: usize,
  pub work_total: usize,
  pub cycle: u32,
}

#[derive(Debug, Clone)]
pub(crate) enum ConsoleEvent {
  CycleStarted(u32),
  Milestone {
    at: String,
    label: &'static str,
    summary: String,
  },
  WorkStarted {
    at: String,
    work: Box<WorkUnit>,
    worker_id: Option<String>,
    lease_id: Option<String>,
  },
  RepairStarted {
    at: String,
    work_unit_id: String,
    attempt: u32,
    max_attempts: u32,
    reason: String,
  },
  CandidateCreated {
    at: String,
    execution: Box<WorkExecution>,
  },
  Changes {
    at: String,
    changes: Vec<RepositoryChange>,
  },
  Verification {
    at: String,
    report: ProjectVerificationRun,
    next_action: String,
  },
  AdvisoryVerification {
    at: String,
    report: VerificationReport,
  },
  IntegrationStarted {
    at: String,
    work_unit_id: String,
    candidate_revision: String,
  },
  WorkIntegrated {
    at: String,
    work_unit_id: String,
    title: Option<String>,
    revision: String,
    changed_paths: usize,
    elapsed_seconds: Option<u64>,
  },
  IntegrationRejected {
    at: String,
    work_unit_id: String,
    reason: String,
  },
  SemanticAssessment {
    at: String,
    report: SemanticAssessmentReport,
  },
  SemanticEvidence {
    at: String,
    evidence: Evidence,
  },
  StaleEvidence {
    at: String,
    evidence_id: String,
    revision: String,
  },
  Contradiction {
    at: String,
    obligation_id: String,
    evidence_count: usize,
  },
  Progress(ProgressSnapshot),
  CompletionGate(CompletionGate),
  Error {
    at: String,
    label: &'static str,
    summary: String,
    detail: Option<String>,
  },
  Diagnostic {
    at: String,
    label: String,
    summary: String,
    detail: String,
  },
}

impl ConsoleEvent {
  fn is_outcome_changing(&self) -> bool {
    matches!(
      self,
      Self::WorkStarted { .. }
        | Self::RepairStarted { .. }
        | Self::WorkIntegrated { .. }
        | Self::IntegrationRejected { .. }
        | Self::Verification { .. }
        | Self::AdvisoryVerification { .. }
        | Self::SemanticAssessment { .. }
        | Self::SemanticEvidence { .. }
        | Self::StaleEvidence { .. }
        | Self::Contradiction { .. }
        | Self::CompletionGate(_)
        | Self::Error { .. }
    )
  }
}

pub(crate) struct ConsolePresenter {
  max_repair_attempts: u32,
  cycle: u32,
  leases: BTreeMap<String, WorkLease>,
  work: BTreeMap<String, WorkUnit>,
  worker_text: BTreeMap<String, String>,
  work_started: BTreeMap<String, Instant>,
  candidates: BTreeMap<String, WorkExecution>,
  current_integration: Option<String>,
  seen_work: BTreeSet<String>,
  shown_semantic_findings: BTreeSet<String>,
  last_repair: Option<(String, u32)>,
  last_progress: Option<ProgressSnapshot>,
  last_failure: Option<String>,
}

impl ConsolePresenter {
  pub(crate) fn new(max_repair_attempts: u32) -> Self {
    Self {
      max_repair_attempts,
      cycle: 0,
      leases: BTreeMap::new(),
      work: BTreeMap::new(),
      work_started: BTreeMap::new(),
      worker_text: BTreeMap::new(),
      candidates: BTreeMap::new(),
      current_integration: None,
      seen_work: BTreeSet::new(),
      shown_semantic_findings: BTreeSet::new(),
      last_repair: None,
      last_progress: None,
      last_failure: None,
    }
  }

  pub(crate) fn present(&mut self, event: &RunEvent) -> Vec<ConsoleEvent> {
    match event {
      RunEvent::State(state) => self.present_state(state),
      RunEvent::Reconcile(result) => vec![ConsoleEvent::Milestone {
        at: now(),
        label: "RECONCILE",
        summary: if result.work_units.is_empty() {
          result.summary.clone()
        } else {
          format!(
            "{} work unit(s) ready · {}",
            result.work_units.len(),
            result.summary
          )
        },
      }],
      RunEvent::ReadyFrontier(units) => {
        for unit in units {
          self.seen_work.insert(unit.id.clone());
          self.work.insert(unit.id.clone(), unit.clone());
        }
        vec![ConsoleEvent::Milestone {
          at: now(),
          label: "PLAN",
          summary: format!("{} work unit(s) ready", units.len()),
        }]
      }
      RunEvent::LeaseIssued(lease) => {
        self.seen_work.insert(lease.work_unit.id.clone());
        self
          .work
          .insert(lease.work_unit.id.clone(), lease.work_unit.clone());
        self.leases.insert(lease.id.clone(), lease.clone());
        Vec::new()
      }
      RunEvent::WorkerStarted {
        worker_id,
        lease_id,
        work_unit_id,
      } => {
        let Some(lease) = self.leases.get(lease_id) else {
          return vec![ConsoleEvent::Error {
            at: now(),
            label: "WORK",
            summary: format!("{work_unit_id} started without its lease context"),
            detail: Some(format!("worker {worker_id} · lease {lease_id}")),
          }];
        };
        self
          .work_started
          .insert(work_unit_id.clone(), Instant::now());
        vec![ConsoleEvent::WorkStarted {
          at: now(),
          work: Box::new(lease.work_unit.clone()),
          worker_id: Some(worker_id.clone()),
          lease_id: Some(lease_id.clone()),
        }]
      }
      RunEvent::CandidateProduced(execution) => {
        self
          .candidates
          .insert(execution.lease.work_unit.id.clone(), execution.clone());
        vec![ConsoleEvent::CandidateCreated {
          at: execution.verification.finished_at.to_rfc3339(),
          execution: Box::new(execution.clone()),
        }]
      }
      RunEvent::RepositoryChanges(changes) => vec![ConsoleEvent::Changes {
        at: now(),
        changes: changes.clone(),
      }],
      RunEvent::Verification(report) => {
        if !report.passed {
          self.last_failure = Some(failure_preview(report));
        }
        vec![ConsoleEvent::AdvisoryVerification {
          at: report.finished_at.to_rfc3339(),
          report: report.clone(),
        }]
      }
      RunEvent::ProjectVerification(report) => {
        if !report.passed {
          self.last_failure = report
            .checks
            .iter()
            .find(|check| check.result.exit_code != Some(0) || check.result.timed_out)
            .map(|check| format!("{} failed", check.name));
        }
        vec![ConsoleEvent::Verification {
          at: report.finished_at.to_rfc3339(),
          report: report.clone(),
          next_action: self.current_integration.as_ref().map_or_else(
            || "reconciliation or blocked completion".into(),
            |id| format!("repair {id}"),
          ),
        }]
      }
      RunEvent::IntegrationStarted {
        work_unit_id,
        candidate_revision,
      } => {
        self.current_integration = Some(work_unit_id.clone());
        vec![ConsoleEvent::IntegrationStarted {
          at: now(),
          work_unit_id: work_unit_id.clone(),
          candidate_revision: candidate_revision.clone(),
        }]
      }
      RunEvent::IntegrationAccepted {
        work_unit_id,
        revision,
      } => {
        let changed_paths = self
          .candidates
          .get(work_unit_id)
          .map_or(0, |candidate| candidate.changed_paths.len());
        let elapsed_seconds = self
          .work_started
          .remove(work_unit_id)
          .map(|started| started.elapsed().as_secs());
        self.current_integration = None;
        vec![ConsoleEvent::WorkIntegrated {
          at: now(),
          work_unit_id: work_unit_id.clone(),
          title: self.work.get(work_unit_id).map(|work| work.title.clone()),
          revision: revision.clone(),
          changed_paths,
          elapsed_seconds,
        }]
      }
      RunEvent::IntegrationRejected {
        work_unit_id,
        reason,
      } => {
        self.current_integration = None;
        self.last_failure = Some(reason.clone());
        vec![ConsoleEvent::IntegrationRejected {
          at: now(),
          work_unit_id: work_unit_id.clone(),
          reason: reason.clone(),
        }]
      }
      RunEvent::SemanticAssessment(report) => {
        self.shown_semantic_findings.extend(
          report
            .assessments
            .iter()
            .filter(|item| !matches!(item.assessment, ObligationAssessment::Satisfied { .. }))
            .map(|item| item.obligation_id.to_string()),
        );
        vec![ConsoleEvent::SemanticAssessment {
          at: now(),
          report: report.clone(),
        }]
      }
      RunEvent::EvidenceEstablished(evidence) => vec![ConsoleEvent::Diagnostic {
        at: evidence.observed_at.to_rfc3339(),
        label: "EVIDENCE".into(),
        summary: format!("{} established", evidence.obligation_id),
        detail: format!(
          "revision {} · {:?}\n{}\n{}",
          evidence.revision,
          evidence.source,
          evidence.rationale,
          evidence.evidence_refs.join("\n")
        ),
      }],
      RunEvent::EvidenceFailed(evidence)
        if evidence.source == EvidenceSource::SemanticAssessment
          && self
            .shown_semantic_findings
            .insert(evidence.obligation_id.to_string()) =>
      {
        vec![ConsoleEvent::SemanticEvidence {
          at: evidence.observed_at.to_rfc3339(),
          evidence: evidence.clone(),
        }]
      }
      RunEvent::EvidenceFailed(evidence) => vec![ConsoleEvent::Diagnostic {
        at: evidence.observed_at.to_rfc3339(),
        label: "EVIDENCE".into(),
        summary: format!("{} did not establish", evidence.obligation_id),
        detail: evidence.rationale.clone(),
      }],
      RunEvent::EvidenceInvalidated {
        evidence_id,
        revision,
      } => vec![ConsoleEvent::StaleEvidence {
        at: now(),
        evidence_id: evidence_id.to_string(),
        revision: revision.clone(),
      }],
      RunEvent::EvidenceContradiction {
        obligation_id,
        evidence_ids,
      } => vec![ConsoleEvent::Contradiction {
        at: now(),
        obligation_id: obligation_id.to_string(),
        evidence_count: evidence_ids.len(),
      }],
      RunEvent::CompletionGate(gate) => vec![ConsoleEvent::CompletionGate(gate.clone())],
      RunEvent::Message(message) => vec![ConsoleEvent::Milestone {
        at: now(),
        label: "CONTROLLER",
        summary: message.clone(),
      }],
      RunEvent::Worker(worker) => self.present_worker(worker),
      RunEvent::WorkspaceCreated { lease_id, path } => vec![ConsoleEvent::Diagnostic {
        at: now(),
        label: "WORKSPACE".into(),
        summary: format!("created for {lease_id}"),
        detail: path.display().to_string(),
      }],
      RunEvent::WorkspaceRemoved { lease_id, path } => vec![ConsoleEvent::Diagnostic {
        at: now(),
        label: "WORKSPACE".into(),
        summary: format!("removed for {lease_id}"),
        detail: path.display().to_string(),
      }],
      RunEvent::DependencyDiscovered {
        lease_id,
        discovery,
      } => vec![ConsoleEvent::Diagnostic {
        at: now(),
        label: "DISCOVERY".into(),
        summary: lease_id.clone(),
        detail: format!("{discovery:?}"),
      }],
      RunEvent::CriterionVerificationChanged {
        criterion_id,
        previous,
        current,
      } => vec![ConsoleEvent::Diagnostic {
        at: now(),
        label: "EVIDENCE".into(),
        summary: criterion_id.to_string(),
        detail: format!("{previous:?} -> {current:?}"),
      }],
      RunEvent::RequirementVerificationChanged {
        requirement_id,
        previous,
        current,
      } => vec![ConsoleEvent::Diagnostic {
        at: now(),
        label: "EVIDENCE".into(),
        summary: requirement_id.to_string(),
        detail: format!("{previous:?} -> {current:?}"),
      }],
      RunEvent::Catalog(_) | RunEvent::Finished(_) => Vec::new(),
    }
  }

  fn present_state(&mut self, state: &State) -> Vec<ConsoleEvent> {
    let mut events = Vec::new();
    if state.cycle > self.cycle {
      self.cycle = state.cycle;
      events.push(ConsoleEvent::CycleStarted(state.cycle));
    }
    if let Some(repair) = &state.current_repair {
      let identity = (repair.work_unit_id.clone(), repair.attempt);
      if self.last_repair.as_ref() != Some(&identity) {
        self.last_repair = Some(identity);
        events.push(ConsoleEvent::RepairStarted {
          at: state.updated_at.clone(),
          work_unit_id: repair.work_unit_id.clone(),
          attempt: repair.attempt,
          max_attempts: self.max_repair_attempts,
          reason: self
            .last_failure
            .clone()
            .unwrap_or_else(|| state.last_summary.clone()),
        });
      }
    }
    if matches!(
      state.phase,
      Phase::Reconciling | Phase::Integrating | Phase::Assessing | Phase::Complete
    ) {
      let progress = ProgressSnapshot {
        requirements_verified: state.requirement_counts.verified,
        requirements_total: state.requirement_counts.total,
        semantic_satisfied: state.verification_layers.semantic_satisfied,
        semantic_total: state.verification_layers.semantic_obligations_total,
        work_completed: state.completed_work_units.len(),
        work_total: self.seen_work.len().max(state.completed_work_units.len()),
        cycle: state.cycle,
      };
      if self.last_progress.as_ref() != Some(&progress)
        && (progress.requirements_total > 0 || progress.work_total > 0)
      {
        self.last_progress = Some(progress.clone());
        events.push(ConsoleEvent::Progress(progress));
      }
    }
    if matches!(state.status, RunStatus::Blocked | RunStatus::Failed) {
      let reason = state
        .blocked_reason
        .as_ref()
        .or(state.last_error.as_ref())
        .cloned()
        .unwrap_or_else(|| state.last_summary.clone());
      events.push(ConsoleEvent::Error {
        at: state.updated_at.clone(),
        label: if state.status == RunStatus::Blocked {
          "BLOCKED"
        } else {
          "FAILED"
        },
        summary: reason,
        detail: None,
      });
    }
    events
  }

  fn present_worker(&mut self, worker: &WorkerEvent) -> Vec<ConsoleEvent> {
    match worker {
      WorkerEvent::ToolEnd {
        at,
        role,
        worker_id,
        tool_name,
        is_error: true,
        output,
        ..
      } => {
        let mut events = self.flush_worker_text(worker_id, *role, at);
        events.push(ConsoleEvent::Error {
          at: at.clone(),
          label: "TOOL",
          summary: format!("{} · {tool_name} failed", role.as_str()),
          detail: output.clone(),
        });
        events
      }
      WorkerEvent::End {
        at,
        role,
        worker_id,
        ok: false,
        message,
        ..
      } => {
        let mut events = self.flush_worker_text(worker_id, *role, at);
        events.push(ConsoleEvent::Error {
          at: at.clone(),
          label: "WORKER",
          summary: format!("{} failed", role.as_str()),
          detail: message.clone(),
        });
        events
      }
      WorkerEvent::Start {
        at,
        role,
        worker_id,
        lease_id,
        work_unit_id,
      } => {
        self.worker_text.remove(worker_id);
        vec![ConsoleEvent::Diagnostic {
          at: at.clone(),
          label: role.as_str().to_ascii_uppercase(),
          summary: format!("worker {worker_id} started"),
          detail: format!(
            "lease {} · work unit {}",
            lease_id.as_deref().unwrap_or("none"),
            work_unit_id.as_deref().unwrap_or("none")
          ),
        }]
      }
      WorkerEvent::Text {
        at,
        role,
        worker_id,
        delta,
        ..
      } => self.buffer_worker_text(worker_id, *role, at, delta),
      WorkerEvent::ToolStart {
        at,
        role,
        worker_id,
        tool_name,
        args,
        ..
      } => {
        let mut events = self.flush_worker_text(worker_id, *role, at);
        events.push(ConsoleEvent::Diagnostic {
          at: at.clone(),
          label: "TOOL".into(),
          summary: format!("{} · {tool_name}", role.as_str()),
          detail: serde_json::to_string(args).unwrap_or_else(|_| "{}".into()),
        });
        events
      }
      WorkerEvent::ToolEnd {
        at,
        role,
        worker_id,
        tool_name,
        output,
        ..
      } => {
        let mut events = self.flush_worker_text(worker_id, *role, at);
        events.push(ConsoleEvent::Diagnostic {
          at: at.clone(),
          label: "TOOL".into(),
          summary: format!("{} · {tool_name} complete", role.as_str()),
          detail: output.clone().unwrap_or_default(),
        });
        events
      }
      WorkerEvent::End {
        at,
        role,
        worker_id,
        message,
        ..
      } => {
        let mut events = self.flush_worker_text(worker_id, *role, at);
        events.push(ConsoleEvent::Diagnostic {
          at: at.clone(),
          label: role.as_str().to_ascii_uppercase(),
          summary: "worker complete".into(),
          detail: message.clone().unwrap_or_default(),
        });
        events
      }
    }
  }

  fn buffer_worker_text(
    &mut self,
    worker_id: &str,
    role: WorkerRole,
    at: &str,
    delta: &str,
  ) -> Vec<ConsoleEvent> {
    let buffer = self.worker_text.entry(worker_id.into()).or_default();
    buffer.push_str(delta);
    let Some(last_newline) = buffer.rfind('\n') else {
      return Vec::new();
    };
    let remainder = buffer.split_off(last_newline + 1);
    let complete = std::mem::replace(buffer, remainder);
    worker_text_events(role, worker_id, at, complete.split_terminator('\n'))
  }
  fn flush_worker_text(
    &mut self,
    worker_id: &str,
    role: WorkerRole,
    at: &str,
  ) -> Vec<ConsoleEvent> {
    self
      .worker_text
      .remove(worker_id)
      .map_or_else(Vec::new, |text| {
        worker_text_events(role, worker_id, at, std::iter::once(text.as_str()))
      })
  }
}

fn worker_text_events<'a>(
  role: WorkerRole,
  worker_id: &str,
  at: &str,
  lines: impl Iterator<Item = &'a str>,
) -> Vec<ConsoleEvent> {
  lines
    .map(sanitize_terminal_text)
    .filter(|line| !line.is_empty())
    .map(|detail| ConsoleEvent::Diagnostic {
      at: at.into(),
      label: role.as_str().to_ascii_uppercase(),
      summary: worker_id.into(),
      detail,
    })
    .collect()
}

mod render;
#[cfg(test)]
use render::format_duration;
use render::now;
pub(crate) use render::ConsoleRenderer;

#[cfg(test)]
mod tests;
