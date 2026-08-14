use ratatui::{
  layout::{Alignment, Rect},
  style::Style,
  text::{Line, Span},
  widgets::{Block, Borders, Clear, Paragraph, Wrap},
  Frame,
};

use loops_domain::model::{RequirementStatus, RunStatus, VerificationReport};

use crate::tui::{
  action::{ActivityCategory, Overlay, Screen},
  app::{failure_preview, requirement_status, status_label, Activity, Application},
  layout::{self, Density},
  theme,
};

pub fn render(frame: &mut Frame<'_>, app: &Application) {
  let area = frame.area();
  let density = layout::density(area);
  if density == Density::TooSmall {
    too_small(frame, area);
    return;
  }

  let (header, body, footer) = layout::shell(area);
  render_header(frame, app, header, density);
  match app.ui().screen {
    Screen::Run => run_screen(frame, app, body, density),
    Screen::Requirements => requirements_screen(frame, app, body, density),
    Screen::Checks => checks_screen(frame, app, body, density),
    Screen::Changes => changes_screen(frame, app, body),
    Screen::History => history_screen(frame, app, body),
  }
  render_footer(frame, app, footer);
  if let Some(overlay) = &app.ui().overlay {
    render_overlay(frame, overlay, area);
  }
}

fn too_small(frame: &mut Frame<'_>, area: Rect) {
  frame.render_widget(
    Paragraph::new(vec![
      Line::styled("LOOPS", theme::identity()),
      Line::raw(""),
      Line::styled("Needs 42 columns × 12 rows", theme::heading()),
      Line::styled("Resize to open the operations console.", theme::muted()),
    ])
    .alignment(Alignment::Center)
    .wrap(Wrap { trim: true }),
    area,
  );
}

fn render_header(frame: &mut Frame<'_>, app: &Application, area: Rect, density: Density) {
  let state = app.state();
  let elapsed = app.elapsed_seconds();
  let mut left = vec![
    Span::raw(" "),
    Span::styled("LOOPS", theme::identity()),
    Span::styled("  ", theme::muted()),
    Span::styled(app.project_name().to_owned(), theme::code()),
  ];
  if density != Density::Narrow {
    left.extend([
      Span::styled("  ·  ", theme::muted()),
      Span::styled(app.ui().screen.label(), theme::accent()),
    ]);
  }
  let mut right = vec![
    Span::styled(run_status_mark(&state.status), status_style(&state.status)),
    Span::styled(
      format!(" {}", status_label(&state.status)),
      status_style(&state.status),
    ),
  ];
  if density != Density::Narrow {
    right.extend([
      Span::styled("   ", theme::muted()),
      Span::styled(crate::tui::app::phase_label(&state.phase), theme::caption()),
      Span::styled(format!("   CYCLE {:02}", state.cycle), theme::caption()),
      Span::styled(
        format!("   {:02}:{:02}", elapsed / 60, elapsed % 60),
        theme::muted(),
      ),
    ]);
  }
  let left_width = span_width(&left).min(area.width as usize);
  let gap = area
    .width
    .saturating_sub(left_width as u16 + span_width(&right) as u16);
  left.push(Span::raw(" ".repeat(gap as usize)));
  left.extend(right);
  frame.render_widget(
    Paragraph::new(vec![
      Line::from(left),
      Line::styled("─".repeat(area.width as usize), theme::subtle_border()),
    ]),
    area,
  );
}

