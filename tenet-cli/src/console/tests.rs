use std::collections::BTreeMap;

use super::{
  format_duration, ConsoleEvent, ConsolePresenter, ConsoleRenderer, InformationMode,
  ProgressSnapshot, RunHeader, SemanticStyle, Tone,
};
use chrono::Utc;
use tenet_domain::{
  events::{CompletionGate, CompletionGateItem, CompletionGateOutcome, RunEvent},
  evidence::{
    Evidence, EvidenceProvenance, EvidenceResult, EvidenceSource, EvidenceValidity,
    ObligationAssessment, ObligationAssessmentResult, SemanticAssessmentReport,
  },
  ids::{CriterionId, EvidenceId, ObligationId, RequirementId, VerificationRunId},
  model::{Phase, RepairProgress, RepositoryChange, RunStatus, State, WorkScope, WorkUnit},
  verification::{
    CommandResult, ProjectCheckResult, ProjectVerificationRun, VerificationReport, VerificationSpec,
  },
};

fn work_unit() -> WorkUnit {
  WorkUnit {
    id: "WU-003".into(),
    title: "Implement token expiry".into(),
    objective: "Enforce expiry".into(),
    requirement_ids: vec!["REQ-003".into()],
    criterion_ids: Vec::new(),
    verification_obligation_ids: Vec::new(),
    suggested_checks: Vec::new(),
    depends_on: Vec::new(),
    scope: WorkScope {
      paths: vec!["src/auth/**".into()],
    },
  }
}

fn failed_verification(timed_out: bool) -> ProjectVerificationRun {
  ProjectVerificationRun {
    run_id: VerificationRunId::new(),
    revision: "candidate".into(),
    suite_hash: "suite".into(),
    checks: vec![ProjectCheckResult {
      name: "integration-tests".into(),
      spec: VerificationSpec {
        program: "cargo".into(),
        args: vec!["test".into(), "--test".into(), "api".into()],
        working_directory: ".".into(),
        environment: BTreeMap::new(),
      },
      timeout_secs: 20,
      result: CommandResult {
        command: "cargo test --test api".into(),
        exit_code: (!timed_out).then_some(101),
        timed_out,
        duration_ms: 18_400,
        stdout: String::new(),
        stderr: "bookmark_duplicate_returns_409: expected 409, observed 500".into(),
      },
    }],
    passed: false,
    started_at: Utc::now(),
    finished_at: Utc::now(),
  }
}

fn render_summary(status: RunStatus, reason: Option<&str>) -> String {
  let mut state = State::fresh();
  state.status = status;
  state.blocked_reason = reason.map(str::to_owned);
  let mut renderer =
    ConsoleRenderer::new(Vec::new(), InformationMode::Default, SemanticStyle::plain());
  renderer.summary(&state, 42).unwrap();
  String::from_utf8(renderer.into_inner()).unwrap()
}

fn render(mode: InformationMode, events: &[ConsoleEvent]) -> String {
  let mut renderer = ConsoleRenderer::new(Vec::new(), mode, SemanticStyle::plain());
  for event in events {
    renderer.render(event).unwrap();
  }
  String::from_utf8(renderer.into_inner()).unwrap()
}

#[test]
fn semantic_style_is_unambiguous_without_ansi() {
  let style = SemanticStyle::plain();

  assert_eq!(style.marker(Tone::Success), "+");
  assert_eq!(style.marker(Tone::Warning), "!");
  assert_eq!(style.marker(Tone::Failure), "x");
  assert_eq!(style.marker(Tone::Active), "*");
  assert_eq!(style.marker(Tone::Milestone), ">");
}

#[test]
fn information_modes_have_explicit_detail_policies() {
  assert!(!InformationMode::Quiet.includes_diagnostics());
  assert!(!InformationMode::Default.includes_diagnostics());
  assert!(InformationMode::Verbose.includes_diagnostics());
}

#[test]
fn durations_remain_compact() {
  assert_eq!(format_duration(62), "1m 02s");
}

#[test]
fn cycle_and_progress_are_append_only_entries() {
  let output = render(
    InformationMode::Default,
    &[
      ConsoleEvent::CycleStarted(2),
      ConsoleEvent::Progress(ProgressSnapshot {
        requirements_verified: 3,
        requirements_total: 8,
        semantic_satisfied: 5,
        semantic_total: 10,
        work_completed: 1,
        work_total: 3,
        cycle: 2,
      }),
    ],
  );

  assert!(output.contains("Cycle 2"), "{output}");
  assert!(
    output.contains("requirements 3/8 · semantic 5/10 · work 1/3 · cycle 2"),
    "{output}"
  );
  assert!(!output.contains('\u{1b}'), "{output:?}");
}

