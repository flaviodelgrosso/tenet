use std::{
  collections::BTreeMap,
  io,
  path::{Path, PathBuf},
  sync::Arc,
  time::{Duration, Instant},
};

use anyhow::Result;
use chrono::{DateTime, Local};
use crossterm::{
  event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
  execute,
  terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
  backend::CrosstermBackend,
  layout::{Alignment, Constraint, Direction, Layout, Rect},
  style::{Color, Modifier, Style},
  text::{Line, Span},
  widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap},
  Frame, Terminal,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
  backend::AgentBackend,
  controller::Controller,
  events::{EventSink, RunEvent},
  model::{
    Phase, ReconcileResult, RepositoryChange, Requirement, RequirementAssessment,
    RequirementStatus, RunStatus, State, VerificationReport, WorkerEvent, WorkerRole,
  },
  store,
};

const MAX_ACTIVITIES: usize = 2_000;
const MAX_TEXT_BYTES: usize = 500_000;
const MAX_TIMELINE: usize = 2_000;
const QUIET: Color = Color::DarkGray;

pub async fn idle(cwd: PathBuf, backend: Arc<dyn AgentBackend>) -> Result<State> {
  open(cwd, backend, false).await
}

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
  let initial_state = store::read_state(&cwd).await?;
  let initial_catalog = store::read_catalog(&cwd).await?;
  let (tx, mut rx) = mpsc::unbounded_channel();
  let cancel = CancellationToken::new();
  let mut app = App::new(project_name, initial_state);
  if let Some(catalog) = initial_catalog {
    app.catalog = catalog.requirements;
  }
  let mut task = start_immediately.then(|| {
    app.begin_run();
    spawn_run(&cwd, &backend, &tx, &cancel)
  });

  let mut terminal = setup_terminal()?;
  let result: Result<State> = async {
    loop {
      while let Ok(run_event) = rx.try_recv() {
        app.apply(run_event);
      }

      terminal.draw(|frame| draw(frame, &mut app))?;

      if event::poll(Duration::from_millis(60))? {
        if let Event::Key(key) = event::read()? {
          match handle_key(&mut app, key, &cancel) {
            TuiAction::None => {}
            TuiAction::Start => {
              app.begin_run();
              task = Some(spawn_run(&cwd, &backend, &tx, &cancel));
            }
            TuiAction::Exit => return Ok(app.state.clone()),
          }
        }
      }

      if task
        .as_ref()
        .is_some_and(tokio::task::JoinHandle::is_finished)
      {
        while let Ok(run_event) = rx.try_recv() {
          app.apply(run_event);
        }
        let completed = task.take().expect("finished task is present");
        return completed.await?;
      }
    }
  }
  .await;

  let restore_result = restore_terminal(&mut terminal);
  match result {
    Ok(state) => {
      restore_result?;
      Ok(state)
    }
    Err(error) => {
      let _ = restore_result;
      Err(error)
    }
  }
}

fn spawn_run(
  cwd: &Path,
  backend: &Arc<dyn AgentBackend>,
  tx: &mpsc::UnboundedSender<RunEvent>,
  cancel: &CancellationToken,
) -> tokio::task::JoinHandle<Result<State>> {
  let controller = Controller::new(
    cwd.to_path_buf(),
    backend.clone(),
    EventSink::new(Some(tx.clone())),
  );
  let task_cancel = cancel.clone();
  tokio::spawn(async move { controller.run(task_cancel).await })
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
  enable_raw_mode()?;
  let mut stdout = io::stdout();
  if let Err(error) = execute!(stdout, EnterAlternateScreen) {
    let _ = disable_raw_mode();
    return Err(error.into());
  }

  let backend = CrosstermBackend::new(stdout);
  let mut terminal = match Terminal::new(backend) {
    Ok(terminal) => terminal,
    Err(error) => {
      let _ = execute!(io::stdout(), LeaveAlternateScreen);
      let _ = disable_raw_mode();
      return Err(error.into());
    }
  };
  if let Err(error) = terminal.clear() {
    let _ = restore_terminal(&mut terminal);
    return Err(error.into());
  }
  Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
  let cursor_result = terminal.show_cursor();
  let screen_result = execute!(terminal.backend_mut(), LeaveAlternateScreen);
  let raw_mode_result = disable_raw_mode();
  cursor_result?;
  screen_result?;
  raw_mode_result?;
  Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
  Overview,
  Worker,
  Requirements,
  Verify,
  Timeline,
}

impl View {
  fn index(self) -> usize {
    match self {
      Self::Overview => 0,
      Self::Worker => 1,
      Self::Requirements => 2,
      Self::Verify => 3,
      Self::Timeline => 4,
    }
  }