fn render_footer(frame: &mut Frame<'_>, app: &Application, area: Rect) {
  let base = if area.width < 60 {
    match app.ui().screen {
      Screen::Run => vec![("/", "search"), ("Enter", "inspect")],
      Screen::History => vec![("/", "search"), ("End", "follow")],
      _ => vec![("/", "filter"), ("Enter", "inspect")],
    }
  } else {
    match app.ui().screen {
      Screen::Run => vec![
        ("/", "search"),
        ("Enter", "inspect"),
        ("c", "context"),
        (":", "commands"),
      ],
      Screen::History => vec![
        ("/", "search"),
        ("a/t/w/v/e", "filter"),
        ("End", "follow"),
        (":", "commands"),
      ],
      _ => vec![
        ("/", "filter"),
        ("Enter", "inspect"),
        (":", "commands"),
        ("?", "help"),
      ],
    }
  };
  let mut spans = key_hints(&base);
  let unseen = match app.ui().screen {
    Screen::Run if !app.ui().run.follow => Some(app.ui().run.unseen),
    Screen::History if !app.ui().history.follow => Some(app.ui().history.unseen),
    _ => None,
  };
  if let Some(count) = unseen {
    spans.extend([
      Span::styled("   ↓ ", theme::warning()),
      Span::styled(format!("{count} newer · End to follow"), theme::muted()),
    ]);
  }
  frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn run_screen(frame: &mut Frame<'_>, app: &Application, area: Rect, density: Density) {
  if !app.running() && matches!(app.state().status, RunStatus::Idle) {
    idle_screen(frame, app, area);
    return;
  }
  let (main, side) = layout::split_main(area, density);
  let title = match app.state().status {
    RunStatus::Blocked | RunStatus::Failed => "ATTENTION",
    RunStatus::Done => "COMPLETE",
    _ => "ACTIVITY",
  };
  let mut lines = screen_heading(title, run_subtitle(app));
  if let Some(summary) = terminal_summary(app, density) {
    lines.push(Line::raw(""));
    lines.push(summary);
  }
  let visible = app.visible_feed();
  if visible.is_empty() {
    lines.extend(empty_state(
      "NO ACTIVITY YET",
      "The controller will stream decisions and tool work here.",
    ));
  } else {
    for (selected, index) in visible.iter().enumerate() {
      if let Some(item) = app.activities().get(*index) {
        lines.extend(activity_lines(
          item,
          selected == app.ui().run.selected,
          density,
          false,
        ));
      }
    }
  }
  frame.render_widget(
    Paragraph::new(lines)
      .scroll((app.ui().run.scroll, 0))
      .wrap(Wrap { trim: true }),
    main,
  );
  if let Some(side) = side {
    context_pane(frame, app, side);
  }
}

fn run_subtitle(app: &Application) -> String {
  if app.ui().run.query.is_empty() {
    format!("{} events", app.visible_feed().len())
  } else {
    format!(
      "{} matching · / {}",
      app.visible_feed().len(),
      app.ui().run.query
    )
  }
}

fn terminal_summary(app: &Application, density: Density) -> Option<Line<'static>> {
  let state = app.state();
  match state.status {
    RunStatus::Done => {
      let detail = if density == Density::Narrow {
        format!(
          "{}/{} requirements",
          state.requirement_counts.satisfied, state.requirement_counts.total
        )
      } else {
        format!(
          "{}/{} requirements · {} cycles · {} work units",
          state.requirement_counts.satisfied,
          state.requirement_counts.total,
          state.cycle,
          state.completed_work_units.len()
        )
      };
      Some(Line::from(vec![
        Span::styled(
          if density == Density::Narrow {
            "✓  COMPLETE  "
          } else {
            "✓  RUN COMPLETE  "
          },
          theme::success(),
        ),
        Span::styled(detail, theme::secondary()),
      ]))
    }
    RunStatus::Blocked | RunStatus::Failed => Some(Line::from(vec![
      Span::styled(
        format!("✕  {}  ", status_label(&state.status)),
        theme::failure(),
      ),
      Span::styled(
        state
          .blocked_reason
          .as_ref()
          .or(state.last_error.as_ref())
          .unwrap_or(&state.last_summary)
          .to_owned(),
        theme::secondary(),
      ),
    ])),
    _ => None,
  }
}

