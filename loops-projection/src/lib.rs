use std::{
  collections::{BTreeMap, VecDeque},
  time::Instant,
};

use loops_domain::{
  events::RunEvent,
  model::{
    Phase, ReconcileResult, RepositoryChange, Requirement, RequirementAssessment,
    RequirementCatalog, RequirementStatus, RunStatus, State, VerificationReport, WorkerEvent,
    WorkerRole,
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
  running: bool,
  started: Instant,
}

impl RunProjection {
  pub fn new(state: State, catalog: Vec<Requirement>) -> Self {
    Self {
      running: matches!(state.status, RunStatus::Running),
      state,
      catalog,
      assessments: BTreeMap::new(),
      activities: VecDeque::new(),
      workers: VecDeque::new(),
      checks: Vec::new(),
      changes: Vec::new(),
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
    self.state = next;
  }

  fn apply_reconcile(&mut self, result: ReconcileResult) {
    for assessment in result.requirements {
      self.assessments.insert(assessment.id.clone(), assessment);
    }
    let summary = result.next_work_unit.as_ref().map_or_else(
      || result.summary.clone(),
      |work| format!("Selected {} · {}", work.id, work.title),
    );
    let detail = result
      .next_work_unit
      .map_or(result.summary, |work| work_detail(&work));
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
      WorkerEvent::Start { role, at } => {
        self.workers.push_back(WorkerSession {
          role,
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
          "Worker session started".into(),
          String::new(),
        );
      }
      WorkerEvent::Text { role, at, delta } => {
        let text = sanitize_terminal_text(&delta);
        append_bounded(&mut self.worker_mut(role, &at).detail, &text);
        self.push_activity(
          at,
          ActivityCategory::Worker,
          role_label(role),
          compact(&text, 160),
          text,
        );
      }
      WorkerEvent::ToolStart {
        role,
        at,
        tool_name,
        args,
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
        at,
        ok,
        message,
      } => {
        let worker = self.worker_mut(role, &at);
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

  fn worker_mut(&mut self, role: WorkerRole, at: &str) -> &mut WorkerSession {
    let new = self
      .workers
      .back()
      .is_none_or(|worker| worker.role != role || worker.ended_at.is_some());
    if new {
      self.workers.push_back(WorkerSession {
        role,
        started_at: at.into(),
        ended_at: None,
        ok: None,
        summary: None,
        detail: String::new(),
      });
    }
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
    Phase::Implementing => "IMPLEMENT",
    Phase::Verifying => "VERIFY",
    Phase::Repairing => "REPAIR",
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
  if !target.is_empty() {
    target.push('\n');
  }
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
fn work_detail(work: &loops_domain::model::WorkUnit) -> String {
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
  use loops_domain::model::{CommandResult, RequirementCounts, WorkUnit};

  fn state(status: RunStatus, phase: Phase) -> State {
    State {
      version: 1,
      status,
      phase,
      run_id: Some("run".into()),
      cycle: 4,
      current_work_unit: None,
      requirement_counts: RequirementCounts {
        total: 2,
        satisfied: 1,
        partial: 1,
        missing: 0,
      },
      completed_work_units: Vec::new(),
      last_summary: "Controller update".into(),
      blocked_reason: None,
      last_error: None,
      updated_at: "10:00:00".into(),
    }
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
      next_work_unit: Some(work()),
    }));
    projection.apply(RunEvent::Worker(WorkerEvent::Start {
      role: WorkerRole::Implement,
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
  fn sanitization_bounds_and_failure_previews_are_shared() {
    assert_eq!(sanitize_terminal_text("a\u{1b}[31mb\0"), "ab");
    assert_eq!(compact("ééé", 5), "éé…");
    let mut failed = report(false);
    failed.commands[0].stderr = "x".repeat(8_000);
    assert!(failure_preview(&failed).len() <= 300);
    assert!(check_detail(&failed).len() <= MAX_DETAIL_BYTES + 100);
  }
}