  fn from_index(index: usize) -> Self {
    match index % 5 {
      0 => Self::Overview,
      1 => Self::Worker,
      2 => Self::Requirements,
      3 => Self::Verify,
      _ => Self::Timeline,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerUiStatus {
  Running,
  Succeeded,
  Failed,
}

struct WorkerSession {
  role: WorkerRole,
  started_at: String,
  finished_at: Option<String>,
  skills: Vec<String>,
  activities: Vec<WorkerActivity>,
  status: WorkerUiStatus,
  message: Option<String>,
  text_bytes: usize,
}

enum WorkerActivity {
  Text { at: String, text: String },
  Tool(ToolInvocation),
  Error { at: String, message: String },
}

struct ToolInvocation {
  name: String,
  args: serde_json::Value,
  started_at: String,
  finished_at: Option<String>,
  output: Option<String>,
  is_error: bool,
  expanded: bool,
}

struct VerificationAttempt {
  report: VerificationReport,
  expanded: Vec<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimelineKind {
  State,
  Worker,
  Verification,
  Error,
  Work,
}

struct TimelineEntry {
  timestamp: String,
  kind: TimelineKind,
  label: String,
  detail: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimelineFilter {
  All,
  Workers,
  Verification,
  State,
  Errors,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StepOutcome {
  Active,
  Pass,
  Fail,
}

struct LoopStep {
  phase: Phase,
  outcome: StepOutcome,
}

struct App {
  project_name: String,
  started: Instant,
  state: State,
  catalog: Vec<Requirement>,
  assessments: BTreeMap<String, RequirementAssessment>,
  workers: Vec<WorkerSession>,
  verifications: Vec<VerificationAttempt>,
  verification_index: usize,
  repository_changes: Vec<RepositoryChange>,
  timeline: Vec<TimelineEntry>,
  cycles: BTreeMap<u32, Vec<LoopStep>>,
  view: View,
  selected: usize,
  scroll: u16,
  follow: bool,
  requirement_detail: bool,
  timeline_filter: TimelineFilter,
  search: Option<String>,
  show_help: bool,
  show_diff: bool,
  run_active: bool,
  finished: bool,
}

impl App {
  fn new(project_name: String, state: State) -> Self {
    Self {
      project_name,
      started: Instant::now(),
      state,
      catalog: Vec::new(),
      assessments: BTreeMap::new(),
      workers: Vec::new(),
      verifications: Vec::new(),
      verification_index: 0,
      repository_changes: Vec::new(),
      timeline: Vec::new(),
      cycles: BTreeMap::new(),
      view: View::Overview,
      selected: 0,
      scroll: 0,
      follow: true,
      requirement_detail: false,
      timeline_filter: TimelineFilter::All,
      search: None,
      show_help: false,
      show_diff: false,
      run_active: false,
      finished: false,
    }
  }

  fn begin_run(&mut self) {
    self.started = Instant::now();
    self.assessments.clear();
    self.workers.clear();
    self.verifications.clear();
    self.verification_index = 0;
    self.repository_changes.clear();
    self.timeline.clear();
    self.cycles.clear();
    self.selected = 0;
    self.scroll = 0;
    self.follow = true;
    self.run_active = true;
    self.finished = false;
  }

  fn apply(&mut self, event: RunEvent) {
    match event {
      RunEvent::State(state) => self.apply_state(state),
      RunEvent::Catalog(catalog) => self.catalog = catalog.requirements,
      RunEvent::Message(message) => {
        let kind = if message.to_ascii_lowercase().contains("error") {
          TimelineKind::Error
        } else {
          TimelineKind::State
        };
        self.push_timeline(now_text(), kind, "CONTROLLER", message);
      }
      RunEvent::Worker(event) => self.apply_worker(event),
      RunEvent::Reconcile(result) => self.apply_reconcile(result),
      RunEvent::Verification(report) => self.apply_verification(report),
      RunEvent::RepositoryChanges(changes) => self.repository_changes = changes,
      RunEvent::Finished(state) => {
        self.apply_state(state);
        self.run_active = false;
        self.finished = true;
      }
    }
  }

  fn apply_state(&mut self, state: State) {
    if state.phase != self.state.phase || state.cycle != self.state.cycle {
      self.record_phase(state.cycle, &state.phase);
      let kind = if matches!(state.status, RunStatus::Failed | RunStatus::Blocked) {
        TimelineKind::Error
      } else {
        TimelineKind::State
      };
      self.push_timeline(
        state.updated_at.clone(),
        kind,
        phase_label(&state.phase),
        state.last_summary.clone(),
      );
    }
    self.state = state;
  }

  fn record_phase(&mut self, cycle: u32, phase: &Phase) {
    let steps = self.cycles.entry(cycle).or_default();
    if let Some(last) = steps.last_mut() {
      if last.outcome == StepOutcome::Active {
        last.outcome = StepOutcome::Pass;
      }
    }
    if !matches!(
      phase,
      Phase::Initialized | Phase::Architecting | Phase::Complete
    ) && steps.last().map(|step| &step.phase) != Some(phase)
    {
      steps.push(LoopStep {
        phase: phase.clone(),
        outcome: StepOutcome::Active,
      });
    }
  }

  fn apply_worker(&mut self, event: WorkerEvent) {
    match event {
      WorkerEvent::Start { role, at, skills } => {
        self.workers.push(WorkerSession {
          role,
          started_at: at.clone(),
          finished_at: None,
          skills,
          activities: Vec::new(),
          status: WorkerUiStatus::Running,
          message: None,
          text_bytes: 0,
        });
        self.push_timeline(at, TimelineKind::Worker, role_label(role), "started".into());
      }
      WorkerEvent::Text { role, at, delta } => {
        let clean = sanitize_terminal_text(&delta);
        let session = self.session_mut(role, &at);
        if let Some(WorkerActivity::Text { text, .. }) = session.activities.last_mut() {
          text.push_str(&clean);
        } else {
          session.activities.push(WorkerActivity::Text {
            at,
            text: clean.clone(),
          });
        }
        session.text_bytes = session.text_bytes.saturating_add(clean.len());
        session.bound_output();
      }
      WorkerEvent::ToolStart {
        role,
        at,
        tool_name,
        args,
      } => {
        let session = self.session_mut(role, &at);
        session
          .activities
          .push(WorkerActivity::Tool(ToolInvocation {
            name: tool_name,
            args,
            started_at: at,
            finished_at: None,
            output: None,
            is_error: false,
            expanded: false,
          }));
        session.bound_output();
      }
      WorkerEvent::ToolEnd {
        role,
        at,
        tool_name,
        is_error,
        output,
      } => {
        let session = self.session_mut(role, &at);
        if let Some(tool) = session.activities.iter_mut().rev().find_map(|activity| {
          let WorkerActivity::Tool(tool) = activity else {
            return None;
          };
          (tool.finished_at.is_none() && tool.name == tool_name).then_some(tool)
        }) {
          tool.finished_at = Some(at.clone());
          tool.output = output.map(|value| sanitize_terminal_text(&value));
          tool.is_error = is_error;
          tool.expanded = is_error;
        } else {
          session
            .activities
            .push(WorkerActivity::Tool(ToolInvocation {
              name: tool_name,
              args: serde_json::Value::Null,
              started_at: at.clone(),
              finished_at: Some(at.clone()),
              output: output.map(|value| sanitize_terminal_text(&value)),
              is_error,
              expanded: is_error,
            }));
        }
        if is_error {
          self.push_timeline(
            at,
            TimelineKind::Error,
            role_label(role),
            "tool failed".into(),
          );
        }
      }
      WorkerEvent::End {
        role,
        at,
        ok,
        message,
      } => {
        let session = self.session_mut(role, &at);
        session.finished_at = Some(at.clone());
        session.status = if ok {
          WorkerUiStatus::Succeeded
        } else {
          WorkerUiStatus::Failed
        };
        session.message = message.clone();
        if let Some(message) = message.filter(|_| !ok) {
          session.activities.push(WorkerActivity::Error {
            at: at.clone(),
            message,
          });
        }
        self.push_timeline(
          at,
          if ok {
            TimelineKind::Worker
          } else {
            TimelineKind::Error
          },
          role_label(role),
          if ok { "completed" } else { "failed" }.into(),
        );
      }
    }
    if self.follow {
      self.selected = self.current_activities_len().saturating_sub(1);
    }
  }

  fn session_mut(&mut self, role: WorkerRole, at: &str) -> &mut WorkerSession {
    let needs_session = self
      .workers
      .last()
      .is_none_or(|session| session.role != role || session.finished_at.is_some());
    if needs_session {
      self.workers.push(WorkerSession {
        role,
        started_at: at.to_owned(),
        finished_at: None,
        skills: Vec::new(),
        activities: Vec::new(),
        status: WorkerUiStatus::Running,
        message: None,
        text_bytes: 0,
      });
    }
    self
      .workers
      .last_mut()
      .expect("worker session was inserted")
  }

  fn apply_reconcile(&mut self, result: ReconcileResult) {
    for assessment in result.requirements {
      self.assessments.insert(assessment.id.clone(), assessment);
    }
    let detail = result
      .next_work_unit
      .as_ref()
      .map(|work| format!("{} selected · {}", work.id, work.title))
      .unwrap_or(result.summary);
    self.push_timeline(now_text(), TimelineKind::Work, "RECONCILE", detail);
  }

  fn apply_verification(&mut self, report: VerificationReport) {
    let passed = report.passed;
    let failed_command = report
      .commands
      .iter()
      .find(|command| command.exit_code != Some(0) || command.timed_out)
      .map(|command| command.command.clone());
    if let Some(steps) = self.cycles.get_mut(&self.state.cycle) {
      if let Some(step) = steps
        .iter_mut()
        .rev()
        .find(|step| matches!(step.phase, Phase::Verifying))
      {
        step.outcome = if passed {
          StepOutcome::Pass
        } else {
          StepOutcome::Fail
        };
      }
    }
    let detail = failed_command.unwrap_or_else(|| "all deterministic gates".into());
    self.push_timeline(
      report.finished_at.clone(),
      TimelineKind::Verification,
      "VERIFY",
      format!("{} · {detail}", if passed { "PASS" } else { "FAIL" }),
    );
    self.verifications.push(VerificationAttempt {
      expanded: report
        .commands
        .iter()
        .map(|command| command.exit_code != Some(0) || command.timed_out)
        .collect(),
      report,
    });
    self.verification_index = self.verifications.len().saturating_sub(1);
    if self.view == View::Verify && self.follow {
      self.selected = 0;
    }
  }

  fn push_timeline(
    &mut self,
    timestamp: String,
    kind: TimelineKind,
    label: impl Into<String>,
    detail: String,
  ) {
    self.timeline.push(TimelineEntry {
      timestamp,
      kind,
      label: label.into(),
      detail,
    });
    if self.timeline.len() > MAX_TIMELINE {
      self.timeline.drain(..MAX_TIMELINE / 4);
    }
  }

  fn switch_view(&mut self, view: View) {
    self.view = view;
    self.selected = 0;
    self.scroll = 0;
    self.requirement_detail = false;
    if view == View::Worker && self.follow {
      self.selected = self.current_activities_len().saturating_sub(1);
    }
  }

  fn current_worker(&self) -> Option<&WorkerSession> {
    self.workers.last()
  }

  fn current_worker_mut(&mut self) -> Option<&mut WorkerSession> {
    self.workers.last_mut()
  }

  fn current_activities_len(&self) -> usize {
    self
      .current_worker()
      .map_or(0, |worker| worker.activities.len())
  }

  fn search_matches(&self, text: &str) -> bool {
    self.search.as_deref().is_none_or(|query| {
      text
        .to_ascii_lowercase()
        .contains(&query.to_ascii_lowercase())
    })
  }
}

impl WorkerSession {
  fn bound_output(&mut self) {
    while self.activities.len() > MAX_ACTIVITIES || self.text_bytes > MAX_TEXT_BYTES {
      let removed = self.activities.remove(0);
      if let WorkerActivity::Text { text, .. } = removed {
        self.text_bytes = self.text_bytes.saturating_sub(text.len());
      }
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TuiAction {
  None,
  Start,
  Exit,
}

fn handle_key(app: &mut App, key: KeyEvent, cancel: &CancellationToken) -> TuiAction {
  if let Some(query) = &mut app.search {
    match key.code {
      KeyCode::Esc | KeyCode::Enter => app.search = None,
      KeyCode::Backspace => {
        query.pop();
      }
      KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => query.push(ch),
      _ => {}
    }
    return TuiAction::None;
  }
  if app.show_help {
    app.show_help = false;
    return TuiAction::None;
  }
  if app.show_diff {
    if matches!(key.code, KeyCode::Esc | KeyCode::Char('d') | KeyCode::Enter) {
      app.show_diff = false;
    }
    return TuiAction::None;
  }
  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
    if !app.run_active {
      return TuiAction::Exit;
    }
    cancel.cancel();
    app.push_timeline(
      now_text(),
      TimelineKind::State,
      "CONTROLLER",
      "stop requested".into(),
    );
    return TuiAction::None;
  }
  match key.code {
    KeyCode::Char('q') => {
      if !app.run_active {
        return TuiAction::Exit;
      }
      cancel.cancel();
      app.push_timeline(
        now_text(),
        TimelineKind::State,
        "CONTROLLER",
        "stop requested".into(),
      );
    }
    KeyCode::Char('r') if !app.run_active => return TuiAction::Start,
    KeyCode::Char('1') => app.switch_view(View::Overview),
    KeyCode::Char('2') => app.switch_view(View::Worker),
    KeyCode::Char('3') => app.switch_view(View::Requirements),
    KeyCode::Char('4') => app.switch_view(View::Verify),
    KeyCode::Char('5') => app.switch_view(View::Timeline),
    KeyCode::Tab => app.switch_view(View::from_index(app.view.index() + 1)),
    KeyCode::BackTab => app.switch_view(View::from_index(app.view.index() + 4)),
    KeyCode::Char('?') => app.show_help = true,
    KeyCode::Char('d') if !app.repository_changes.is_empty() => app.show_diff = true,
    KeyCode::Left | KeyCode::Char('[') if app.view == View::Verify => {
      app.verification_index = app.verification_index.saturating_sub(1);
      app.selected = 0;
    }
    KeyCode::Right | KeyCode::Char(']') if app.view == View::Verify => {
      if app.verification_index + 1 < app.verifications.len() {
        app.verification_index += 1;
        app.selected = 0;
      }
    }
    KeyCode::Char('/') => app.search = Some(String::new()),
    KeyCode::Char('f') => {
      app.follow = !app.follow;
      if app.follow && app.view == View::Worker {
        app.selected = app.current_activities_len().saturating_sub(1);
      }
    }
    KeyCode::Char('a') if app.view == View::Timeline => app.timeline_filter = TimelineFilter::All,
    KeyCode::Char('w') if app.view == View::Timeline => {
      app.timeline_filter = TimelineFilter::Workers
    }
    KeyCode::Char('v') if app.view == View::Timeline => {
      app.timeline_filter = TimelineFilter::Verification
    }
    KeyCode::Char('s') if app.view == View::Timeline => app.timeline_filter = TimelineFilter::State,
    KeyCode::Char('e') if app.view == View::Timeline => {
      app.timeline_filter = TimelineFilter::Errors
    }
    KeyCode::Up | KeyCode::Char('k') => move_selection(app, -1),
    KeyCode::Down | KeyCode::Char('j') => move_selection(app, 1),
    KeyCode::PageUp => {
      app.scroll = app.scroll.saturating_sub(12);
      app.selected = app.selected.saturating_sub(12);
      app.follow = false;
    }
    KeyCode::PageDown => {
      app.scroll = app.scroll.saturating_add(12);
      app.selected = app.selected.saturating_add(12);
      app.follow = false;
    }
    KeyCode::Home | KeyCode::Char('g') => {
      app.scroll = 0;
      app.selected = 0;
      app.follow = false;
    }
    KeyCode::End | KeyCode::Char('G') => {
      app.follow = true;
      app.selected = match app.view {
        View::Worker => app.current_activities_len().saturating_sub(1),
        View::Requirements => app.catalog.len().saturating_sub(1),
        View::Verify => app
          .verifications
          .get(app.verification_index)
          .map_or(0, |attempt| attempt.report.commands.len().saturating_sub(1)),
        _ => app.selected,
      };
    }
    KeyCode::Enter | KeyCode::Char(' ') => toggle_selected(app),
    KeyCode::Esc => {
      if app.requirement_detail {
        app.requirement_detail = false;
      } else if !app.run_active {
        return TuiAction::Exit;
      }
    }
    _ => {}
  }
  TuiAction::None
}

fn move_selection(app: &mut App, delta: isize) {
  app.follow = false;
  if matches!(app.view, View::Overview | View::Timeline) {
    app.scroll = if delta < 0 {
      app.scroll.saturating_sub(1)
    } else {
      app.scroll.saturating_add(1)
    };
    return;
  }
  let limit = match app.view {
    View::Worker => app.current_activities_len(),
    View::Requirements => app.catalog.len(),
    View::Verify => app
      .verifications
      .get(app.verification_index)
      .map_or(0, |attempt| attempt.report.commands.len()),
    _ => 0,
  };
  if delta < 0 {
    app.selected = app.selected.saturating_sub(1);
  } else if app.selected + 1 < limit {
    app.selected += 1;
  }
}

fn toggle_selected(app: &mut App) {
  match app.view {
    View::Worker => {
      let selected = app.selected;
      if let Some(WorkerActivity::Tool(tool)) = app
        .current_worker_mut()
        .and_then(|worker| worker.activities.get_mut(selected))
      {
        tool.expanded = !tool.expanded;
      }
    }
    View::Requirements if !app.catalog.is_empty() => {
      app.requirement_detail = !app.requirement_detail
    }
    View::Verify => {
      let selected = app.selected;
      if let Some(expanded) = app
        .verifications
        .get_mut(app.verification_index)
        .and_then(|attempt| attempt.expanded.get_mut(selected))
      {
        *expanded = !*expanded;
      }
    }
    _ => {}
  }
}

fn draw(frame: &mut Frame, app: &mut App) {
  let area = frame.area();
  if area.width < 42 || area.height < 12 {
    frame.render_widget(
      Paragraph::new("loops mission control\n\nTerminal too small. Need at least 42 × 12.")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::Yellow)),
      area,
    );
    return;
  }
  let chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
      Constraint::Length(2),
      Constraint::Min(7),
      Constraint::Length(2),
    ])
    .split(area);

  draw_header(frame, app, chunks[0]);
  match app.view {
    View::Overview => draw_overview(frame, app, chunks[1]),
    View::Worker => draw_worker(frame, app, chunks[1]),
    View::Requirements => draw_requirements(frame, app, chunks[1]),
    View::Verify => draw_verify(frame, app, chunks[1]),
    View::Timeline => draw_timeline(frame, app, chunks[1]),
  }
  draw_footer(frame, app, chunks[2]);
  if app.show_help {
    draw_help(frame, area);
  }
  if app.show_diff {
    draw_changes_overlay(frame, app, area);
  }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
  let counts = &app.state.requirement_counts;
  let left = Line::from(vec![
    Span::styled(" loops", Style::default().add_modifier(Modifier::BOLD)),
    Span::styled(" · ", Style::default().fg(QUIET)),
    Span::raw(&app.project_name),
  ]);
  let (run_status, run_color) = if app.run_active {
    (
      status_name(&app.state.status),
      status_color(&app.state.status),
    )
  } else {
    ("READY", Color::Cyan)
  };
  let elapsed = if app.run_active {
    format!(" · {}", format_duration(app.started.elapsed()))
  } else {
    String::new()
  };
  let right = Line::from(vec![
    Span::styled(
      run_status,
      Style::default().fg(run_color).add_modifier(Modifier::BOLD),
    ),
    Span::styled(
      format!(
        " · {} · cycle {} · {}/{}{} ",
        phase_label(&app.state.phase),
        app.state.cycle,
        counts.satisfied,
        counts.total,
        elapsed
      ),
      Style::default().fg(Color::Gray),
    ),
  ]);
  let cols =
    Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).split(area);
  frame.render_widget(Paragraph::new(left), cols[0]);
  frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), cols[1]);
}

fn draw_overview(frame: &mut Frame, app: &App, area: Rect) {
  if area.width >= 96 {
    let cols =
      Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).split(area);
    draw_overview_primary(frame, app, cols[0]);
    draw_overview_secondary(frame, app, cols[1]);
  } else {
    let rows =
      Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area);
    draw_overview_primary(frame, app, rows[0]);
    draw_overview_secondary(frame, app, rows[1]);
  }
}

