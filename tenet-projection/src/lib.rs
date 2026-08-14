use std::{
  collections::{BTreeMap, BTreeSet, VecDeque},
  time::Instant,
};

use tenet_domain::{
  events::RunEvent,
  model::{
    Phase, ReconcileResult, RepositoryChange, Requirement, RequirementAssessment,
    RequirementCatalog, RequirementStatus, RunStatus, State, VerificationReport, WorkExecution,
    WorkUnit, WorkerEvent, WorkerRole,
  },
};

pub const MAX_ACTIVITY: usize = 2_000;
pub const MAX_DETAIL_BYTES: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityCategory {
  Controller,
  Worker,
  Check,
  Error,
}

impl ActivityCategory {
  pub fn label(self) -> &'static str {
    match self {
      Self::Controller => "CONTROLLER",
      Self::Worker => "WORKER",
      Self::Check => "CHECK",
      Self::Error => "ATTENTION",
    }
  }
}

#[derive(Debug, Clone)]
pub struct Activity {
  pub at: String,
  pub category: ActivityCategory,
  pub title: String,

  pub summary: String,
  pub detail: String,
}

#[derive(Debug, Clone)]
pub struct WorkerSession {
  pub role: WorkerRole,
  pub worker_id: String,
  pub lease_id: Option<String>,
  pub work_unit_id: Option<String>,
  pub started_at: String,
  pub ended_at: Option<String>,
  pub ok: Option<bool>,
  pub summary: Option<String>,
  pub detail: String,
}

/// Terminal-independent projection of controller events into bounded run semantics.
pub struct RunProjection {
  state: State,
  catalog: Vec<Requirement>,
  assessments: BTreeMap<String, RequirementAssessment>,
  activities: VecDeque<Activity>,
  workers: VecDeque<WorkerSession>,
  checks: Vec<VerificationReport>,
  changes: Vec<RepositoryChange>,
  ready_work_units: Vec<WorkUnit>,
  candidate_queue: Vec<WorkExecution>,
  current_integration: Option<String>,
  completed_units: BTreeSet<String>,
  blocked_units: BTreeSet<String>,
  running: bool,
  streaming_worker: Option<String>,
  started: Instant,
}

impl RunProjection {
  pub fn new(state: State, catalog: Vec<Requirement>) -> Self {
    let completed_units = state
      .completed_work_units
      .iter()
      .map(|item| item.work_unit.id.clone())
      .collect();
    Self {
      running: matches!(state.status, RunStatus::Running),
      state,
      catalog,
      assessments: BTreeMap::new(),
      activities: VecDeque::new(),
      workers: VecDeque::new(),
      checks: Vec::new(),
      changes: Vec::new(),
      ready_work_units: Vec::new(),
      candidate_queue: Vec::new(),
      current_integration: None,
      completed_units,
      blocked_units: BTreeSet::new(),
      streaming_worker: None,
      started: Instant::now(),
    }
  }

  pub fn state(&self) -> &State {
    &self.state
  }
  pub fn catalog(&self) -> &[Requirement] {
    &self.catalog
  }
  pub fn assessments(&self) -> &BTreeMap<String, RequirementAssessment> {
    &self.assessments
  }
  pub fn activities(&self) -> &VecDeque<Activity> {
    &self.activities
  }
  pub fn workers(&self) -> &VecDeque<WorkerSession> {
    &self.workers
  }
  pub fn current_worker(&self) -> Option<&WorkerSession> {
    self.workers.back()
  }
  pub fn checks(&self) -> &[VerificationReport] {
    &self.checks
  }
  pub fn changes(&self) -> &[RepositoryChange] {
    &self.changes
  }
  pub fn ready_work_units(&self) -> &[WorkUnit] {
    &self.ready_work_units
  }
  pub fn candidate_queue(&self) -> &[WorkExecution] {
    &self.candidate_queue
  }
  pub fn current_integration(&self) -> Option<&str> {
    self.current_integration.as_deref()
  }
  pub fn completed_units(&self) -> &BTreeSet<String> {
    &self.completed_units
  }
  pub fn blocked_units(&self) -> &BTreeSet<String> {
    &self.blocked_units
  }
  pub fn active_work_units(&self) -> impl Iterator<Item = &WorkUnit> {
    self
      .state
      .active_leases
      .values()
      .map(|lease| &lease.work_unit)
  }
  pub fn running(&self) -> bool {
    self.running
  }
  pub fn elapsed_seconds(&self) -> u64 {
    self.started.elapsed().as_secs()
  }