fn activity_lines(
  item: &Activity,
  selected: bool,
  density: Density,
  compact: bool,
) -> Vec<Line<'static>> {
  let prefix = selected_prefix(selected);
  let metadata = if density == Density::Narrow {
    String::new()
  } else {
    format!("{}  ", display_time(&item.at))
  };
  let is_tool = item.title.contains("TOOL");
  let is_phase = item.category == ActivityCategory::Controller
    && !item.title.eq_ignore_ascii_case("CONTROLLER")
    && !item.title.eq_ignore_ascii_case("RUN STARTED");
  let mut lines = Vec::new();
  if is_phase && !compact {
    lines.push(selected_line(
      selected,
      vec![
        Span::styled(prefix.clone(), theme::accent()),
        Span::styled(metadata, theme::muted()),
        Span::styled("◆  ", category_style(item.category)),
        Span::styled(item.title.clone(), theme::heading()),
      ],
    ));
    lines.push(selected_line(
      selected,
      vec![
        Span::raw("   "),
        Span::styled(indent(&item.summary, 6), theme::secondary()),
      ],
    ));
    return lines;
  }
  let (glyph, title_style, summary_style) = if is_tool {
    ("├─", theme::muted(), theme::code())
  } else {
    (
      activity_mark(item.category),
      category_style(item.category),
      if item.category == ActivityCategory::Worker {
        theme::muted()
      } else {
        theme::secondary()
      },
    )
  };
  lines.push(selected_line(
    selected,
    vec![
      Span::styled(prefix, theme::accent()),
      Span::styled(metadata, theme::muted()),
      Span::styled(format!("{glyph} "), title_style),
      Span::styled(
        format!("{:<14}", short_title(&item.title, is_tool)),
        title_style,
      ),
      Span::styled(item.summary.clone(), summary_style),
    ],
  ));
  if item.category == ActivityCategory::Error && !compact && item.title.contains("CHECKS") {
    lines.push(selected_line(
      selected,
      vec![
        Span::raw("   "),
        Span::styled("└─ ", theme::failure()),
        Span::styled(first_line(&item.detail), theme::muted()),
      ],
    ));
  }
  lines
}

fn idle_screen(frame: &mut Frame<'_>, app: &Application, area: Rect) {
  let state = app.state();
  let mut lines = screen_heading("READY", "Project state is loaded");
  lines.extend([
    Line::raw(""),
    Line::styled("Press r to start", theme::accent()),
    Line::styled(
      "Reconcile requirements, select work, and verify deterministic evidence.",
      theme::secondary(),
    ),
    Line::raw(""),
    section_line("REQUIREMENT HEALTH"),
    Line::from(vec![
      Span::styled(
        format!("{} ✓", state.requirement_counts.satisfied),
        theme::success(),
      ),
      Span::styled(
        format!("   {} ◐", state.requirement_counts.partial),
        theme::warning(),
      ),
      Span::styled(
        format!("   {} ○", state.requirement_counts.missing),
        theme::muted(),
      ),
      Span::styled(
        format!(
          "   {}",
          progress_meter(
            state.requirement_counts.satisfied,
            state.requirement_counts.total,
            14
          )
        ),
        theme::muted(),
      ),
    ]),
    Line::raw(""),
    section_line("PREVIOUS SUMMARY"),
    Line::styled(state.last_summary.clone(), theme::secondary()),
  ]);
  if let Some(reason) = state.blocked_reason.as_ref().or(state.last_error.as_ref()) {
    lines.extend([
      Line::raw(""),
      section_line("LAST ATTENTION"),
      Line::styled(reason.clone(), theme::failure()),
    ]);
  }
  frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
}

fn context_pane(frame: &mut Frame<'_>, app: &Application, area: Rect) {
  let state = app.state();
  let mut lines = screen_heading("CONTEXT", "Live run state");
  let active_work = app.active_work_units().collect::<Vec<_>>();
  lines.extend([Line::raw(""), section_line("ACTIVE WORK")]);
  if active_work.is_empty() {
    lines.push(Line::styled("No active work units", theme::muted()));
  } else {
    for work in active_work {
      lines.extend([
        Line::styled(work.id.clone(), theme::accent()),
        Line::styled(work.title.clone(), theme::heading()),
        Line::styled(work.requirement_ids.join("  ·  "), theme::muted()),
      ]);
    }
  }
  lines.extend([
    Line::raw(""),
    section_line("REQUIREMENTS"),
    Line::from(vec![
      Span::styled(
        format!("{} ✓", state.requirement_counts.satisfied),
        theme::success(),
      ),
      Span::styled(
        format!("   {} ◐", state.requirement_counts.partial),
        theme::warning(),
      ),
      Span::styled(
        format!("   {} ○", state.requirement_counts.missing),
        theme::muted(),
      ),
    ]),
    Line::styled(
      progress_meter(
        state.requirement_counts.satisfied,
        state.requirement_counts.total,
        18,
      ),
      theme::muted(),
    ),
    Line::raw(""),
    section_line("LOOP"),
    Line::from(vec![
      Span::styled(format!("{:02}", state.cycle), theme::heading()),
      Span::styled(
        format!("   {}", crate::tui::app::phase_label(&state.phase)),
        theme::secondary(),
      ),
    ]),
    Line::raw(""),
    section_line("CHECKS"),
    check_context_line(app),
    Line::raw(""),
    section_line("REPOSITORY"),
    Line::styled(
      format!("{} changed paths", app.changes().len()),
      theme::secondary(),
    ),
  ]);
  if let Some(worker) = app
    .current_worker()
    .filter(|worker| worker.ended_at.is_none())
  {
    lines.extend([
      Line::raw(""),
      section_line("ACTIVE WORKER"),
      Line::styled(crate::tui::action::role_label(worker.role), theme::accent()),
      Line::styled(format!("Started {}", worker.started_at), theme::muted()),
    ]);
  }
  if let Some(attention) = state.blocked_reason.as_ref().or(state.last_error.as_ref()) {
    lines.extend([
      Line::raw(""),
      section_line("ATTENTION"),
      Line::styled(attention.clone(), theme::failure()),
    ]);
  }
  frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
}