fn draw_overview_primary(frame: &mut Frame, app: &App, area: Rect) {
  let rows = Layout::vertical([
    Constraint::Length(5),
    Constraint::Length(6),
    Constraint::Min(5),
  ])
  .split(area);
  let counts = &app.state.requirement_counts;
  let ratio = if counts.total == 0 {
    0.0
  } else {
    counts.satisfied as f64 / counts.total as f64
  };
  frame.render_widget(
    Gauge::default()
      .block(section_block(" SPEC PROGRESS "))
      .gauge_style(Style::default().fg(Color::Green))
      .ratio(ratio)
      .label(format!(
        "{} / {} requirements",
        counts.satisfied, counts.total
      )),
    rows[0],
  );

  let mut current = vec![section_heading("CURRENT")];
  if !app.run_active {
    current.push(Line::from(Span::styled(
      "READY · press r to start or resume the autonomous run",
      Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD),
    )));
    current.push(Line::from(Span::styled(
      &app.state.last_summary,
      Style::default().fg(QUIET),
    )));
  } else if let Some(work) = &app.state.current_work_unit {
    current.push(Line::from(vec![Span::styled(
      format!("{} · {}", phase_label(&app.state.phase), work.id),
      Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD),
    )]));
    current.push(Line::from(Span::styled(
      &work.title,
      Style::default().add_modifier(Modifier::BOLD),
    )));
    current.push(Line::from(work.requirement_ids.join("  ")));
  } else {
    current.push(Line::from(Span::styled(
      &app.state.last_summary,
      Style::default().fg(Color::Gray),
    )));
  }
  frame.render_widget(Paragraph::new(current).wrap(Wrap { trim: true }), rows[1]);

  let mut recent = vec![section_heading("RECENT WORK")];
  for completed in app.state.completed_work_units.iter().rev().take(4).rev() {
    recent.push(Line::from(vec![
      Span::styled("✓ ", Style::default().fg(Color::Green)),
      Span::styled(
        &completed.work_unit.id,
        Style::default().add_modifier(Modifier::BOLD),
      ),
      Span::raw(format!("  {}", completed.work_unit.title)),
    ]));
  }
  if let Some(work) = &app.state.current_work_unit {
    recent.push(Line::from(vec![
      Span::styled("● ", Style::default().fg(Color::Cyan)),
      Span::styled(
        &work.id,
        Style::default()
          .fg(Color::Cyan)
          .add_modifier(Modifier::BOLD),
      ),
      Span::raw(format!("  {}", work.title)),
    ]));
  }
  if recent.len() == 1 {
    recent.push(Line::from(Span::styled(
      "No completed work units yet",
      Style::default().fg(QUIET),
    )));
  }
  frame.render_widget(Paragraph::new(recent).wrap(Wrap { trim: true }), rows[2]);
}