#[test]
fn quiet_suppresses_plan_but_preserves_failures() {
  let output = render(
    InformationMode::Quiet,
    &[
      ConsoleEvent::Milestone {
        at: "10:00:00".into(),
        label: "PLAN",
        summary: "three work units".into(),
      },
      ConsoleEvent::Error {
        at: "10:00:01".into(),
        label: "FAILED",
        summary: "worker crashed".into(),
        detail: None,
      },
    ],
  );

  assert!(!output.contains("PLAN"), "{output}");
  assert!(output.contains("x FAILED"), "{output}");
}

#[test]
fn verbose_preserves_diagnostic_detail() {
  let output = render(
    InformationMode::Verbose,
    &[ConsoleEvent::Diagnostic {
      at: "10:00:00".into(),
      label: "TOOL".into(),
      summary: "cargo test complete".into(),
      detail: "stdout detail".into(),
    }],
  );

  assert!(output.contains("cargo test complete"), "{output}");
  assert!(output.contains("stdout detail"), "{output}");
}

#[test]
fn successful_one_work_unit_run_has_clear_lifecycle() {
  let output = render(
    InformationMode::Default,
    &[
      ConsoleEvent::WorkStarted {
        at: "12:31:03".into(),
        work: Box::new(work_unit()),
        worker_id: None,
        lease_id: None,
      },
      ConsoleEvent::WorkIntegrated {
        at: "12:32:14".into(),
        work_unit_id: "WU-003".into(),
        title: Some("Implement token expiry".into()),
        revision: "8fa02dd987".into(),
        changed_paths: 4,
        elapsed_seconds: Some(71),
      },
    ],
  );

  assert!(output.contains("* WU-003"), "{output}");
  assert!(output.contains("REQ-003 · src/auth/**"), "{output}");
  assert!(output.contains("+ WU-003"), "{output}");
  assert!(output.contains("integrated -> 8fa02dd"), "{output}");
  assert!(output.contains("4 files · 1m 11s"), "{output}");
}

#[test]
fn repair_is_attached_to_work_unit_and_cause() {
  let output = render(
    InformationMode::Default,
    &[ConsoleEvent::RepairStarted {
      at: "12:32:20".into(),
      work_unit_id: "WU-003".into(),
      attempt: 1,
      max_attempts: 3,
      reason: "integration-tests failed".into(),
    }],
  );

  assert!(output.contains("! REPAIR"), "{output}");
  assert!(output.contains("WU-003 · attempt 1/3"), "{output}");
  assert!(
    output.contains("reason: integration-tests failed"),
    "{output}"
  );
}

#[test]
fn project_verification_failure_has_bounded_actionable_detail() {
  let output = render(
    InformationMode::Default,
    &[ConsoleEvent::Verification {
      at: "12:32:19".into(),
      report: failed_verification(false),
      next_action: "repair WU-003".into(),
    }],
  );

  assert!(output.contains("x VERIFY"), "{output}");
  assert!(output.contains("check       integration-tests"), "{output}");
  assert!(output.contains("exit        101"), "{output}");
  assert!(output.contains("expected 409, observed 500"), "{output}");
  assert!(output.contains("next        repair WU-003"), "{output}");
}

#[test]
fn project_verification_timeout_is_distinct() {
  let output = render(
    InformationMode::Default,
    &[ConsoleEvent::Verification {
      at: "12:32:19".into(),
      report: failed_verification(true),
      next_action: "repair WU-003".into(),
    }],
  );

  assert!(output.contains("timeout after 20s"), "{output}");
}

#[test]
fn semantic_gap_and_uncertainty_are_distinct() {
  let report = SemanticAssessmentReport {
    summary: "assessment".into(),
    assessments: vec![
      ObligationAssessmentResult {
        obligation_id: ObligationId::from("REQ-003/AC-02/VO-01"),
        assessment: ObligationAssessment::Gap {
          description: "Required 15-minute expiry is not established".into(),
        },
      },
      ObligationAssessmentResult {
        obligation_id: ObligationId::from("REQ-006/AC-01/VO-02"),
        assessment: ObligationAssessment::Uncertain {
          reason: "Specification is ambiguous about retries".into(),
          specification_ambiguous: true,
        },
      },
    ],
  };
  let output = render(
    InformationMode::Default,
    &[ConsoleEvent::SemanticAssessment {
      at: "12:35:00".into(),
      report,
    }],
  );

  assert!(output.contains("x GAP"), "{output}");
  assert!(output.contains("! UNCERTAIN"), "{output}");
  assert!(output.contains("REQ-003/AC-02/VO-01"), "{output}");
  assert!(output.contains("REQ-006/AC-01/VO-02"), "{output}");
}