fn requirements_screen(frame: &mut Frame<'_>, app: &Application, area: Rect, density: Density) {
  let (list_area, detail_area) = layout::split_main(area, density);
  let mut lines = screen_heading("REQUIREMENTS", requirement_subtitle(app));
  let visible = app.visible_requirements();
  if visible.is_empty() {
    lines.extend(empty_state(
      "NO REQUIREMENTS",
      "Adjust the filter or load a requirement catalog.",
    ));
  }
  for (selected, index) in visible.iter().enumerate() {
    if let Some(requirement) = app.catalog().get(*index) {
      let status = requirement_status(app.assessments().get(&requirement.id));
      let label = if density == Density::Narrow {
        String::new()
      } else {
        format!("  {:<10}", status_text(&status))
      };
      lines.push(selected_line(
        selected == app.ui().requirements.selected,
        vec![
          Span::styled(
            selected_prefix(selected == app.ui().requirements.selected),
            theme::accent(),
          ),
          Span::styled(
            format!("{} ", requirement_mark(&status)),
            status_style_requirement(&status),
          ),
          Span::styled(format!("{:<10}", requirement.id), theme::code()),
          Span::styled(requirement.title.clone(), theme::secondary()),
          Span::styled(label, theme::muted()),
        ],
      ));
    }
  }
  frame.render_widget(
    Paragraph::new(lines)
      .scroll((app.ui().requirements.scroll, 0))
      .wrap(Wrap { trim: true }),
    list_area,
  );
  if let Some(detail_area) = detail_area {
    requirement_detail_pane(frame, app, detail_area);
  }
}

fn requirement_subtitle(app: &Application) -> String {
  if app.ui().requirements.query.is_empty() {
    format!("{} items", app.catalog().len())
  } else {
    format!("/ {}", app.ui().requirements.query)
  }
}

fn requirement_detail_pane(frame: &mut Frame<'_>, app: &Application, area: Rect) {
  let mut lines = screen_heading("DETAIL", "Why this is not finished");
  let Some(item) = app.selected_requirement() else {
    lines.extend(empty_state(
      "NOTHING SELECTED",
      "Choose a requirement to inspect evidence and gaps.",
    ));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
    return;
  };
  let assessment = app.assessments().get(&item.id);
  let status = requirement_status(assessment);
  lines.extend([
    Line::raw(""),
    Line::styled(item.id.clone(), theme::accent()),
    Line::styled(item.title.clone(), theme::heading()),
    Line::from(vec![Span::styled(
      status_text(&status),
      status_style_requirement(&status),
    )]),
    Line::raw(""),
    section_line("DESCRIPTION"),
    Line::styled(item.description.clone(), theme::secondary()),
    Line::raw(""),
    section_line("GAPS"),
  ]);
  lines.extend(detail_bullets(
    assessment.map(|value| value.gaps.as_slice()).unwrap_or(&[]),
    "No gaps assessed yet",
    theme::failure(),
  ));
  lines.extend([Line::raw(""), section_line("EVIDENCE")]);
  lines.extend(detail_bullets(
    assessment
      .map(|value| value.evidence.as_slice())
      .unwrap_or(&[]),
    "No evidence assessed yet",
    theme::muted(),
  ));
  lines.push(Line::raw(""));
  lines.push(section_line("ACCEPTANCE"));
  lines.extend(detail_bullets(
    &item.acceptance_criteria,
    "No acceptance criteria recorded",
    theme::secondary(),
  ));
  frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
}