fn draw_overview_secondary(frame: &mut Frame, app: &App, area: Rect) {
  let rows = Layout::vertical([
    Constraint::Length(6),
    Constraint::Length(6),
    Constraint::Min(5),
  ])
  .split(area);
  let counts = &app.state.requirement_counts;
  let requirements = vec![
    section_heading("REQUIREMENTS"),
    metric_line("✓", Color::Green, counts.satisfied, "satisfied"),
    metric_line("◐", Color::Yellow, counts.partial, "partial"),
    metric_line("○", QUIET, counts.missing, "missing"),
  ];
  frame.render_widget(Paragraph::new(requirements), rows[0]);

  let last_verify = app.verifications.last();
  let verification = vec![
    section_heading("VERIFICATION"),
    label_value(
      "Last run",
      last_verify.map_or("—", |attempt| {
        if attempt.report.passed {
          "PASS"
        } else {
          "FAIL"
        }
      }),
      last_verify.map_or(QUIET, |attempt| {
        if attempt.report.passed {
          Color::Green
        } else {
          Color::Red
        }
      }),
    ),
    label_value(
      "Attempts",
      &app.verifications.len().to_string(),
      Color::Gray,
    ),
    label_value("Cycle", &app.state.cycle.to_string(), Color::Gray),
  ];
  frame.render_widget(Paragraph::new(verification), rows[1]);

  let mut loop_lines = vec![section_heading("ENGINEERING LOOP")];
  for (cycle, steps) in app.cycles.iter().rev().take(3).rev() {
    let mut spans = vec![Span::styled(
      format!("Cycle {cycle:<3} "),
      Style::default().fg(QUIET),
    )];
    for (index, step) in steps.iter().enumerate() {
      if index > 0 {
        spans.push(Span::styled(" → ", Style::default().fg(QUIET)));
      }
      let (mark, color) = step_mark(step.outcome);
      spans.push(Span::styled(
        format!("{} {mark}", phase_short(&step.phase)),
        Style::default().fg(color),
      ));
    }
    loop_lines.push(Line::from(spans));
  }
  if loop_lines.len() == 1 {
    loop_lines.push(Line::from(Span::styled(
      "Waiting for first cycle",
      Style::default().fg(QUIET),
    )));
  }
  frame.render_widget(
    Paragraph::new(loop_lines).wrap(Wrap { trim: true }),
    rows[2],
  );
}

fn draw_worker(frame: &mut Frame, app: &mut App, area: Rect) {
  let Some(worker) = app.current_worker() else {
    frame.render_widget(
      empty_state("WORKER", "Waiting for the first fresh worker"),
      area,
    );
    return;
  };
  let context_height = if area.height >= 22 { 6 } else { 3 };
  let rows = Layout::vertical([
    Constraint::Length(4),
    Constraint::Min(5),
    Constraint::Length(context_height),
  ])
  .split(area);
  let elapsed = duration_between(&worker.started_at, worker.finished_at.as_deref());
  let work = app.state.current_work_unit.as_ref();
  let heading = vec![
    Line::from(vec![
      Span::styled(
        format!("{} {}", role_symbol(worker.role), role_label(worker.role)),
        Style::default()
          .fg(role_color(worker.role))
          .add_modifier(Modifier::BOLD),
      ),
      Span::styled(
        format!(
          " · {} · {}",
          worker_status(worker.status),
          elapsed.as_deref().unwrap_or("elapsed unavailable")
        ),
        Style::default().fg(QUIET),
      ),
    ]),
    Line::from(work.map_or_else(
      || app.state.last_summary.clone(),
      |unit| format!("{} · {}", unit.id, unit.title),
    )),
    Line::from(work.map_or_else(String::new, |unit| unit.requirement_ids.join("  "))),
  ];
  frame.render_widget(Paragraph::new(heading), rows[0]);

  let selected = app.selected.min(worker.activities.len().saturating_sub(1));
  let items: Vec<ListItem> = worker
    .activities
    .iter()
    .enumerate()
    .filter(|(_, activity)| activity_matches(app, activity))
    .map(|(index, activity)| activity_item(activity, index == selected))
    .collect();
  let mut list_state = ListState::default();
  if !items.is_empty() {
    list_state.select(Some(selected.min(items.len().saturating_sub(1))));
  }
  let list = List::new(if items.is_empty() {
    vec![ListItem::new("Waiting for structured activity…")]
  } else {
    items
  })
  .block(section_block(" ACTIVITY "))
  .highlight_style(Style::default().bg(Color::Rgb(30, 38, 46)));
  frame.render_stateful_widget(list, rows[1], &mut list_state);

  let mut context = vec![section_heading("CONTEXT")];
  context.push(Line::from(vec![
    Span::styled("skills   ", Style::default().fg(QUIET)),
    Span::raw(if worker.skills.is_empty() {
      "—".into()
    } else {
      worker.skills.join(", ")
    }),
  ]));
  context.push(Line::from(vec![
    Span::styled("changes  ", Style::default().fg(QUIET)),
    Span::raw(format_changes_summary(&app.repository_changes)),
  ]));
  if context_height >= 6 {
    context.push(Line::from(vec![
      Span::styled("tools    ", Style::default().fg(QUIET)),
      Span::raw("structured invocations · Enter/Space expands output"),
    ]));
  }
  frame.render_widget(Paragraph::new(context).wrap(Wrap { trim: true }), rows[2]);
}

