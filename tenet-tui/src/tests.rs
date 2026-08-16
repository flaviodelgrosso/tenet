use ratatui::{backend::TestBackend, Terminal};
use tenet_domain::{
  events::RunEvent,
  evidence::{AcceptanceCriterion, ImplementationState},
  ids::{CriterionId, ObligationId, RequirementId},
  model::{
    CommandResult, Phase, ReconcileResult, RepositoryChange, Requirement, RequirementAssessment,
    RequirementCatalog, RunStatus, State, VerificationReport, WorkScope, WorkUnit, WorkerEvent,
    WorkerRole,
  },
};

use crate::tui::{
  action::{Action, ActivityCategory, Overlay, Screen},
  app::{phase_label, status_label, Application},
  layout::{density, Density},
  render,
};

fn requirement(id: &str) -> Requirement {
  Requirement {
    id: RequirementId::from(id),
    title: format!("Requirement {id}"),
    description: "Make the observable contract work".into(),
    required: true,
  }
}
fn app() -> Application {
  Application::new(
    "demo".into(),
    State::fresh(),
    vec![requirement("R1"), requirement("R2")],
  )
}
fn report(passed: bool) -> VerificationReport {
  VerificationReport {
    passed,
    started_at: "2026-08-16T10:00:00Z".parse().expect("valid timestamp"),
    finished_at: "2026-08-16T10:00:01Z".parse().expect("valid timestamp"),
    commands: vec![CommandResult {
      command: "cargo test".into(),
      exit_code: if passed { Some(0) } else { Some(101) },
      timed_out: false,
      duration_ms: 1000,
      stdout: String::new(),
      stderr: if passed {
        String::new()
      } else {
        "assertion failed".into()
      },
    }],
    warnings: Vec::new(),
  }
}

#[test]
fn projection_builds_causal_activity_feed() {
  let mut app = app();
  let mut state = State::fresh();
  state.status = RunStatus::Running;
  state.phase = Phase::Implementing;
  state.cycle = 1;
  state.last_summary = "Implementing R1".into();
  app.apply(RunEvent::State(state));
  app.apply(RunEvent::Worker(WorkerEvent::Start {
    role: WorkerRole::Implement,
    worker_id: "worker-1".into(),
    lease_id: Some("lease-1".into()),
    work_unit_id: Some("W1".into()),
    at: "10:00:00".into(),
  }));
  app.apply(RunEvent::Worker(WorkerEvent::ToolStart {
    role: WorkerRole::Implement,
    worker_id: "worker-1".into(),
    lease_id: Some("lease-1".into()),
    work_unit_id: Some("W1".into()),
    at: "10:00:01".into(),
    tool_name: "cargo test".into(),
    args: serde_json::json!({"all": true}),
  }));
  app.apply(RunEvent::Verification(report(false)));
  assert!(app
    .activities()
    .iter()
    .any(|event| event.title == "IMPLEMENT"));
  assert!(app
    .activities()
    .iter()
    .any(|event| event.category == ActivityCategory::Error && event.title == "CHECKS FAIL"));
}

#[test]
fn run_feed_shows_each_successful_tool_once_and_keeps_failures() {
  let mut app = app();
  let tool_start = WorkerEvent::ToolStart {
    role: WorkerRole::Implement,
    worker_id: "worker-1".into(),
    lease_id: Some("lease-1".into()),
    work_unit_id: Some("W1".into()),
    at: "10:00:01".into(),
    tool_name: "cargo test".into(),
    args: serde_json::json!({"all": true}),
  };
  app.apply(RunEvent::Worker(tool_start));
  app.apply(RunEvent::Worker(WorkerEvent::ToolEnd {
    role: WorkerRole::Implement,
    worker_id: "worker-1".into(),
    lease_id: Some("lease-1".into()),
    work_unit_id: Some("W1".into()),
    at: "10:00:02".into(),
    tool_name: "cargo test".into(),
    is_error: false,
    output: None,
  }));
  app.apply(RunEvent::Worker(WorkerEvent::ToolEnd {
    role: WorkerRole::Implement,
    worker_id: "worker-1".into(),
    lease_id: Some("lease-1".into()),
    work_unit_id: Some("W1".into()),
    at: "10:00:03".into(),
    tool_name: "cargo clippy".into(),
    is_error: true,
    output: Some("failed".into()),
  }));

  let visible_titles = app
    .visible_feed()
    .into_iter()
    .map(|index| app.activities()[index].title.as_str())
    .collect::<Vec<_>>();
  assert_eq!(
    visible_titles,
    ["IMPLEMENT · TOOL", "IMPLEMENT · TOOL FAILED"]
  );
}