#[test]
fn stale_evidence_has_warning_semantics() {
  let output = render(
    InformationMode::Default,
    &[ConsoleEvent::StaleEvidence {
      at: "12:35:00".into(),
      evidence_id: "EV-12".into(),
      revision: "49eab13ffff".into(),
    }],
  );

  assert!(output.contains("! STALE"), "{output}");
  assert!(
    output.contains("repository advanced to 49eab13"),
    "{output}"
  );
}

#[test]
fn blocked_completion_gate_lists_controller_blocker() {
  let output = render(
    InformationMode::Default,
    &[ConsoleEvent::CompletionGate(CompletionGate {
      revision: "91cb348ffff".into(),
      earned: false,
      items: vec![CompletionGateItem {
        label: "semantic obligations".into(),
        outcome: CompletionGateOutcome::Unsatisfied,
        detail: "14/16 satisfied".into(),
      }],
      blockers: vec!["semantic assessment found a gap for REQ-006/AC-02/VO-01".into()],
    })],
  );

  assert!(output.contains("Completion gate"), "{output}");
  assert!(output.contains("x BLOCKED"), "{output}");
  assert!(output.contains("REQ-006/AC-02/VO-01"), "{output}");
}

#[test]
fn changed_paths_are_bounded_and_preserve_statuses() {
  let changes = (0..10)
    .map(|index| RepositoryChange {
      status: ['A', 'M', 'D', 'R'][index % 4],
      path: format!("src/file-{index}"),
    })
    .collect();
  let output = render(
    InformationMode::Default,
    &[ConsoleEvent::Changes {
      at: "12:34:00".into(),
      changes,
    }],
  );

  assert!(output.contains("10 files"), "{output}");
  assert!(output.contains("A src/file-0"), "{output}");
  assert!(output.contains("R src/file-3"), "{output}");
  assert!(output.contains("... 2 more"), "{output}");
  assert!(!output.contains("src/file-9"), "{output}");
}

#[test]
fn failed_run_summary_is_distinct() {
  let output = render_summary(RunStatus::Failed, Some("agent process exited"));

  assert!(output.contains("x FAILED"), "{output}");
  assert!(output.contains("agent process exited"), "{output}");
  assert!(!output.contains("tenet resume"), "{output}");
}

#[test]
fn stopped_run_summary_has_resume_guidance() {
  let output = render_summary(RunStatus::Stopped, Some("cancelled by user"));

  assert!(output.contains("! STOPPED"), "{output}");
  assert!(
    output.contains("Run `tenet resume` to continue."),
    "{output}"
  );
}

#[test]
fn default_mode_hides_routine_diagnostics() {
  let output = render(
    InformationMode::Default,
    &[ConsoleEvent::Diagnostic {
      at: "10:00:00".into(),
      label: "TOOL".into(),
      summary: "cargo test complete".into(),
      detail: "routine output".into(),
    }],
  );

  assert!(output.is_empty(), "{output}");
}

#[test]
fn plain_renderer_never_emits_ansi() {
  let output = render(
    InformationMode::Default,
    &[ConsoleEvent::Milestone {
      at: "10:00:00".into(),
      label: "PLAN",
      summary: "ready".into(),
    }],
  );

  assert!(!output.contains('\u{1b}'), "{output:?}");
  assert!(output.contains("> PLAN"), "{output}");
}

#[test]
fn run_header_is_rendered_once_with_known_context() {
  let header = RunHeader {
    repository: Some("bookmark-api".into()),
    revision: "eaa187c999".into(),
    specification: "spec.md".into(),
    agent: Some("test-agent".into()),
    requirements: Some(8),
    verification_checks: 3,
    max_cycles: 25,
  };
  let mut renderer =
    ConsoleRenderer::new(Vec::new(), InformationMode::Default, SemanticStyle::plain());
  renderer.header(&header).unwrap();
  renderer.header(&header).unwrap();
  let output = String::from_utf8(renderer.into_inner()).unwrap();

  assert_eq!(
    output.matches("TENET · autonomous engineering run").count(),
    1
  );
  assert!(output.contains("repository     bookmark-api"), "{output}");
  assert!(output.contains("revision       eaa187c"), "{output}");
  assert!(output.contains("requirements   8"), "{output}");
  assert!(
    output.contains("verification   3 project check(s)"),
    "{output}"
  );
}