fn activity_matches(app: &App, activity: &WorkerActivity) -> bool {
  match activity {
    WorkerActivity::Text { text, .. } => app.search_matches(text),
    WorkerActivity::Tool(tool) => {
      app.search_matches(&format!("{} {}", tool.name, compact_args(&tool.args)))
    }
    WorkerActivity::Error { message, .. } => app.search_matches(message),
  }
}

fn activity_item(activity: &WorkerActivity, selected: bool) -> ListItem<'static> {
  match activity {
    WorkerActivity::Text { at, text } => {
      let excerpt = text.trim();
      ListItem::new(vec![
        Line::from(vec![
          Span::styled(time_of(at), Style::default().fg(QUIET)),
          Span::styled("  REASONING", Style::default().fg(QUIET)),
        ]),
        Line::from(Span::styled(
          excerpt.to_owned(),
          Style::default().fg(Color::Gray),
        )),
      ])
    }
    WorkerActivity::Tool(tool) => {
      let running = tool.finished_at.is_none();
      let (mark, color) = if running {
        ("●", Color::Cyan)
      } else if tool.is_error {
        ("✗", Color::Red)
      } else {
        ("✓", Color::Green)
      };
      let mut lines = vec![Line::from(vec![
        Span::styled(format!("{mark} "), Style::default().fg(color)),
        Span::styled(
          tool.name.clone(),
          Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
          tool_duration(tool).map_or_else(String::new, |duration| format!("  {duration}")),
          Style::default().fg(QUIET),
        ),
        Span::styled(
          if selected { "  selected" } else { "" },
          Style::default().fg(QUIET),
        ),
      ])];
      let args = compact_args(&tool.args);
      if !args.is_empty() {
        lines.push(Line::from(Span::styled(
          format!("   {args}"),
          Style::default().fg(Color::Gray),
        )));
      }
      if tool.expanded {
        if let Some(output) = &tool.output {
          for line in output.lines().take(24) {
            lines.push(Line::from(Span::styled(
              format!("   {line}"),
              Style::default().fg(if tool.is_error { Color::Red } else { QUIET }),
            )));
          }
        }
      } else if tool.output.is_some() {
        lines.push(Line::from(Span::styled(
          "   output collapsed · Enter to inspect",
          Style::default().fg(QUIET),
        )));
      }
      ListItem::new(lines)
    }
    WorkerActivity::Error { at, message } => ListItem::new(vec![
      Line::from(vec![
        Span::styled(
          "✗ ERROR",
          Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", time_of(at)), Style::default().fg(QUIET)),
      ]),
      Line::from(message.clone()),
    ]),
  }
}

fn draw_requirements(frame: &mut Frame, app: &mut App, area: Rect) {
  if app.requirement_detail {
    draw_requirement_detail(frame, app, area);
    return;
  }
  if app.catalog.is_empty() {
    frame.render_widget(
      empty_state("REQUIREMENTS", "Waiting for the architect catalog"),
      area,
    );
    return;
  }
  let items: Vec<ListItem> = app
    .catalog
    .iter()
    .filter(|requirement| app.search_matches(&format!("{} {}", requirement.id, requirement.title)))
    .map(|requirement| {
      let status = app
        .assessments
        .get(&requirement.id)
        .map(|item| &item.status);
      let (mark, color) = requirement_mark(status);
      ListItem::new(Line::from(vec![
        Span::styled(format!(" {mark}  "), Style::default().fg(color)),
        Span::styled(
          format!("{:<10}", requirement.id),
          Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(&requirement.title),
      ]))
    })
    .collect();
  let mut state = ListState::default();
  state.select(Some(app.selected.min(items.len().saturating_sub(1))));
  let title = format!(
    " REQUIREMENTS  {} / {} ",
    app.state.requirement_counts.satisfied, app.state.requirement_counts.total
  );
  frame.render_stateful_widget(
    List::new(items)
      .block(section_block(&title))
      .highlight_symbol("›")
      .highlight_style(Style::default().bg(Color::Rgb(30, 38, 46))),
    area,
    &mut state,
  );
}

fn draw_requirement_detail(frame: &mut Frame, app: &App, area: Rect) {
  let Some(requirement) = app.catalog.get(app.selected) else {
    return;
  };
  let assessment = app.assessments.get(&requirement.id);
  let (_, color) = requirement_mark(assessment.map(|item| &item.status));
  let status = assessment.map_or("UNASSESSED", |item| requirement_status(&item.status));
  let mut lines = vec![
    Line::from(vec![
      Span::styled(
        format!("{} · {}", requirement.id, requirement.title),
        Style::default().add_modifier(Modifier::BOLD),
      ),
      Span::styled(
        format!("   {status}"),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
      ),
    ]),
    Line::from(""),
    section_heading("REQUIREMENT"),
    Line::from(requirement.description.clone()),
  ];
  if !requirement.acceptance_criteria.is_empty() {
    lines.push(Line::from(""));
    lines.push(section_heading("ACCEPTANCE"));
    for criterion in &requirement.acceptance_criteria {
      lines.push(Line::from(format!("  • {criterion}")));
    }
  }
  if let Some(assessment) = assessment {
    lines.push(Line::from(""));
    lines.push(section_heading("EVIDENCE"));
    if assessment.evidence.is_empty() {
      lines.push(Line::from(Span::styled(
        "  No evidence recorded",
        Style::default().fg(QUIET),
      )));
    }
    for evidence in &assessment.evidence {
      lines.push(Line::from(vec![
        Span::styled("  ✓ ", Style::default().fg(Color::Green)),
        Span::raw(evidence),
      ]));
    }
    if !assessment.gaps.is_empty() {
      lines.push(Line::from(""));
      lines.push(section_heading("GAPS"));
      for gap in &assessment.gaps {
        lines.push(Line::from(vec![
          Span::styled("  ! ", Style::default().fg(Color::Yellow)),
          Span::raw(gap),
        ]));
      }
    }
  }
  if let Some(work) = &app.state.current_work_unit {
    if work.requirement_ids.contains(&requirement.id) {
      lines.push(Line::from(""));
      lines.push(section_heading("CURRENT WORK"));
      lines.push(Line::from(vec![
        Span::styled("  → ", Style::default().fg(Color::Cyan)),
        Span::raw(format!("{} {}", work.id, work.title)),
      ]));
    }
  }
  frame.render_widget(
    Paragraph::new(lines)
      .block(section_block(" REQUIREMENT DETAIL "))
      .wrap(Wrap { trim: true })
      .scroll((app.scroll, 0)),
    area,
  );
}

fn draw_verify(frame: &mut Frame, app: &mut App, area: Rect) {
  let Some(attempt) = app.verifications.get(app.verification_index) else {
    frame.render_widget(
      empty_state("VERIFY", "No deterministic verification attempt yet"),
      area,
    );
    return;
  };
  let rows = Layout::vertical([Constraint::Length(4), Constraint::Min(5)]).split(area);
  let status = if attempt.report.passed {
    "PASS"
  } else {
    "FAIL"
  };
  let color = if attempt.report.passed {
    Color::Green
  } else {
    Color::Red
  };
  let mut heading = vec![Line::from(vec![
    Span::styled(
      format!(
        "VERIFY · attempt {} / {}",
        app.verification_index + 1,
        app.verifications.len()
      ),
      Style::default().add_modifier(Modifier::BOLD),
    ),
    Span::styled(
      format!("   {status}"),
      Style::default().fg(color).add_modifier(Modifier::BOLD),
    ),
    Span::styled(
      format!(
        "   {}",
        time_range(&attempt.report.started_at, &attempt.report.finished_at)
      ),
      Style::default().fg(QUIET),
    ),
  ])];
  if app.verification_index + 1 == app.verifications.len()
    && matches!(app.state.phase, Phase::Repairing)
  {
    heading.push(Line::from(Span::styled(
      "→ REPAIR started from this deterministic evidence",
      Style::default().fg(Color::Yellow),
    )));
  } else if let Some(warning) = attempt.report.warnings.first() {
    heading.push(Line::from(Span::styled(
      format!("! {warning}"),
      Style::default().fg(Color::Yellow),
    )));
  }
  frame.render_widget(Paragraph::new(heading), rows[0]);
  let items: Vec<ListItem> = attempt
    .report
    .commands
    .iter()
    .enumerate()
    .map(|(index, command)| {
      let passed = command.exit_code == Some(0) && !command.timed_out;
      let (mark, command_color) = if command.timed_out {
        ("!", Color::Yellow)
      } else if passed {
        ("✓", Color::Green)
      } else {
        ("✗", Color::Red)
      };
      let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {mark} "), Style::default().fg(command_color)),
        Span::styled(
          command.command.clone(),
          Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
          format!("  {}", format_millis(command.duration_ms)),
          Style::default().fg(QUIET),
        ),
      ])];
      if attempt.expanded.get(index).copied().unwrap_or(false) {
        let output = command.stderr.lines().chain(command.stdout.lines());
        for line in output.take(30) {
          lines.push(Line::from(Span::styled(
            format!("   {line}"),
            Style::default().fg(if passed { QUIET } else { Color::Red }),
          )));
        }
        if command.stderr.is_empty() && command.stdout.is_empty() {
          lines.push(Line::from(Span::styled(
            "   no command output",
            Style::default().fg(QUIET),
          )));
        }
      } else if !command.stderr.is_empty() || !command.stdout.is_empty() {
        lines.push(Line::from(Span::styled(
          "   output collapsed · Enter to inspect",
          Style::default().fg(QUIET),
        )));
      }
      ListItem::new(lines)
    })
    .collect();
  let mut list_state = ListState::default();
  list_state.select(Some(app.selected.min(items.len().saturating_sub(1))));
  frame.render_stateful_widget(
    List::new(items)
      .block(section_block(" DETERMINISTIC GATES "))
      .highlight_style(Style::default().bg(Color::Rgb(30, 38, 46))),
    rows[1],
    &mut list_state,
  );
}