#[test]
fn requirements_selection_is_screen_local_and_filtered() {
  let mut app = app();
  app.dispatch(Action::Go(Screen::Requirements));
  app.dispatch(Action::OpenSearch);
  app.dispatch(Action::Type('r'));
  app.dispatch(Action::Type('2'));
  app.dispatch(Action::Confirm);
  assert_eq!(app.visible_requirements(), vec![1]);
  app.dispatch(Action::Navigate(1));
  assert_eq!(app.ui().requirements.selected, 0);
  app.dispatch(Action::Go(Screen::Checks));
  assert_eq!(app.ui().requirements.selected, 0);
}

#[test]
fn run_search_filters_without_losing_activity_selection() {
  let mut app = app();
  app.begin_run();
  app.apply(RunEvent::Message("first controller event".into()));
  app.apply(RunEvent::Message("target deployment event".into()));
  app.dispatch(Action::OpenSearch);
  for character in "target".chars() {
    app.dispatch(Action::Type(character));
  }
  app.dispatch(Action::Confirm);
  assert_eq!(app.visible_feed().len(), 1);
  assert_eq!(
    app.selected_activity().unwrap().summary,
    "target deployment event"
  );
}

#[test]
fn follow_stops_when_scrolling_and_end_restores_it() {
  let mut app = app();
  app.begin_run();
  app.apply(RunEvent::Message("one".into()));
  app.dispatch(Action::Navigate(-1));
  app.apply(RunEvent::Message("two".into()));
  assert!(!app.ui().run.follow);
  assert_eq!(app.ui().run.unseen, 1);
  app.dispatch(Action::Last);
  assert!(app.ui().run.follow);
  assert_eq!(app.ui().run.unseen, 0);
}

#[test]
fn overlay_state_is_single_explicit_stack() {
  let mut app = app();
  app.dispatch(Action::OpenHelp);
  assert!(matches!(app.ui().overlay, Some(Overlay::Help)));
  app.dispatch(Action::Cancel);
  assert!(app.ui().overlay.is_none());
  app.dispatch(Action::OpenPalette);
  app.dispatch(Action::Type('h'));
  app.dispatch(Action::Type('e'));
  app.dispatch(Action::Type('l'));
  app.dispatch(Action::Type('p'));
  app.dispatch(Action::Confirm);
  assert!(matches!(app.ui().overlay, Some(Overlay::Help)));
}

#[test]
fn active_quit_requires_confirmation_before_stop() {
  let mut app = app();
  app.begin_run();
  assert_eq!(app.dispatch(Action::Exit), crate::tui::action::Effect::None);
  assert!(matches!(app.ui().overlay, Some(Overlay::ConfirmStop)));
  app.dispatch(Action::Cancel);
  assert!(app.ui().overlay.is_none());
  app.dispatch(Action::Exit);
  assert_eq!(
    app.dispatch(Action::Confirm),
    crate::tui::action::Effect::Stop
  );
}

#[test]
fn responsive_layout_has_intentional_thresholds() {
  assert_eq!(
    density(ratatui::layout::Rect::new(0, 0, 41, 20)),
    Density::TooSmall
  );
  assert_eq!(
    density(ratatui::layout::Rect::new(0, 0, 50, 20)),
    Density::Narrow
  );
  assert_eq!(
    density(ratatui::layout::Rect::new(0, 0, 80, 20)),
    Density::Medium
  );
  assert_eq!(
    density(ratatui::layout::Rect::new(0, 0, 110, 20)),
    Density::Wide
  );
}

#[test]
fn status_and_phase_labels_are_human_oriented() {
  assert_eq!(phase_label(&Phase::Repairing), "REPAIR");
  assert_eq!(status_label(&RunStatus::Blocked), "BLOCKED");
}

#[test]
fn activity_storage_is_bounded() {
  let mut app = app();
  for index in 0..2_100 {
    app.apply(RunEvent::Message(format!("message {index}")));
  }
  assert_eq!(app.activities().len(), 2_000);
}

#[test]
fn verification_failure_then_repair_is_visible() {
  let mut app = app();
  app.apply(RunEvent::Verification(report(false)));
  let mut state = State::fresh();
  state.status = RunStatus::Running;
  state.phase = Phase::Repairing;
  state.last_summary = "Repairing R1".into();
  app.apply(RunEvent::State(state));
  app.apply(RunEvent::Verification(report(true)));
  let titles = app
    .activities()
    .iter()
    .map(|event| event.title.as_str())
    .collect::<Vec<_>>();
  assert!(titles
    .windows(3)
    .any(|window| window == ["CHECKS FAIL", "REPAIR", "CHECKS PASS"]));
}