#[test]
fn header_configuration_cannot_forge_or_overflow_lines() {
  let header = RunHeader {
    repository: Some("repo\nFORGED".into()),
    revision: "eaa187c999".into(),
    specification: "spec.md\n\u{1b}[31mFORGED".into(),
    agent: Some("agent".repeat(200)),
    requirements: None,
    verification_checks: 1,
    max_cycles: 1,
  };
  let mut renderer =
    ConsoleRenderer::new(Vec::new(), InformationMode::Default, SemanticStyle::plain());
  renderer.header(&header).unwrap();
  let output = String::from_utf8(renderer.into_inner()).unwrap();

  assert!(!output.contains('\u{1b}'), "{output:?}");
  assert!(!output.lines().any(|line| line == "FORGED"), "{output}");
  assert!(output.lines().all(|line| line.len() < 600), "{output}");
}

#[test]
fn completion_blockers_are_sanitized_and_bounded() {
  let output = render(
    InformationMode::Default,
    &[ConsoleEvent::CompletionGate(CompletionGate {
      revision: "91cb348".into(),
      earned: false,
      items: Vec::new(),
      blockers: vec![format!("gap\n\u{1b}[31mFORGED {}", "x".repeat(1_000))],
    })],
  );

  assert!(!output.contains('\u{1b}'), "{output:?}");
  assert!(!output.lines().any(|line| line == "FORGED"), "{output}");
  assert!(output.len() < 800, "{}", output.len());
}

#[test]
fn identical_progress_snapshots_are_suppressed() {
  let mut state = State::fresh();
  state.phase = Phase::Assessing;
  state.cycle = 2;
  state.requirement_counts.total = 8;
  state.requirement_counts.verified = 3;
  let mut presenter = ConsolePresenter::new(3);

  let first = presenter.present(&RunEvent::State(state.clone()));
  let second = presenter.present(&RunEvent::State(state));

  assert_eq!(
    first
      .iter()
      .filter(|event| matches!(event, ConsoleEvent::Progress(_)))
      .count(),
    1
  );
  assert!(!second
    .iter()
    .any(|event| matches!(event, ConsoleEvent::Progress(_))));
}

#[test]
fn done_summary_reads_as_earned_completion() {
  let output = render_summary(RunStatus::Done, None);

  assert!(output.contains("+ DONE"), "{output}");
  assert!(output.contains("Repository earned completion"), "{output}");
}

#[test]
fn advisory_failure_drives_repair_reason() {
  let report = VerificationReport {
    passed: false,
    started_at: Utc::now(),
    finished_at: Utc::now(),
    commands: vec![CommandResult {
      command: "cargo test".into(),
      exit_code: Some(101),
      timed_out: false,
      duration_ms: 100,
      stdout: String::new(),
      stderr: "token expiry assertion failed".into(),
    }],
    executions: Vec::new(),
    warnings: Vec::new(),
  };
  let mut state = State::fresh();
  state.current_repair = Some(RepairProgress {
    work_unit_id: "WU-003".into(),
    attempt: 1,
  });
  let mut presenter = ConsolePresenter::new(3);
  presenter.present(&RunEvent::Verification(report));

  let events = presenter.present(&RunEvent::State(state));
  let output = render(InformationMode::Default, &events);

  assert!(output.contains("WU-003 · attempt 1/3"), "{output}");
  assert!(output.contains("token expiry assertion failed"), "{output}");
}

#[test]
fn semantic_evidence_does_not_repeat_assessment_findings() {
  let obligation_id = ObligationId::from("REQ-003/AC-02/VO-01");
  let report = SemanticAssessmentReport {
    summary: "gap".into(),
    assessments: vec![ObligationAssessmentResult {
      obligation_id: obligation_id.clone(),
      assessment: ObligationAssessment::Gap {
        description: "expiry not established".into(),
      },
    }],
  };
  let evidence = Evidence {
    id: EvidenceId::new(),
    requirement_id: RequirementId::from("REQ-003"),
    criterion_id: CriterionId::from("REQ-003/AC-02"),
    obligation_id,
    source: EvidenceSource::SemanticAssessment,
    result: EvidenceResult::Failed,
    revision: "abc1234".into(),
    observed_at: Utc::now(),
    provenance: EvidenceProvenance::independent_assessment("assessor"),
    rationale: "expiry not established".into(),
    evidence_refs: Vec::new(),
    validity: EvidenceValidity::Valid,
  };
  let mut presenter = ConsolePresenter::new(3);
  let assessment_events = presenter.present(&RunEvent::SemanticAssessment(report));
  let evidence_events = presenter.present(&RunEvent::EvidenceFailed(evidence));

  let assessment_output = render(InformationMode::Default, &assessment_events);
  let evidence_output = render(InformationMode::Default, &evidence_events);

  assert_eq!(assessment_output.matches("GAP").count(), 1);
  assert!(evidence_output.is_empty(), "{evidence_output}");
}