fn draw_timeline(frame: &mut Frame, app: &App, area: Rect) {
  let lines: Vec<Line> = app
    .timeline
    .iter()
    .filter(|entry| timeline_visible(app.timeline_filter, entry.kind, &entry.detail))
    .filter(|entry| app.search_matches(&format!("{} {}", entry.label, entry.detail)))
    .map(|entry| {
      let (mark, color) = timeline_mark(entry.kind, &entry.detail);
      Line::from(vec![
        Span::styled(
          format!("{}  ", time_of(&entry.timestamp)),
          Style::default().fg(QUIET),
        ),
        Span::styled(
          format!("{:<12}", entry.label),
          Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{mark} "), Style::default().fg(color)),
        Span::raw(&entry.detail),
      ])
    })
    .collect();
  let visible = area.height.saturating_sub(2) as usize;
  let scroll = if app.follow {
    lines.len().saturating_sub(visible).min(u16::MAX as usize) as u16
  } else {
    app.scroll
  };
  let filter = match app.timeline_filter {
    TimelineFilter::All => "all",
    TimelineFilter::Workers => "workers",
    TimelineFilter::Verification => "verification",
    TimelineFilter::State => "state",
    TimelineFilter::Errors => "errors",
  };
  frame.render_widget(
    Paragraph::new(if lines.is_empty() {
      vec![Line::from("No matching audit entries")]
    } else {
      lines
    })
    .block(section_block(&format!(" TIMELINE · {filter} ")))
    .wrap(Wrap { trim: true })
    .scroll((scroll, 0)),
    area,
  );
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
  let tabs = [
    "1 Overview",
    "2 Worker",
    "3 Requirements",
    "4 Verify",
    "5 Timeline",
  ];
  let mut spans = Vec::new();
  for (index, tab) in tabs.iter().enumerate() {
    let style = if app.view.index() == index {
      Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
    } else {
      Style::default().fg(QUIET)
    };
    spans.push(Span::styled(format!(" {tab}  "), style));
  }
  let contextual = match app.view {
    View::Overview => "↑↓ scroll   d changes   ? help",
    View::Worker => "↑↓ select   Enter expand   f follow   / search",
    View::Requirements => "↑↓ select   Enter inspect   / filter",
    View::Verify => "↑↓ command   ←→ attempt   Enter output   f follow",
    View::Timeline => "a all  w workers  v verify  s state  e errors  / search",
  };
  let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);
  frame.render_widget(Paragraph::new(Line::from(spans)), rows[0]);
  let mode = if let Some(query) = &app.search {
    format!(" filter: {query}_")
  } else if app.run_active {
    format!(" {contextual}   q/Ctrl-C stop")
  } else {
    format!(" {contextual}   r start run   q close")
  };
  frame.render_widget(
    Paragraph::new(mode).style(Style::default().fg(Color::Gray)),
    rows[1],
  );
}

fn draw_help(frame: &mut Frame, area: Rect) {
  let popup = centered_rect(70, 70, area);
  frame.render_widget(Clear, popup);
  let lines = vec![
    section_heading("MISSION CONTROL KEYS"),
    Line::from(""),
    Line::from("r         start or resume run"),
    Line::from("1–5       switch view"),
    Line::from("j/k ↑/↓   select or scroll"),
    Line::from("Enter     inspect or expand"),
    Line::from("Space     expand or collapse"),
    Line::from("Esc       back or close detail"),
    Line::from("f         toggle follow mode"),
    Line::from("d         repository changes"),
    Line::from("/         filter current view"),
    Line::from("?         close this help"),
    Line::from("q/Ctrl-C  stop active run or close when idle"),
    Line::from(""),
    Line::from(Span::styled(
      "Deterministic verification—not worker claims—controls completion.",
      Style::default().fg(Color::Cyan),
    )),
  ];
  frame.render_widget(
    Paragraph::new(lines)
      .block(Block::default().borders(Borders::ALL).title(" loops help "))
      .wrap(Wrap { trim: true }),
    popup,
  );
}

fn draw_changes_overlay(frame: &mut Frame, app: &App, area: Rect) {
  let popup = centered_rect(72, 72, area);
  frame.render_widget(Clear, popup);
  let mut lines = vec![section_heading("CHANGES"), Line::from("")];
  for change in &app.repository_changes {
    let color = match change.status {
      'A' | '?' => Color::Green,
      'D' => Color::Red,
      _ => Color::Yellow,
    };
    lines.push(Line::from(vec![
      Span::styled(
        format!("{}  ", change.status),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
      ),
      Span::raw(&change.path),
    ]));
  }
  lines.push(Line::from(""));
  lines.push(Line::from(Span::styled(
    format_changes_summary(&app.repository_changes),
    Style::default().fg(QUIET),
  )));
  frame.render_widget(
    Paragraph::new(lines)
      .block(
        Block::default()
          .borders(Borders::ALL)
          .title(" repository changes · Esc close "),
      )
      .wrap(Wrap { trim: true }),
    popup,
  );
}

