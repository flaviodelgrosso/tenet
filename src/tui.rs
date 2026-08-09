use std::{io, path::PathBuf, sync::Arc, time::Duration};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap},
    Frame, Terminal,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    backend::AgentBackend,
    controller::Controller,
    events::{EventSink, RunEvent},
    model::{
        RequirementAssessment, RequirementStatus, RunStatus, State, VerificationReport,
        WorkerEvent, WorkerRole,
    },
};

pub async fn run(cwd: PathBuf, backend: Arc<dyn AgentBackend>) -> Result<State> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let controller = Controller::new(cwd, backend, EventSink::new(Some(tx)));
    let task_cancel = cancel.clone();
    let mut task = tokio::spawn(async move { controller.run(task_cancel).await });

    let mut terminal = setup_terminal()?;
    let mut app = App::new();
    let result = loop {
        while let Ok(run_event) = rx.try_recv() {
            app.apply(run_event);
        }

        terminal.draw(|frame| draw(frame, &app))?;

        if event::poll(Duration::from_millis(60))? {
            if let Event::Key(key) = event::read()? {
                if handle_key(&mut app, key, &cancel) && app.finished {
                    break task.await??;
                }
            }
        }

        if task.is_finished() && !app.finished {
            match (&mut task).await {
                Ok(Ok(state)) => {
                    app.state = state.clone();
                    app.finished = true;
                    app.push_timeline("run task finished".into());
                    // The JoinHandle has been consumed. Return only after the user closes the final view.
                    loop {
                        while let Ok(run_event) = rx.try_recv() {
                            app.apply(run_event);
                        }
                        terminal.draw(|frame| draw(frame, &app))?;
                        if event::poll(Duration::from_millis(80))? {
                            if let Event::Key(key) = event::read()? {
                                if matches!(
                                    key.code,
                                    KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')
                                ) {
                                    break;
                                }
                                let _ = handle_key(&mut app, key, &cancel);
                            }
                        }
                    }
                    break state;
                }
                Ok(Err(error)) => {
                    app.finished = true;
                    app.push_transcript(format!("\n\nERROR: {error:#}\n"));
                    loop {
                        terminal.draw(|frame| draw(frame, &app))?;
                        if event::poll(Duration::from_millis(80))? {
                            if let Event::Key(key) = event::read()? {
                                if matches!(
                                    key.code,
                                    KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')
                                ) {
                                    break;
                                }
                                let _ = handle_key(&mut app, key, &cancel);
                            }
                        }
                    }
                    restore_terminal(&mut terminal)?;
                    return Err(error);
                }
                Err(join_error) => {
                    restore_terminal(&mut terminal)?;
                    return Err(join_error.into());
                }
            }
        }
    };

    restore_terminal(&mut terminal)?;
    Ok(result)
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Run,
    Evidence,
    Timeline,
}

struct App {
    state: State,
    assessments: Vec<RequirementAssessment>,
    transcript: String,
    timeline: Vec<String>,
    view: View,
    scroll: u16,
    follow: bool,
    finished: bool,
}

impl App {
    fn new() -> Self {
        Self {
            state: State::fresh(),
            assessments: Vec::new(),
            transcript: String::new(),
            timeline: Vec::new(),
            view: View::Run,
            scroll: 0,
            follow: true,
            finished: false,
        }
    }

    fn apply(&mut self, event: RunEvent) {
        match event {
            RunEvent::State(state) => {
                self.push_timeline(format!("{} · {}", phase_name(&state), state.last_summary));
                self.state = state;
            }
            RunEvent::Message(message) => self.push_timeline(message),
            RunEvent::Worker(event) => self.apply_worker(event),
            RunEvent::Reconcile(result) => {
                self.assessments = result.requirements;
                self.push_timeline(format!("reconcile · {}", result.summary));
            }
            RunEvent::Verification(report) => self.apply_verification(report),
            RunEvent::Finished(state) => {
                self.state = state;
                self.finished = true;
            }
        }
    }