  pub fn begin_run(&mut self) {
    self.running = true;
    self.started = Instant::now();
    self.activities.clear();
    self.workers.clear();
    self.checks.clear();
    self.changes.clear();
    self.streaming_worker = None;
    self.assessments.clear();
    self.push_activity(
      "now".into(),
      ActivityCategory::Controller,
      "RUN STARTED",
      "Controller started autonomous execution".into(),
      String::new(),
    );
  }

  pub fn apply(&mut self, event: RunEvent) {
    if !matches!(&event, RunEvent::Worker(WorkerEvent::Text { .. })) {
      self.streaming_worker = None;
    }
    match event {
      RunEvent::State(state) => self.apply_state(state),
      RunEvent::Catalog(RequirementCatalog { requirements, .. }) => self.catalog = requirements,
      RunEvent::Message(message) => {
        let category = if message.to_ascii_lowercase().contains("error") {
          ActivityCategory::Error
        } else {
          ActivityCategory::Controller
        };
        self.push_activity(now(), category, "CONTROLLER", message.clone(), message);
      }
      RunEvent::Worker(event) => self.apply_worker(event),
      RunEvent::Reconcile(result) => self.apply_reconcile(result),
      RunEvent::ReadyFrontier(units) => self.ready_work_units = units,
      RunEvent::LeaseIssued(lease) => self.push_activity(
        lease.issued_at,
        ActivityCategory::Controller,
        "LEASE ISSUED",
        format!("{} · {}", lease.work_unit.id, lease.worker_id),
        lease.workspace.display().to_string(),
      ),
      RunEvent::WorkerStarted {
        worker_id,
        work_unit_id,
        ..
      } => self.push_activity(
        now(),
        ActivityCategory::Worker,
        "WORKER STARTED",
        format!("{work_unit_id} · {worker_id}"),
        String::new(),
      ),
      RunEvent::CandidateProduced(candidate) => self.candidate_queue.push(candidate),
      RunEvent::IntegrationStarted {
        work_unit_id,
        candidate_revision,
      } => {
        self.current_integration = Some(work_unit_id.clone());
        self.push_activity(
          now(),
          ActivityCategory::Controller,
          "INTEGRATION",
          work_unit_id,
          candidate_revision,
        );
      }
      RunEvent::IntegrationAccepted {
        work_unit_id,
        revision,
      } => {
        self.current_integration = None;
        self.completed_units.insert(work_unit_id.clone());
        self
          .candidate_queue
          .retain(|candidate| candidate.lease.work_unit.id != work_unit_id);
        self.push_activity(
          now(),
          ActivityCategory::Check,
          "INTEGRATION ACCEPTED",
          work_unit_id,
          revision,
        );
      }
      RunEvent::IntegrationRejected {
        work_unit_id,
        reason,
      } => {
        self.current_integration = None;
        self.blocked_units.insert(work_unit_id.clone());
        self.push_activity(
          now(),
          ActivityCategory::Error,
          "INTEGRATION REJECTED",
          work_unit_id,
          reason,
        );
      }
      RunEvent::DependencyDiscovered {
        lease_id,
        discovery,
      } => self.push_activity(
        now(),
        ActivityCategory::Controller,
        "DISCOVERY",
        lease_id,
        format!("{discovery:?}"),
      ),
      RunEvent::WorkspaceCreated { lease_id, path } => self.push_activity(
        now(),
        ActivityCategory::Controller,
        "WORKSPACE CREATED",
        lease_id,
        path.display().to_string(),
      ),
      RunEvent::WorkspaceRemoved { lease_id, path } => self.push_activity(
        now(),
        ActivityCategory::Controller,
        "WORKSPACE REMOVED",
        lease_id,
        path.display().to_string(),
      ),
      RunEvent::Verification(report) => self.apply_check(report),
      RunEvent::RepositoryChanges(changes) => {
        let summary = format!("{} files changed", changes.len());
        let detail = changes
          .iter()
          .map(|change| format!("{} {}", change.status, change.path))
          .collect::<Vec<_>>()
          .join("\n");
        self.changes = changes;
        self.push_activity(
          now(),
          ActivityCategory::Controller,
          "CHANGES",
          summary,
          detail,
        );
      }
      RunEvent::Finished(state) => {
        self.apply_state(state);
        self.running = false;
      }
    }
  }