fn centered_rect(width_percent: u16, height_percent: u16, area: Rect) -> Rect {
  let vertical = Layout::vertical([
    Constraint::Percentage((100 - height_percent) / 2),
    Constraint::Percentage(height_percent),
    Constraint::Percentage((100 - height_percent) / 2),
  ])
  .split(area);
  Layout::horizontal([
    Constraint::Percentage((100 - width_percent) / 2),
    Constraint::Percentage(width_percent),
    Constraint::Percentage((100 - width_percent) / 2),
  ])
  .split(vertical[1])[1]
}

fn section_block(title: &str) -> Block<'static> {
  Block::default()
    .borders(Borders::TOP)
    .border_style(Style::default().fg(Color::Rgb(55, 65, 75)))
    .title(Span::styled(
      title.to_owned(),
      Style::default()
        .fg(Color::Gray)
        .add_modifier(Modifier::BOLD),
    ))
}

fn empty_state(title: &str, message: &str) -> Paragraph<'static> {
  Paragraph::new(format!("\n{message}"))
    .block(section_block(&format!(" {title} ")))
    .alignment(Alignment::Center)
    .style(Style::default().fg(QUIET))
}

fn section_heading(text: &str) -> Line<'static> {
  Line::from(Span::styled(
    text.to_owned(),
    Style::default()
      .fg(Color::Gray)
      .add_modifier(Modifier::BOLD),
  ))
}

fn metric_line(mark: &str, color: Color, value: usize, label: &str) -> Line<'static> {
  Line::from(vec![
    Span::styled(format!("{mark} "), Style::default().fg(color)),
    Span::styled(
      format!("{value:>3}"),
      Style::default().add_modifier(Modifier::BOLD),
    ),
    Span::raw(format!(" {label}")),
  ])
}

fn label_value(label: &str, value: &str, color: Color) -> Line<'static> {
  Line::from(vec![
    Span::styled(format!("{label:<16}"), Style::default().fg(QUIET)),
    Span::styled(
      value.to_owned(),
      Style::default().fg(color).add_modifier(Modifier::BOLD),
    ),
  ])
}

fn requirement_mark(status: Option<&RequirementStatus>) -> (&'static str, Color) {
  match status {
    Some(RequirementStatus::Satisfied) => ("✓", Color::Green),
    Some(RequirementStatus::Partial) => ("◐", Color::Yellow),
    Some(RequirementStatus::Missing) => ("○", QUIET),
    None => ("·", QUIET),
  }
}

fn requirement_status(status: &RequirementStatus) -> &'static str {
  match status {
    RequirementStatus::Satisfied => "SATISFIED",
    RequirementStatus::Partial => "PARTIAL",
    RequirementStatus::Missing => "MISSING",
  }
}

fn role_label(role: WorkerRole) -> &'static str {
  match role {
    WorkerRole::Architect => "ARCHITECT",
    WorkerRole::Reconcile => "RECONCILE",
    WorkerRole::Implement => "IMPLEMENT",
    WorkerRole::Repair => "REPAIR",
    WorkerRole::Assess => "ASSESS",
  }
}

fn role_symbol(role: WorkerRole) -> &'static str {
  match role {
    WorkerRole::Architect => "◇",
    WorkerRole::Reconcile => "⌕",
    WorkerRole::Implement => "◆",
    WorkerRole::Repair => "⚒",
    WorkerRole::Assess => "◆",
  }
}

fn role_color(role: WorkerRole) -> Color {
  match role {
    WorkerRole::Repair => Color::Yellow,
    WorkerRole::Implement => Color::Cyan,
    _ => Color::Gray,
  }
}

fn worker_status(status: WorkerUiStatus) -> &'static str {
  match status {
    WorkerUiStatus::Running => "ACTIVE",
    WorkerUiStatus::Succeeded => "COMPLETED",
    WorkerUiStatus::Failed => "FAILED",
  }
}

fn phase_label(phase: &Phase) -> &'static str {
  match phase {
    Phase::Initialized => "INITIALIZED",
    Phase::Architecting => "ARCHITECT",
    Phase::Reconciling => "RECONCILE",
    Phase::Implementing => "IMPLEMENT",
    Phase::Verifying => "VERIFY",
    Phase::Repairing => "REPAIR",
    Phase::Assessing => "ASSESS",
    Phase::Complete => "COMPLETE",
  }
}

fn phase_short(phase: &Phase) -> &'static str {
  match phase {
    Phase::Reconciling => "R",
    Phase::Implementing => "I",
    Phase::Verifying => "V",
    Phase::Repairing => "P",
    Phase::Assessing => "A",
    _ => "·",
  }
}

fn step_mark(outcome: StepOutcome) -> (&'static str, Color) {
  match outcome {
    StepOutcome::Active => ("●", Color::Cyan),
    StepOutcome::Pass => ("✓", Color::Green),
    StepOutcome::Fail => ("✗", Color::Red),
  }
}

fn status_name(status: &RunStatus) -> &'static str {
  match status {
    RunStatus::Idle => "IDLE",
    RunStatus::Running => "RUNNING",
    RunStatus::Done => "DONE",
    RunStatus::Blocked => "BLOCKED",
    RunStatus::Failed => "FAILED",
    RunStatus::Stopped => "STOPPED",
  }
}

fn status_color(status: &RunStatus) -> Color {
  match status {
    RunStatus::Done => Color::Green,
    RunStatus::Blocked => Color::Yellow,
    RunStatus::Failed => Color::Red,
    RunStatus::Running => Color::Cyan,
    _ => Color::Gray,
  }
}

fn timeline_visible(filter: TimelineFilter, kind: TimelineKind, detail: &str) -> bool {
  match filter {
    TimelineFilter::All => true,
    TimelineFilter::Workers => kind == TimelineKind::Worker,
    TimelineFilter::Verification => kind == TimelineKind::Verification,
    TimelineFilter::State => matches!(kind, TimelineKind::State | TimelineKind::Work),
    TimelineFilter::Errors => {
      kind == TimelineKind::Error
        || (kind == TimelineKind::Verification && detail.starts_with("FAIL"))
    }
  }
}

fn timeline_mark(kind: TimelineKind, detail: &str) -> (&'static str, Color) {
  match kind {
    TimelineKind::Error => ("✗", Color::Red),
    TimelineKind::Verification if detail.starts_with("PASS") => ("✓", Color::Green),
    TimelineKind::Verification if detail.starts_with("FAIL") => ("✗", Color::Red),
    TimelineKind::Verification => ("·", Color::Gray),
    TimelineKind::Work => ("→", Color::Cyan),
    TimelineKind::Worker => ("◆", Color::Gray),
    TimelineKind::State => ("·", QUIET),
  }
}

fn format_changes_summary(changes: &[RepositoryChange]) -> String {
  if changes.is_empty() {
    "clean working tree".into()
  } else {
    format!(
      "{} changed {}",
      changes.len(),
      if changes.len() == 1 { "file" } else { "files" }
    )
  }
}