    fn apply_worker(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::Start { role, .. } => {
                self.push_transcript(format!("\n\n◆ {} · fresh context\n", role_label(role)));
                self.push_timeline(format!("{} started", role.as_str()));
            }
            WorkerEvent::Text { delta, .. } => self.push_transcript(sanitize_terminal_text(&delta)),
            WorkerEvent::ToolStart {
                tool_name, args, ..
            } => {
                let args = compact_args(&args);
                self.push_transcript(format!("\n  ❯ {tool_name}{args}\n"));
            }
            WorkerEvent::ToolEnd {
                tool_name,
                is_error,
                output,
                ..
            } => {
                let mark = if is_error { "✗" } else { "✓" };
                self.push_transcript(format!("  {mark} {tool_name}\n"));
                if let Some(output) = output {
                    let output = sanitize_terminal_text(&output);
                    for line in output.lines().take(14) {
                        self.push_transcript(format!("    {line}\n"));
                    }
                }
            }
            WorkerEvent::End {
                role, ok, message, ..
            } => {
                let mark = if ok { "✓" } else { "✗" };
                self.push_transcript(format!(
                    "\n{mark} {} {}\n",
                    role_label(role),
                    message.unwrap_or_default()
                ));
                self.push_timeline(format!(
                    "{} {}",
                    role.as_str(),
                    if ok { "done" } else { "failed" }
                ));
            }
        }
    }

    fn apply_verification(&mut self, report: VerificationReport) {
        self.push_transcript("\n\n✓ VERIFY · deterministic gates\n".into());
        for command in &report.commands {
            let mark = if command.exit_code == Some(0) && !command.timed_out {
                "✓"
            } else {
                "✗"
            };
            self.push_transcript(format!(
                "  {mark} {}  ({} ms)\n",
                command.command, command.duration_ms
            ));
            if mark == "✗" {
                for line in command
                    .stderr
                    .lines()
                    .chain(command.stdout.lines())
                    .take(20)
                {
                    self.push_transcript(format!("    {line}\n"));
                }
            }
        }
        for warning in &report.warnings {
            self.push_transcript(format!("  ! {warning}\n"));
        }
        self.push_timeline(format!(
            "verify · {}",
            if report.passed { "PASS" } else { "FAIL" }
        ));
    }

    fn push_transcript(&mut self, text: String) {
        self.transcript.push_str(&text);
        if self.transcript.len() > 2_000_000 {
            let mut keep_from = self.transcript.len() - 1_500_000;
            while keep_from < self.transcript.len() && !self.transcript.is_char_boundary(keep_from)
            {
                keep_from += 1;
            }
            self.transcript = self.transcript[keep_from..].to_owned();
        }
    }

    fn push_timeline(&mut self, text: String) {
        self.timeline.push(text);
        if self.timeline.len() > 2000 {
            self.timeline.drain(..500);
        }
    }
}

fn handle_key(app: &mut App, key: KeyEvent, cancel: &CancellationToken) -> bool {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        if app.finished {
            return true;
        }
        cancel.cancel();
        app.push_timeline("stop requested".into());
        return false;
    }
    match key.code {
        KeyCode::Char('q') => {
            if app.finished {
                return true;
            }
            cancel.cancel();
            app.push_timeline("stop requested".into());
        }
        KeyCode::Tab => {
            app.view = match app.view {
                View::Run => View::Evidence,
                View::Evidence => View::Timeline,
                View::Timeline => View::Run,
            };
            app.scroll = 0;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.scroll = app.scroll.saturating_sub(1);
            app.follow = false;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.scroll = app.scroll.saturating_add(1);
            app.follow = false;
        }
        KeyCode::PageUp => {
            app.scroll = app.scroll.saturating_sub(12);
            app.follow = false;
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_add(12);
            app.follow = false;
        }
        KeyCode::Home | KeyCode::Char('g') => {
            app.scroll = 0;
            app.follow = false;
        }
        KeyCode::End | KeyCode::Char('G') => {
            app.follow = true;
        }
        KeyCode::Enter | KeyCode::Esc if app.finished => return true,
        _ => {}
    }
    false
}

fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(frame.area());

    draw_header(frame, app, chunks[0]);
    match app.view {
        View::Run => draw_run(frame, app, chunks[1]),
        View::Evidence => draw_evidence(frame, app, chunks[1]),
        View::Timeline => draw_timeline(frame, app, chunks[1]),
    }
    draw_footer(frame, app, chunks[2]);
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let tabs = Tabs::new(vec![
        Line::from(" Run "),
        Line::from(" Evidence "),
        Line::from(" Timeline "),
    ])
    .select(match app.view {
        View::Run => 0,
        View::Evidence => 1,
        View::Timeline => 2,
    })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" loops · spec-driven autonomous development "),
    )
    .highlight_style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(tabs, area);
}