fn checks_screen(frame: &mut Frame<'_>, app: &Application, area: Rect, density: Density) {
  let mut lines = screen_heading("CHECKS", checks_subtitle(app));
  let visible = app.visible_checks();
  if visible.is_empty() {
    lines.extend(empty_state(
      "NO CHECKS RECORDED",
      "Verification attempts will appear when the controller reaches verify.",
    ));
  }
  for (selected, index) in visible.iter().enumerate() {
    if let Some(report) = app.checks().get(*index) {
      lines.extend(check_attempt_lines(
        report,
        selected == app.ui().checks.selected,
        selected + 1,
        density,
      ));
    }
  }
  frame.render_widget(
    Paragraph::new(lines)
      .scroll((app.ui().checks.scroll, 0))
      .wrap(Wrap { trim: true }),
    area,
  );
}

fn checks_subtitle(app: &Application) -> String {
  if app.ui().checks.query.is_empty() {
    format!("{} attempts", app.checks().len())
  } else {
    format!("/ {}", app.ui().checks.query)
  }
}

fn check_attempt_lines(
  report: &VerificationReport,
  selected: bool,
  attempt: usize,
  density: Density,
) -> Vec<Line<'static>> {
  let state = if report.passed {
    "✓ PASS"
  } else {
    "✕ FAIL"
  };
  let style = if report.passed {
    theme::success()
  } else {
    theme::failure()
  };
  let mut lines = vec![
    Line::raw(""),
    selected_line(
      selected,
      vec![
        Span::styled(selected_prefix(selected), theme::accent()),
        Span::styled(format!("ATTEMPT {attempt:02}"), theme::heading()),
        Span::styled("   ", theme::muted()),
        Span::styled(state, style),
        Span::styled(
          if density == Density::Narrow {
            String::new()
          } else {
            format!("   {}", display_time(&report.finished_at))
          },
          theme::muted(),
        ),
      ],
    ),
    selected_line(
      selected,
      vec![
        Span::raw("   "),
        Span::styled(format!("{} checks", report.commands.len()), theme::muted()),
        Span::styled(" · ", theme::muted()),
        Span::styled(
          format!(
            "{} → {}",
            display_time(&report.started_at),
            display_time(&report.finished_at)
          ),
          theme::muted(),
        ),
      ],
    ),
  ];
  for command in &report.commands {
    let passed = command.exit_code == Some(0) && !command.timed_out;
    let mark = if passed { "✓" } else { "✕" };
    let style = if passed {
      theme::success()
    } else {
      theme::failure()
    };
    lines.push(selected_line(
      selected,
      vec![
        Span::raw("   "),
        Span::styled(format!("{mark} "), style),
        Span::styled(command.command.clone(), theme::code()),
        Span::styled(
          format!("   {}", duration(command.duration_ms)),
          theme::muted(),
        ),
      ],
    ));
    if !passed {
      let preview = first_line(if command.stderr.is_empty() {
        &command.stdout
      } else {
        &command.stderr
      });
      if !preview.is_empty() {
        lines.push(selected_line(
          selected,
          vec![Span::raw("     "), Span::styled(preview, theme::muted())],
        ));
      }
    }
  }
  lines
}

fn changes_screen(frame: &mut Frame<'_>, app: &Application, area: Rect) {
  let mut lines = screen_heading("CHANGES", changes_subtitle(app));
  let visible = app.visible_changes();
  if visible.is_empty() {
    lines.extend(empty_state(
      "WORKTREE CLEAN",
      "Repository changes will appear here as work is applied.",
    ));
  }
  for (selected, index) in visible.iter().enumerate() {
    if let Some(change) = app.changes().get(*index) {
      let (mark, label, style) = change_style(change.status);
      lines.push(selected_line(
        selected == app.ui().changes.selected,
        vec![
          Span::styled(
            selected_prefix(selected == app.ui().changes.selected),
            theme::accent(),
          ),
          Span::styled(format!("{mark} "), style),
          Span::styled(format!("{label:<9}"), theme::caption()),
          Span::styled(change.path.clone(), theme::secondary()),
        ],
      ));
    }
  }
  frame.render_widget(
    Paragraph::new(lines)
      .scroll((app.ui().changes.scroll, 0))
      .wrap(Wrap { trim: true }),
    area,
  );
}