  fn apply_state(&mut self, next: State) {
    let changed = self.state.phase != next.phase
      || self.state.cycle != next.cycle
      || self.state.status != next.status;
    if changed {
      let category = if matches!(next.status, RunStatus::Blocked | RunStatus::Failed) {
        ActivityCategory::Error
      } else {
        ActivityCategory::Controller
      };
      self.push_activity(
        next.updated_at.clone(),
        category,
        phase_label(&next.phase),
        next.last_summary.clone(),
        state_detail(&next),
      );
    }
    self.completed_units = next
      .completed_work_units
      .iter()
      .map(|item| item.work_unit.id.clone())
      .collect();
    self.blocked_units = next
      .work_statuses
      .iter()
      .filter(|(_, status)| {
        matches!(
          status,
          tenet_domain::model::WorkStatus::Blocked | tenet_domain::model::WorkStatus::Failed
        )
      })
      .map(|(id, _)| id.clone())
      .collect();
    self.state = next;
  }

  fn apply_reconcile(&mut self, result: ReconcileResult) {
    for assessment in result.requirements {
      self.assessments.insert(assessment.id.clone(), assessment);
    }
    let summary = match result.work_units.as_slice() {
      [] => result.summary.clone(),
      [work] => format!("Proposed {} · {}", work.id, work.title),
      work => format!("Proposed {} work units", work.len()),
    };
    let detail = if result.work_units.is_empty() {
      result.summary
    } else {
      result
        .work_units
        .iter()
        .map(work_detail)
        .collect::<Vec<_>>()
        .join("\n\n")
    };
    self.push_activity(
      now(),
      ActivityCategory::Controller,
      "RECONCILE",
      summary,
      detail,
    );
  }

  fn apply_check(&mut self, report: VerificationReport) {
    let passed = report.passed;
    let summary = if passed {
      "PASS · deterministic checks passed".into()
    } else {
      format!("FAIL · {}", failure_preview(&report))
    };
    let detail = check_detail(&report);
    self.push_activity(
      report.finished_at.clone(),
      if passed {
        ActivityCategory::Check
      } else {
        ActivityCategory::Error
      },
      if passed { "CHECKS PASS" } else { "CHECKS FAIL" },
      summary,
      detail,
    );
    self.checks.push(report);
  }