fn compact_args(value: &serde_json::Value) -> String {
  let text = serde_json::to_string(value).unwrap_or_default();
  if text == "null" || text == "{}" {
    return String::new();
  }
  if text.len() > 180 {
    let end = utf8_floor(&text, 180);
    format!("{}…", &text[..end])
  } else {
    text
  }
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

fn utf8_floor(text: &str, max_bytes: usize) -> usize {
  let mut end = max_bytes.min(text.len());
  while end > 0 && !text.is_char_boundary(end) {
    end -= 1;
  }
  end
}

fn time_of(timestamp: &str) -> String {
  DateTime::parse_from_rfc3339(timestamp)
    .map(|value| value.with_timezone(&Local).format("%H:%M:%S").to_string())
    .unwrap_or_else(|_| timestamp.chars().take(8).collect())
}

fn now_text() -> String {
  chrono::Utc::now().to_rfc3339()
}

fn duration_between(start: &str, end: Option<&str>) -> Option<String> {
  let start = DateTime::parse_from_rfc3339(start).ok()?;
  let end = end
    .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
    .unwrap_or_else(|| chrono::Utc::now().fixed_offset());
  let millis = (end - start).num_milliseconds().max(0) as u128;
  Some(format_millis(millis))
}

fn tool_duration(tool: &ToolInvocation) -> Option<String> {
  duration_between(&tool.started_at, tool.finished_at.as_deref())
}

fn time_range(start: &str, end: &str) -> String {
  duration_between(start, Some(end)).unwrap_or_else(|| "duration unavailable".into())
}

fn format_millis(millis: u128) -> String {
  if millis < 1_000 {
    format!("{millis} ms")
  } else {
    format!("{:.1}s", millis as f64 / 1_000.0)
  }
}

fn format_duration(duration: Duration) -> String {
  let seconds = duration.as_secs();
  format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;
  use crate::model::{CommandResult, RequirementCatalog, RequirementCounts, WorkUnit};

  fn app() -> App {
    App::new("test-project".into(), State::fresh())
  }

  fn worker_start() -> WorkerEvent {
    WorkerEvent::Start {
      role: WorkerRole::Implement,
      at: "2026-08-10T10:00:00Z".into(),
      skills: vec!["implementation".into(), "rust".into()],
    }
  }

  #[test]
  fn idle_run_key_requests_start() {
    let mut app = app();
    let cancel = CancellationToken::new();
    let action = handle_key(
      &mut app,
      KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
      &cancel,
    );
    assert_eq!(action, TuiAction::Start);
  }

  #[test]
  fn idle_quit_key_exits_without_cancelling() {
    let mut app = app();
    let cancel = CancellationToken::new();
    let action = handle_key(
      &mut app,
      KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
      &cancel,
    );
    assert!(action == TuiAction::Exit && !cancel.is_cancelled());
  }

  #[test]
  fn active_quit_key_cancels_without_closing_immediately() {
    let mut app = app();
    app.begin_run();
    let cancel = CancellationToken::new();
    let action = handle_key(
      &mut app,
      KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
      &cancel,
    );
    assert!(action == TuiAction::None && cancel.is_cancelled());
  }

  #[test]
  fn worker_start_creates_structured_session() {
    let mut app = app();
    app.apply(RunEvent::Worker(worker_start()));
    assert!(matches!(
      app.workers.last().map(|worker| worker.status),
      Some(WorkerUiStatus::Running)
    ));
  }

  #[test]
  fn streamed_worker_text_coalesces_adjacent_deltas() {
    let mut app = app();
    app.apply(RunEvent::Worker(worker_start()));
    for delta in ["inspect ", "auth"] {
      app.apply(RunEvent::Worker(WorkerEvent::Text {
        role: WorkerRole::Implement,
        at: "2026-08-10T10:00:01Z".into(),
        delta: delta.into(),
      }));
    }
    let WorkerActivity::Text { text, .. } = &app.workers[0].activities[0] else {
      panic!("expected text activity");
    };
    assert_eq!(text, "inspect auth");
  }

  #[test]
  fn tool_start_preserves_structured_arguments() {
    let mut app = app();
    app.apply(RunEvent::Worker(worker_start()));
    app.apply(RunEvent::Worker(WorkerEvent::ToolStart {
      role: WorkerRole::Implement,
      at: "2026-08-10T10:00:01Z".into(),
      tool_name: "read".into(),
      args: json!({"path":"src/auth.rs"}),
    }));
    let WorkerActivity::Tool(tool) = &app.workers[0].activities[0] else {
      panic!("expected tool activity");
    };
    assert_eq!(tool.args, json!({"path":"src/auth.rs"}));
  }

  #[test]
  fn tool_end_updates_matching_invocation_and_expands_errors() {
    let mut app = app();
    app.apply(RunEvent::Worker(worker_start()));
    app.apply(RunEvent::Worker(WorkerEvent::ToolStart {
      role: WorkerRole::Implement,
      at: "2026-08-10T10:00:01Z".into(),
      tool_name: "bash".into(),
      args: json!({"command":"cargo test"}),
    }));
    app.apply(RunEvent::Worker(WorkerEvent::ToolEnd {
      role: WorkerRole::Implement,
      at: "2026-08-10T10:00:03Z".into(),
      tool_name: "bash".into(),
      is_error: true,
      output: Some("failed".into()),
    }));
    let WorkerActivity::Tool(tool) = &app.workers[0].activities[0] else {
      panic!("expected tool activity");
    };
    assert!(tool.is_error && tool.expanded && tool.finished_at.is_some());
  }

  #[test]
  fn worker_end_records_terminal_status() {
    let mut app = app();
    app.apply(RunEvent::Worker(worker_start()));
    app.apply(RunEvent::Worker(WorkerEvent::End {
      role: WorkerRole::Implement,
      at: "2026-08-10T10:00:04Z".into(),
      ok: true,
      message: None,
    }));
    assert_eq!(app.workers[0].status, WorkerUiStatus::Succeeded);
  }

  #[test]
  fn reconcile_updates_requirement_objects() {
    let mut app = app();
    app.apply(RunEvent::Catalog(RequirementCatalog {
      spec_hash: "hash".into(),
      requirements: vec![Requirement {
        id: "REQ-001".into(),
        title: "Auth".into(),
        description: "Authenticate users".into(),
        acceptance_criteria: Vec::new(),
      }],
    }));
    app.apply(RunEvent::Reconcile(ReconcileResult {
      complete: false,
      summary: "one gap".into(),
      requirements: vec![RequirementAssessment {
        id: "REQ-001".into(),
        status: RequirementStatus::Partial,
        evidence: vec!["login exists".into()],
        gaps: vec!["callback validation".into()],
      }],
      next_work_unit: None,
    }));
    assert!(matches!(
      app.assessments.get("REQ-001").map(|item| &item.status),
      Some(RequirementStatus::Partial)
    ));
  }

  #[test]
  fn verification_report_preserves_commands_and_expands_failure() {
    let mut app = app();
    app.apply(RunEvent::Verification(VerificationReport {
      passed: false,
      started_at: "2026-08-10T10:00:00Z".into(),
      finished_at: "2026-08-10T10:00:01Z".into(),
      commands: vec![CommandResult {
        command: "cargo test".into(),
        exit_code: Some(1),
        timed_out: false,
        duration_ms: 1_000,
        stdout: String::new(),
        stderr: "failure".into(),
      }],
      warnings: Vec::new(),
    }));
    assert_eq!(app.verifications[0].expanded, [true]);
  }

  #[test]
  fn run_completion_preserves_final_state() {
    let mut app = app();
    let mut state = State::fresh();
    state.status = RunStatus::Done;
    state.phase = Phase::Complete;
    state.requirement_counts = RequirementCounts {
      total: 1,
      satisfied: 1,
      partial: 0,
      missing: 0,
    };
    app.apply(RunEvent::Finished(state));
    assert!(app.finished && app.state.status == RunStatus::Done);
  }

  #[test]
  fn current_work_unit_is_available_to_overview() {
    let mut app = app();
    let mut state = State::fresh();
    state.current_work_unit = Some(WorkUnit {
      id: "WU-007".into(),
      title: "OAuth callback".into(),
      objective: String::new(),
      requirement_ids: vec!["REQ-004".into()],
      acceptance_criteria: Vec::new(),
      suggested_checks: Vec::new(),
    });
    app.apply(RunEvent::State(state));
    assert_eq!(
      app
        .state
        .current_work_unit
        .as_ref()
        .map(|work| work.id.as_str()),
      Some("WU-007")
    );
  }
  #[test]
  fn idle_overview_shows_ready_start_action() {
    use ratatui::backend::TestBackend;

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = app();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let screen = terminal
      .backend()
      .buffer()
      .content
      .iter()
      .map(|cell| cell.symbol())
      .collect::<String>();
    assert!(screen.contains("READY") && screen.contains("r start run"));
  }

  #[test]
  fn every_view_renders_at_minimum_supported_size() {
    use ratatui::backend::TestBackend;

    let backend = TestBackend::new(42, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = app();
    for view in [
      View::Overview,
      View::Worker,
      View::Requirements,
      View::Verify,
      View::Timeline,
    ] {
      app.switch_view(view);
      terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    }
  }
}