fn changes_subtitle(app: &Application) -> String {
  if app.ui().changes.query.is_empty() {
    format!("{} paths", app.changes().len())
  } else {
    format!("/ {}", app.ui().changes.query)
  }
}

fn history_screen(frame: &mut Frame<'_>, app: &Application, area: Rect) {
  let subtitle = if app.ui().history.query.is_empty() {
    app.ui().history.filter.label().to_owned()
  } else {
    format!(
      "{} · / {}",
      app.ui().history.filter.label(),
      app.ui().history.query
    )
  };
  let mut lines = screen_heading("HISTORY", subtitle);
  let visible = app.visible_history();
  if visible.is_empty() {
    lines.extend(empty_state(
      "NO MATCHING EVENTS",
      "Clear the filter or wait for controller activity.",
    ));
  }
  for (selected, index) in visible.iter().enumerate() {
    if let Some(item) = app.activities().get(*index) {
      lines.extend(activity_lines(
        item,
        selected == app.ui().history.selected,
        Density::Medium,
        true,
      ));
    }
  }
  frame.render_widget(
    Paragraph::new(lines)
      .scroll((app.ui().history.scroll, 0))
      .wrap(Wrap { trim: true }),
    area,
  );
}

fn render_overlay(frame: &mut Frame<'_>, overlay: &Overlay, area: Rect) {
  let inspector = matches!(overlay, Overlay::Inspector { .. });
  let (title, lines, scroll) = match overlay {
    Overlay::Help => ("HELP", help_lines(), 0),
    Overlay::Palette { query } => ("COMMAND", palette_lines(query), 0),
    Overlay::Search { query } => ("FILTER", search_lines(query), 0),
    Overlay::Inspector {
      title,
      body,
      scroll,
    } => (title.as_str(), inspector_lines(body), *scroll),
    Overlay::ConfirmStop => ("STOP THIS RUN?", confirm_lines(), 0),
  };
  let width = if inspector {
    area.width.saturating_sub(8)
  } else {
    64.min(area.width.saturating_sub(4))
  };
  let requested_height = if inspector {
    area.height.saturating_sub(4)
  } else {
    13
  };
  let popup = layout::centered(
    area,
    width,
    requested_height.min(area.height.saturating_sub(2)),
  );
  frame.render_widget(Clear, popup);
  frame.render_widget(
    Paragraph::new(lines)
      .block(
        Block::default()
          .title(Span::styled(format!(" {title} "), theme::heading()))
          .borders(Borders::ALL)
          .border_style(theme::focused_border()),
      )
      .scroll((scroll, 0))
      .wrap(Wrap { trim: false }),
    popup,
  );
}

fn help_lines() -> Vec<Line<'static>> {
  vec![
    Line::styled("Navigation", theme::heading()),
    Line::styled(
      "g r  Run     g q  Requirements     g c  Checks",
      theme::secondary(),
    ),
    Line::styled("g d  Changes g h  History", theme::secondary()),
    Line::raw(""),
    Line::styled("Actions", theme::heading()),
    Line::styled(
      "j/k move   Enter inspect   / filter   : commands",
      theme::secondary(),
    ),
    Line::styled(
      "End follows live events   q asks before stopping a run",
      theme::secondary(),
    ),
    Line::raw(""),
    Line::styled("Esc closes this help", theme::muted()),
  ]
}