fn draw_run(frame: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    let items: Vec<ListItem> = if app.assessments.is_empty() {
        vec![ListItem::new("No assessment yet")]
    } else {
        app.assessments
            .iter()
            .map(|req| {
                let (mark, color) = match req.status {
                    RequirementStatus::Satisfied => ("✓", Color::Green),
                    RequirementStatus::Partial => ("◐", Color::Yellow),
                    RequirementStatus::Missing => ("○", Color::DarkGray),
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{mark} "), Style::default().fg(color)),
                    Span::raw(&req.id),
                ]))
            })
            .collect()
    };
    let title = format!(
        " requirements {}/{} ",
        app.state.requirement_counts.satisfied, app.state.requirement_counts.total
    );
    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title(title)),
        cols[0],
    );

    let visible_height = cols[1].height.saturating_sub(2) as usize;
    let logical_lines = app.transcript.lines().count();
    let scroll = if app.follow {
        logical_lines
            .saturating_sub(visible_height)
            .min(u16::MAX as usize) as u16
    } else {
        app.scroll
    };
    let title = format!(" {} · cycle {} ", phase_name(&app.state), app.state.cycle);
    let transcript = if app.transcript.is_empty() {
        "Waiting for worker output…"
    } else {
        &app.transcript
    };
    let paragraph = Paragraph::new(transcript)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, cols[1]);
}

fn draw_evidence(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = Vec::new();
    if app.assessments.is_empty() {
        lines.push(Line::from(
            "No requirement evidence has been collected yet.",
        ));
    }
    for req in &app.assessments {
        let mark = match req.status {
            RequirementStatus::Satisfied => "✓",
            RequirementStatus::Partial => "◐",
            RequirementStatus::Missing => "○",
        };
        lines.push(Line::from(vec![Span::styled(
            format!("{mark} {}", req.id),
            Style::default().add_modifier(Modifier::BOLD),
        )]));
        for evidence in &req.evidence {
            lines.push(Line::from(format!("    evidence: {evidence}")));
        }
        for gap in &req.gaps {
            lines.push(Line::from(Span::styled(
                format!("    gap: {gap}"),
                Style::default().fg(Color::Yellow),
            )));
        }
        lines.push(Line::from(""));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" requirement evidence "),
            )
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0)),
        area,
    );
}

fn draw_timeline(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = app
        .timeline
        .iter()
        .enumerate()
        .map(|(i, item)| Line::from(format!("{:04}  {item}", i + 1)))
        .collect();
    let visible = area.height.saturating_sub(2) as usize;
    let scroll = if app.follow {
        lines.len().saturating_sub(visible).min(u16::MAX as usize) as u16
    } else {
        app.scroll
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" loop timeline "),
            )
            .scroll((scroll, 0)),
        area,
    );
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let work = app
        .state
        .current_work_unit
        .as_ref()
        .map(|w| w.id.as_str())
        .unwrap_or("-");
    let status = format!(
        " {} · {} · WU {} · {}/{} requirements ",
        status_name(&app.state.status),
        phase_name(&app.state),
        work,
        app.state.requirement_counts.satisfied,
        app.state.requirement_counts.total,
    );
    let help = if app.finished {
        "Enter/Esc/q close · Tab views · ↑↓ scroll · End follow"
    } else {
        "Tab views · ↑↓/Pg scroll · End follow · q/Ctrl-C stop"
    };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(status_color(&app.state.status))),
        cols[0],
    );
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        cols[1],
    );
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

fn phase_name(state: &State) -> &'static str {
    use crate::model::Phase::*;
    match state.phase {
        Initialized => "INITIALIZED",
        Architecting => "ARCHITECT",
        Reconciling => "RECONCILE",
        Implementing => "IMPLEMENT",
        Verifying => "VERIFY",
        Repairing => "REPAIR",
        Assessing => "ASSESS",
        Complete => "COMPLETE",
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

fn compact_args(value: &serde_json::Value) -> String {
    let text = serde_json::to_string(value).unwrap_or_default();
    if text == "null" || text == "{}" {
        return String::new();
    }
    let text = if text.len() > 140 {
        let mut end = 140;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &text[..end])
    } else {
        text
    };
    format!(" {text}")
}