  fn apply_worker(&mut self, event: WorkerEvent) {
    match event {
      WorkerEvent::Start {
        role,
        worker_id,
        lease_id,
        work_unit_id,
        at,
      } => {
        self.workers.push_back(WorkerSession {
          role,
          worker_id: worker_id.clone(),
          lease_id,
          work_unit_id,
          started_at: at.clone(),
          ended_at: None,
          ok: None,
          summary: None,
          detail: String::new(),
        });
        self.push_activity(
          at,
          ActivityCategory::Worker,
          format!("{} STARTED", role_label(role)),
          worker_id,
          String::new(),
        );
      }
      WorkerEvent::Text {
        role,
        worker_id,
        lease_id,
        work_unit_id,
        at,
        delta,
      } => {
        let text = sanitize_terminal_text(&delta);
        append_bounded(
          &mut self
            .worker_mut(role, &worker_id, lease_id, work_unit_id, &at)
            .detail,
          &text,
        );
        let continues_stream = self.streaming_worker.as_deref() == Some(&worker_id);
        if continues_stream {
          if let Some(activity) = self.activities.back_mut() {
            append_bounded(&mut activity.detail, &text);
            activity.summary = compact(&activity.detail, 160);
          }
        } else {
          self.push_activity(
            at,
            ActivityCategory::Worker,
            role_label(role),
            compact(&text, 160),
            text,
          );
        }
        self.streaming_worker = Some(worker_id);
      }
      WorkerEvent::ToolStart {
        role,
        at,
        tool_name,
        args,
        ..
      } => {
        let detail = format!(
          "Tool: {tool_name}\nArguments:\n{}",
          serde_json::to_string_pretty(&args).unwrap_or_else(|_| "{}".into())
        );
        self.push_activity(
          at,
          ActivityCategory::Worker,
          format!("{} · TOOL", role_label(role)),
          tool_name,
          detail,
        );
      }
      WorkerEvent::ToolEnd {
        role,
        at,
        tool_name,
        is_error,
        output,
        ..
      } => {
        let output = output
          .map(|value| sanitize_terminal_text(&value))
          .unwrap_or_default();
        let title = if is_error {
          "TOOL FAILED"
        } else {
          "TOOL COMPLETE"
        };
        self.push_activity(
          at,
          if is_error {
            ActivityCategory::Error
          } else {
            ActivityCategory::Worker
          },
          format!("{} · {title}", role_label(role)),
          tool_name.clone(),
          format!("Tool: {tool_name}\n\n{output}"),
        );
      }
      WorkerEvent::End {
        role,
        worker_id,
        lease_id,
        work_unit_id,
        at,
        ok,
        message,
      } => {
        let worker = self.worker_mut(role, &worker_id, lease_id, work_unit_id, &at);
        worker.ended_at = Some(at.clone());
        worker.ok = Some(ok);
        worker.summary = message.clone();
        let summary = message.unwrap_or_else(|| {
          if ok {
            "Worker completed".into()
          } else {
            "Worker failed".into()
          }
        });
        self.push_activity(
          at,
          if ok {
            ActivityCategory::Worker
          } else {
            ActivityCategory::Error
          },
          format!(
            "{} {}",
            role_label(role),
            if ok { "COMPLETE" } else { "FAILED" }
          ),
          summary.clone(),
          summary,
        );
      }
    }
  }

  fn worker_mut(
    &mut self,
    role: WorkerRole,
    worker_id: &str,
    lease_id: Option<String>,
    work_unit_id: Option<String>,
    at: &str,
  ) -> &mut WorkerSession {
    if let Some(index) = self
      .workers
      .iter()
      .rposition(|worker| worker.worker_id == worker_id)
    {
      return &mut self.workers[index];
    }
    self.workers.push_back(WorkerSession {
      role,
      worker_id: worker_id.into(),
      lease_id,
      work_unit_id,
      started_at: at.into(),
      ended_at: None,
      ok: None,
      summary: None,
      detail: String::new(),
    });
    self
      .workers
      .back_mut()
      .expect("worker session was inserted")
  }

  fn push_activity(
    &mut self,
    at: String,
    category: ActivityCategory,
    title: impl Into<String>,
    summary: String,
    detail: String,
  ) {
    self.activities.push_back(Activity {
      at,
      category,
      title: title.into(),
      summary,
      detail: compact(&detail, MAX_DETAIL_BYTES),
    });
    while self.activities.len() > MAX_ACTIVITY {
      self.activities.pop_front();
    }
  }
}