fn palette_lines(query: &str) -> Vec<Line<'static>> {
  let commands = [
    ("Run", "Open live run activity", "g r"),
    ("Requirements", "Browse requirement gaps", "g q"),
    ("Checks", "Review verification evidence", "g c"),
    ("Changes", "Inspect repository paths", "g d"),
    ("History", "Search the audit trail", "g h"),
    ("Start", "Start or resume the controller", "r"),
    ("Help", "Show keyboard reference", "?"),
  ];
  let query_folded = query.to_ascii_lowercase();
  let matches = commands.into_iter().filter(|(name, description, _)| {
    query_folded.is_empty()
      || name.to_ascii_lowercase().contains(&query_folded)
      || description.to_ascii_lowercase().contains(&query_folded)
  });
  let mut lines = vec![
    Line::from(vec![
      Span::styled("› ", theme::accent()),
      Span::styled(format!(":{query}_"), theme::primary()),
    ]),
    Line::raw(""),
  ];
  let mut found = false;
  for (position, (name, description, shortcut)) in matches.enumerate() {
    found = true;
    lines.push(selected_line(
      position == 0,
      vec![
        Span::styled(selected_prefix(position == 0), theme::accent()),
        Span::styled(format!("{name:<16}"), theme::heading()),
        Span::styled(description, theme::secondary()),
        Span::styled(format!("  {shortcut}"), theme::muted()),
      ],
    ));
  }
  if !found {
    lines.push(Line::styled("No matching command", theme::muted()));
  }
  lines.push(Line::raw(""));
  lines.push(Line::styled(
    "Enter runs an exact command · Esc cancels",
    theme::muted(),
  ));
  lines
}

fn search_lines(query: &str) -> Vec<Line<'static>> {
  vec![
    Line::from(vec![
      Span::styled("/ ", theme::accent()),
      Span::styled(format!("{query}_"), theme::primary()),
    ]),
    Line::raw(""),
    Line::styled("Enter applies filter · Esc cancels", theme::muted()),
  ]
}

fn confirm_lines() -> Vec<Line<'static>> {
  vec![
    Line::styled("Stop this run?", theme::heading()),
    Line::raw(""),
    Line::styled(
      "The active worker will be cancelled gracefully.",
      theme::secondary(),
    ),
    Line::raw(""),
    Line::from(vec![
      Span::styled("Esc", theme::heading()),
      Span::styled("  Keep running", theme::muted()),
      Span::styled("                 Enter", theme::failure()),
      Span::styled("  Stop run", theme::secondary()),
    ]),
  ]
}

fn inspector_lines(body: &str) -> Vec<Line<'static>> {
  body
    .lines()
    .map(|line| {
      let style =
        if line.starts_with('$') || line.starts_with("stdout:") || line.starts_with("stderr:") {
          theme::code()
        } else if line.starts_with("FAIL") || line.starts_with("Blocked:") {
          theme::failure()
        } else if line.ends_with(':')
          || matches!(
            line,
            "Objective" | "Requirements" | "Acceptance criteria" | "Evidence" | "Gaps"
          )
        {
          theme::caption()
        } else {
          theme::secondary()
        };
      Line::styled(line.to_owned(), style)
    })
    .collect()
}

fn screen_heading(title: &str, subtitle: impl Into<String>) -> Vec<Line<'static>> {
  vec![Line::from(vec![
    Span::styled(title.to_owned(), theme::heading()),
    Span::styled(format!("  ·  {}", subtitle.into()), theme::muted()),
  ])]
}

fn section_line(label: &str) -> Line<'static> {
  Line::styled(label.to_owned(), theme::caption())
}

fn empty_state(title: &str, detail: &str) -> Vec<Line<'static>> {
  vec![
    Line::raw(""),
    Line::styled(title.to_owned(), theme::heading()),
    Line::styled(detail.to_owned(), theme::muted()),
  ]
}

fn detail_bullets(items: &[String], empty: &str, style: Style) -> Vec<Line<'static>> {
  if items.is_empty() {
    vec![Line::styled(format!("· {empty}"), theme::muted())]
  } else {
    items
      .iter()
      .map(|item| {
        Line::from(vec![
          Span::styled("· ", style),
          Span::styled(item.clone(), theme::secondary()),
        ])
      })
      .collect()
  }
}

fn check_context_line(app: &Application) -> Line<'static> {
  match app.checks().last() {
    Some(report) if report.passed => Line::from(vec![
      Span::styled("✓ ", theme::success()),
      Span::styled("Latest verification passed", theme::secondary()),
    ]),
    Some(report) => Line::from(vec![
      Span::styled("✕ ", theme::failure()),
      Span::styled(failure_preview(report), theme::secondary()),
    ]),
    None => Line::styled("No deterministic check recorded", theme::muted()),
  }
}

fn key_hints(hints: &[(&str, &str)]) -> Vec<Span<'static>> {
  hints
    .iter()
    .enumerate()
    .flat_map(|(index, (key, description))| {
      let mut spans = Vec::new();
      if index > 0 {
        spans.push(Span::styled("   ", theme::muted()));
      }
      spans.push(Span::styled((*key).to_owned(), theme::heading()));
      spans.push(Span::styled(format!(" {description}"), theme::muted()));
      spans
    })
    .collect()
}