#[test]
fn completed_and_blocked_states_render_without_dashboard_failure() {
  for status in [RunStatus::Done, RunStatus::Blocked] {
    let mut app = app();
    let mut state = State::fresh();
    state.status = status;
    state.phase = Phase::Complete;
    state.last_summary = "Terminal result".into();
    app.apply(RunEvent::Finished(state));
    let backend = TestBackend::new(120, 32);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render::render(frame, &app)).unwrap();
  }
}

#[test]
fn idle_and_narrow_terminal_render() {
  let app = app();
  for (width, height) in [(120, 32), (42, 12)] {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render::render(frame, &app)).unwrap();
  }
}

#[test]
fn requirement_projection_preserves_evidence_and_gaps() {
  let mut app = app();
  app.apply(RunEvent::Catalog(RequirementCatalog {
    spec_hash: "hash".into(),
    requirements: vec![requirement("R1")],
    acceptance_criteria: vec![AcceptanceCriterion {
      id: CriterionId::from("R1/AC-01"),
      requirement_id: RequirementId::from("R1"),
      description: "It works".into(),
      mandatory: true,
    }],
    verification_obligations: Vec::new(),
  }));
  app.apply(RunEvent::Reconcile(ReconcileResult {
    summary: "Missing R1".into(),
    requirements: vec![RequirementAssessment {
      requirement_id: RequirementId::from("R1"),
      implementation_state: ImplementationState::Partial,
      observations: vec!["Implemented API".into()],
      missing_implementation: vec!["No check".into()],
      missing_evidence: vec![ObligationId::from("R1/AC-01/VO-01")],
    }],
    work_units: vec![WorkUnit {
      id: "W1".into(),
      title: "Finish R1".into(),
      objective: "Complete it".into(),
      requirement_ids: vec![RequirementId::from("R1")],
      criterion_ids: vec![CriterionId::from("R1/AC-01")],
      verification_obligation_ids: vec![ObligationId::from("R1/AC-01/VO-01")],
      suggested_checks: vec![],
      depends_on: Vec::new(),
      scope: WorkScope {
        paths: vec!["src/**".into()],
      },
    }],
  }));
  assert_eq!(
    app.assessments().get("R1").unwrap().implementation_state,
    ImplementationState::Partial
  );
}

fn rendered(app: &Application, width: u16, height: u16) -> String {
  let backend = TestBackend::new(width, height);
  let mut terminal = Terminal::new(backend).expect("test terminal");
  terminal
    .draw(|frame| render::render(frame, app))
    .expect("render should succeed");
  let buffer = terminal.backend().buffer();
  (0..height)
    .map(|y| {
      (0..width)
        .map(|x| buffer[(x, y)].symbol())
        .collect::<String>()
    })
    .collect::<Vec<_>>()
    .join("\n")
}

#[test]
fn idle_render_has_intentional_start_hierarchy() {
  let output = rendered(&app(), 140, 40);
  assert!(output.contains("Press r to start"), "{output}");
}

#[test]
fn active_run_renders_phase_tool_and_focus_at_wide_and_narrow_sizes() {
  let mut app = app();
  app.begin_run();
  let mut state = State::fresh();
  state.status = RunStatus::Running;
  state.phase = Phase::Implementing;
  state.cycle = 4;
  state.last_summary = "Implement authentication".into();
  app.apply(RunEvent::State(state));
  app.apply(RunEvent::Worker(WorkerEvent::ToolStart {
    role: WorkerRole::Implement,
    worker_id: "worker-2".into(),
    lease_id: Some("lease-2".into()),
    work_unit_id: Some("W2".into()),
    at: "10:01:00".into(),
    tool_name: "cargo test".into(),
    args: serde_json::json!({"package": "tenet-tui"}),
  }));
  let wide = rendered(&app, 140, 40);
  let narrow = rendered(&app, 60, 22);
  assert!(wide.contains("CONTEXT"), "{wide}");
  assert!(wide.contains("◆  IMPLEMENT"), "{wide}");
  assert!(wide.contains("TOOL"), "{wide}");
  assert!(narrow.contains("IMPLEMENT"), "{narrow}");
  assert!(!narrow.contains("CONTEXT"), "{narrow}");
  assert!(wide.contains("▌"), "{wide}");
  assert!(!narrow.contains("10:01:00"), "{narrow}");
}