pub fn role_label(role: WorkerRole) -> &'static str {
  match role {
    WorkerRole::Architect => "ARCHITECT",
    WorkerRole::Reconcile => "RECONCILE",
    WorkerRole::Implement => "IMPLEMENT",
    WorkerRole::Repair => "REPAIR",
    WorkerRole::Assess => "ASSESS",
  }
}
pub fn phase_label(phase: &Phase) -> &'static str {
  match phase {
    Phase::Initialized => "READY",
    Phase::Architecting => "ARCHITECT",
    Phase::Reconciling => "RECONCILE",
    Phase::Scheduling => "SCHEDULE",
    Phase::Implementing => "IMPLEMENT",
    Phase::Verifying => "VERIFY",
    Phase::Repairing => "REPAIR",
    Phase::Integrating => "INTEGRATE",
    Phase::Assessing => "ASSESS",
    Phase::Complete => "COMPLETE",
  }
}
pub fn status_label(status: &RunStatus) -> &'static str {
  match status {
    RunStatus::Idle => "READY",
    RunStatus::Running => "RUNNING",
    RunStatus::Done => "COMPLETE",
    RunStatus::Blocked => "BLOCKED",
    RunStatus::Failed => "FAILED",
    RunStatus::Stopped => "STOPPED",
  }
}
pub fn requirement_status(assessment: Option<&RequirementAssessment>) -> RequirementStatus {
  assessment.map_or(RequirementStatus::Missing, |item| item.status.clone())
}
pub fn failure_preview(report: &VerificationReport) -> String {
  report
    .commands
    .iter()
    .find(|item| item.exit_code != Some(0) || item.timed_out)
    .map(|item| {
      let output = if !item.stderr.trim().is_empty() {
        &item.stderr
      } else {
        &item.stdout
      };
      format!(
        "{} · {}",
        item.command,
        compact(&sanitize_terminal_text(output), 240)
      )
    })
    .unwrap_or_else(|| "verification failed".into())
}
pub fn sanitize_terminal_text(text: &str) -> String {
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
pub fn compact(text: &str, max: usize) -> String {
  if text.len() <= max {
    return text.into();
  }
  let mut end = max;
  while !text.is_char_boundary(end) {
    end -= 1;
  }
  format!("{}…", &text[..end])
}

fn append_bounded(target: &mut String, text: &str) {
  target.push_str(text);
  *target = compact(target, MAX_DETAIL_BYTES);
}
fn state_detail(state: &State) -> String {
  format!(
    "Status: {}\nPhase: {}\nCycle: {}\nRequirements: {}/{} satisfied\n\n{}",
    status_label(&state.status),
    phase_label(&state.phase),
    state.cycle,
    state.requirement_counts.satisfied,
    state.requirement_counts.total,
    state.last_summary
  )
}
fn work_detail(work: &tenet_domain::model::WorkUnit) -> String {
  format!("{} · {}\n\nObjective\n{}\n\nRequirements\n{}\n\nAcceptance criteria\n{}\n\nSuggested checks\n{}", work.id, work.title, work.objective, bullets(&work.requirement_ids), bullets(&work.acceptance_criteria), bullets(&work.suggested_checks))
}
pub fn check_detail(report: &VerificationReport) -> String {
  let commands = report
    .commands
    .iter()
    .map(|command| {
      format!(
        "{} {} ({} ms)\n{}",
        if command.exit_code == Some(0) && !command.timed_out {
          "PASS"
        } else if command.timed_out {
          "TIMEOUT"
        } else {
          "FAIL"
        },
        command.command,
        command.duration_ms,
        compact(
          &sanitize_terminal_text(if command.stderr.trim().is_empty() {
            &command.stdout
          } else {
            &command.stderr
          }),
          MAX_DETAIL_BYTES
        )
      )
    })
    .collect::<Vec<_>>()
    .join("\n\n");
  format!("{}\n\nWarnings\n{}", commands, bullets(&report.warnings))
}
fn bullets(items: &[String]) -> String {
  if items.is_empty() {
    "None".into()
  } else {
    items
      .iter()
      .map(|item| format!("• {item}"))
      .collect::<Vec<_>>()
      .join("\n")
  }
}
fn now() -> String {
  chrono::Local::now().format("%H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
  use super::*;
  use tenet_domain::model::{CommandResult, RequirementCounts, WorkScope, WorkUnit};

  fn state(status: RunStatus, phase: Phase) -> State {
    let mut state = State::fresh();
    state.status = status;
    state.phase = phase;
    state.run_id = Some("run".into());
    state.cycle = 4;
    state.requirement_counts = RequirementCounts {
      total: 2,
      satisfied: 1,
      partial: 1,
      missing: 0,
    };
    state.last_summary = "Controller update".into();
    state.updated_at = "10:00:00".into();
    state
  }
  fn report(passed: bool) -> VerificationReport {
    VerificationReport {
      passed,
      started_at: "10:00:00".into(),
      finished_at: "10:00:01".into(),
      warnings: Vec::new(),
      commands: vec![CommandResult {
        command: "cargo test".into(),
        exit_code: passed.then_some(0).or(Some(1)),
        timed_out: false,
        duration_ms: 1000,
        stdout: String::new(),
        stderr: if passed {
          String::new()
        } else {
          "\u{1b}[31massertion failed\u{1b}[0m".into()
        },
      }],
    }
  }
  fn work() -> WorkUnit {
    WorkUnit {
      id: "W-014".into(),
      title: "Authentication error handling".into(),
      objective: "Handle errors".into(),
      requirement_ids: vec!["REQ-012".into()],
      acceptance_criteria: Vec::new(),
      suggested_checks: Vec::new(),
      depends_on: Vec::new(),
      scope: WorkScope {
        paths: vec!["src/auth/**".into()],
      },
    }
  }

  #[test]
  fn projects_reconcile_implementation_and_verification() {
    let mut projection = RunProjection::new(State::fresh(), Vec::new());
    projection.apply(RunEvent::Reconcile(ReconcileResult {
      complete: false,
      summary: "select work".into(),
      requirements: vec![RequirementAssessment {
        id: "REQ-012".into(),
        status: RequirementStatus::Partial,
        evidence: vec!["API exists".into()],
        gaps: vec!["No test".into()],
      }],
      work_units: vec![work()],
    }));
    projection.apply(RunEvent::Worker(WorkerEvent::Start {
      role: WorkerRole::Implement,
      worker_id: "worker-1".into(),
      lease_id: Some("lease-1".into()),
      work_unit_id: Some("W-014".into()),
      at: "10:00:00".into(),
    }));
    projection.apply(RunEvent::Verification(report(true)));
    assert_eq!(
      projection.assessments()["REQ-012"].status,
      RequirementStatus::Partial
    );
    assert!(projection
      .activities()
      .iter()
      .any(|item| item.summary.contains("W-014")));
    assert!(projection
      .activities()
      .iter()
      .any(|item| item.title == "CHECKS PASS"));
    assert_eq!(
      projection.current_worker().unwrap().role,
      WorkerRole::Implement
    );
  }

  #[test]
  fn makes_failure_repair_and_pass_causal() {
    let mut projection = RunProjection::new(State::fresh(), Vec::new());
    projection.apply(RunEvent::Verification(report(false)));
    projection.apply(RunEvent::State(state(RunStatus::Running, Phase::Repairing)));
    projection.apply(RunEvent::Verification(report(true)));
    let titles = projection
      .activities()
      .iter()
      .map(|item| item.title.as_str())
      .collect::<Vec<_>>();
    assert!(titles
      .windows(3)
      .any(|window| window == ["CHECKS FAIL", "REPAIR", "CHECKS PASS"]));
    assert!(failure_preview(projection.checks().first().unwrap()).contains("assertion failed"));
  }

  #[test]
  fn projects_terminal_states_and_changes() {
    for status in [RunStatus::Done, RunStatus::Blocked, RunStatus::Stopped] {
      let mut projection = RunProjection::new(State::fresh(), Vec::new());
      let mut terminal = state(status.clone(), Phase::Complete);
      terminal.blocked_reason =
        (status == RunStatus::Blocked).then_some("Repair attempts exhausted".into());
      projection.apply(RunEvent::Finished(terminal));
      assert!(!projection.running());
      assert_eq!(projection.state().status, status);
    }
    let mut projection = RunProjection::new(State::fresh(), Vec::new());
    projection.apply(RunEvent::RepositoryChanges(vec![RepositoryChange {
      status: 'M',
      path: "src/auth.rs".into(),
    }]));
    assert_eq!(projection.changes()[0].path, "src/auth.rs");
    assert!(projection
      .activities()
      .iter()
      .any(|item| item.title == "CHANGES"));
  }

  #[test]
  fn coalesces_text_chunks_until_the_next_worker_response() {
    let mut projection = RunProjection::new(State::fresh(), Vec::new());
    for delta in ["Starting ", "deterministic run"] {
      projection.apply(RunEvent::Worker(WorkerEvent::Text {
        role: WorkerRole::Architect,
        worker_id: "architect-1".into(),
        lease_id: None,
        work_unit_id: None,
        at: "10:00:00".into(),
        delta: delta.into(),
      }));
    }

    assert_eq!(projection.activities().len(), 1);
    assert_eq!(
      projection.activities()[0].summary,
      "Starting deterministic run"
    );

    projection.apply(RunEvent::Worker(WorkerEvent::ToolStart {
      role: WorkerRole::Architect,
      worker_id: "architect-1".into(),
      lease_id: None,
      work_unit_id: None,
      at: "10:00:01".into(),
      tool_name: "read".into(),
      args: serde_json::json!({}),
    }));
    for delta in ["Second ", "response"] {
      projection.apply(RunEvent::Worker(WorkerEvent::Text {
        role: WorkerRole::Architect,
        worker_id: "architect-1".into(),
        lease_id: None,
        work_unit_id: None,
        at: "10:00:02".into(),
        delta: delta.into(),
      }));
    }

    assert_eq!(projection.activities().len(), 3);
    assert_eq!(projection.activities()[2].summary, "Second response");
  }

  #[test]
  fn concurrent_workers_with_same_role_remain_distinct() {
    let mut projection = RunProjection::new(State::fresh(), Vec::new());
    for (worker_id, lease_id, work_unit_id) in
      [("worker-b", "lease-b", "B"), ("worker-c", "lease-c", "C")]
    {
      projection.apply(RunEvent::Worker(WorkerEvent::Start {
        role: WorkerRole::Implement,
        worker_id: worker_id.into(),
        lease_id: Some(lease_id.into()),
        work_unit_id: Some(work_unit_id.into()),
        at: "10:00:00".into(),
      }));
    }

    assert_eq!(projection.workers().len(), 2);
    assert_ne!(
      projection.workers()[0].worker_id,
      projection.workers()[1].worker_id
    );
  }

  #[test]
  fn sanitization_bounds_and_failure_previews_are_shared() {
    assert_eq!(sanitize_terminal_text("a\u{1b}[31mb\0"), "ab");
    assert_eq!(compact("ééé", 5), "éé…");
    let mut failed = report(false);
    failed.commands[0].stderr = "x".repeat(8_000);
    assert!(failure_preview(&failed).len() <= 300);
    assert!(check_detail(&failed).len() <= MAX_DETAIL_BYTES + 100);
  }
}