fn selected_prefix(selected: bool) -> String {
  if selected {
    "▌ ".into()
  } else {
    "  ".into()
  }
}

fn selected_line(selected: bool, spans: Vec<Span<'static>>) -> Line<'static> {
  Line::from(spans).style(if selected {
    theme::selected()
  } else {
    theme::primary()
  })
}

fn run_status_mark(status: &RunStatus) -> &'static str {
  match status {
    RunStatus::Done => "✓",
    RunStatus::Blocked | RunStatus::Failed => "✕",
    RunStatus::Stopped => "■",
    RunStatus::Running => "●",
    RunStatus::Idle => "○",
  }
}

fn activity_mark(category: ActivityCategory) -> &'static str {
  match category {
    ActivityCategory::Controller => "◆",
    ActivityCategory::Worker => "·",
    ActivityCategory::Check => "✓",
    ActivityCategory::Error => "✕",
  }
}

fn category_style(category: ActivityCategory) -> Style {
  match category {
    ActivityCategory::Controller => theme::accent(),
    ActivityCategory::Worker => theme::muted(),
    ActivityCategory::Check => theme::success(),
    ActivityCategory::Error => theme::failure(),
  }
}

fn status_style(status: &RunStatus) -> Style {
  match status {
    RunStatus::Done => theme::success(),
    RunStatus::Blocked | RunStatus::Failed => theme::failure(),
    RunStatus::Stopped => theme::warning(),
    RunStatus::Running => theme::accent(),
    RunStatus::Idle => theme::muted(),
  }
}

fn status_style_requirement(status: &RequirementStatus) -> Style {
  match status {
    RequirementStatus::Satisfied => theme::success(),
    RequirementStatus::Partial => theme::warning(),
    RequirementStatus::Missing => theme::failure(),
  }
}

fn requirement_mark(status: &RequirementStatus) -> &'static str {
  match status {
    RequirementStatus::Satisfied => "✓",
    RequirementStatus::Partial => "◐",
    RequirementStatus::Missing => "○",
  }
}

fn status_text(status: &RequirementStatus) -> &'static str {
  match status {
    RequirementStatus::Satisfied => "SATISFIED",
    RequirementStatus::Partial => "PARTIAL",
    RequirementStatus::Missing => "MISSING",
  }
}

fn change_style(status: char) -> (&'static str, &'static str, Style) {
  match status {
    'A' => ("A", "ADDED", theme::added()),
    'D' => ("D", "DELETED", theme::deleted()),
    'M' => ("M", "MODIFIED", theme::modified()),
    '?' | 'U' => ("?", "UNTRACKED", theme::warning()),
    _ => ("·", "CHANGED", theme::muted()),
  }
}

fn short_title(title: &str, tool: bool) -> String {
  if tool {
    "TOOL".into()
  } else {
    title.chars().take(14).collect()
  }
}

fn first_line(value: &str) -> String {
  value
    .lines()
    .find(|line| !line.trim().is_empty())
    .unwrap_or_default()
    .trim()
    .to_owned()
}

fn indent(value: &str, spaces: usize) -> String {
  let padding = " ".repeat(spaces);
  value.replace('\n', &format!("\n{padding}"))
}

fn display_time(at: &str) -> &str {
  let time = at.split('T').nth(1).unwrap_or(at);
  time.get(..8).unwrap_or(time)
}

fn duration(milliseconds: u128) -> String {
  if milliseconds >= 1_000 {
    format!("{:.1}s", milliseconds as f64 / 1_000.0)
  } else {
    format!("{milliseconds}ms")
  }
}

fn progress_meter(done: usize, total: usize, width: usize) -> String {
  if total == 0 {
    return format!("{}  0/0", "░".repeat(width));
  }
  let filled = done.saturating_mul(width).div_ceil(total).min(width);
  format!(
    "{}{}  {done}/{total}",
    "█".repeat(filled),
    "░".repeat(width - filled)
  )
}

fn span_width(spans: &[Span<'_>]) -> usize {
  spans.iter().map(|span| span.content.chars().count()).sum()
}