#[test]
fn activity_feed_preserves_full_event_titles() {
  let mut app = app();
  app.begin_run();
  app.apply(RunEvent::IntegrationAccepted {
    work_unit_id: "ACWU-001".into(),
    revision: "abc123".into(),
  });

  let output = rendered(&app, 90, 22);

  assert!(output.contains("INTEGRATION ACCEPTED"), "{output}");
}

#[test]
fn failure_repair_and_checks_render_structured_evidence() {
  let mut app = app();
  app.begin_run();
  app.apply(RunEvent::Verification(report(false)));
  let mut state = State::fresh();
  state.status = RunStatus::Running;
  state.phase = Phase::Repairing;
  state.last_summary = "Repairing the failing assertion".into();
  app.apply(RunEvent::State(state));
  app.dispatch(Action::Go(Screen::Checks));
  let output = rendered(&app, 110, 32);
  assert!(output.contains("✕ FAIL"), "{output}");
  assert!(output.contains("assertion failed"), "{output}");
  assert!(output.contains("ATTEMPT 01"), "{output}");
}

#[test]
fn requirements_changes_history_and_overlays_have_composed_surfaces() {
  let mut app = app();
  app.apply(RunEvent::Reconcile(ReconcileResult {
    summary: "R1 is partial".into(),
    requirements: vec![RequirementAssessment {
      requirement_id: RequirementId::from("R1"),
      implementation_state: ImplementationState::Partial,
      observations: vec!["Implementation exists".into()],
      missing_implementation: vec!["No verification yet".into()],
      missing_evidence: vec![ObligationId::from("R1/AC-01/VO-01")],
    }],
    work_units: Vec::new(),
  }));
  app.dispatch(Action::Go(Screen::Requirements));
  let requirements = rendered(&app, 140, 40);
  assert!(requirements.contains("DETAIL"), "{requirements}");
  assert!(
    requirements.contains("No verification yet"),
    "{requirements}"
  );

  app.apply(RunEvent::RepositoryChanges(vec![RepositoryChange {
    status: 'M',
    path: "tenet-tui/src/render.rs".into(),
  }]));
  app.dispatch(Action::Go(Screen::Changes));
  let changes = rendered(&app, 90, 28);
  assert!(changes.contains("MODIFIED"), "{changes}");

  app.dispatch(Action::Go(Screen::History));
  let history = rendered(&app, 90, 28);
  assert!(history.contains("HISTORY"), "{history}");

  app.dispatch(Action::OpenPalette);
  app.dispatch(Action::Type('r'));
  let palette = rendered(&app, 90, 28);
  assert!(palette.contains("COMMAND"), "{palette}");
  assert!(palette.contains("Requirements"), "{palette}");
  app.dispatch(Action::Cancel);
  app.dispatch(Action::OpenSearch);
  let search = rendered(&app, 90, 28);
  assert!(search.contains("FILTER"), "{search}");
}

#[test]
fn inspector_blocked_completed_and_boundary_layouts_render_without_overlap() {
  let mut app = app();
  app.begin_run();
  app.apply(RunEvent::Message("controller event".into()));
  app.dispatch(Action::Inspect);
  let inspector = rendered(&app, 90, 28);
  assert!(inspector.contains("CONTROLLER"), "{inspector}");
  app.dispatch(Action::Cancel);
  app.dispatch(Action::Exit);
  let confirmation = rendered(&app, 90, 28);
  assert!(confirmation.contains("Keep running"), "{confirmation}");
  assert!(confirmation.contains("Stop run"), "{confirmation}");
  app.dispatch(Action::Cancel);

  for status in [RunStatus::Blocked, RunStatus::Done] {
    let mut state = State::fresh();
    state.status = status.clone();
    state.phase = Phase::Complete;
    state.last_summary = "Terminal outcome".into();
    app.apply(RunEvent::Finished(state));
    let output = rendered(&app, 110, 32);
    assert!(
      output.contains(if status == RunStatus::Done {
        "RUN COMPLETE"
      } else {
        "BLOCKED"
      }),
      "{output}"
    );
  }
  for size in [(140, 40), (110, 32), (90, 28), (60, 22), (42, 12)] {
    let _ = rendered(&app, size.0, size.1);
  }
  let tiny = rendered(&app, 42, 12);
  assert!(tiny.contains("/ search   Enter inspect"), "{tiny}");
}
