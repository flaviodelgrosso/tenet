use super::*;

pub(crate) struct ConsoleRenderer<W: Write> {
  writer: W,
  mode: InformationMode,
  style: SemanticStyle,
  header_rendered: bool,
  completion_gate: Option<CompletionGate>,
}

impl ConsoleRenderer<io::Stdout> {
  pub(crate) fn stdout(mode: InformationMode) -> Self {
    let stdout = io::stdout();
    let style = SemanticStyle::auto(stdout.is_terminal());
    Self::new(stdout, mode, style)
  }
}

impl<W: Write> ConsoleRenderer<W> {
  pub(crate) fn new(writer: W, mode: InformationMode, style: SemanticStyle) -> Self {
    Self {
      writer,
      mode,
      style,
      header_rendered: false,
      completion_gate: None,
    }
  }

  pub(crate) fn header(&mut self, header: &RunHeader) -> Result<()> {
    if self.header_rendered {
      return Ok(());
    }
    self.header_rendered = true;
    writeln!(self.writer, "TENET · autonomous engineering run\n")?;
    if let Some(repository) = &header.repository {
      writeln!(self.writer, "repository     {}", self.safe_text(repository))?;
    }
    writeln!(
      self.writer,
      "revision       {}",
      self.display_revision(&header.revision)
    )?;
    writeln!(
      self.writer,
      "specification  {}",
      self.safe_text(&header.specification)
    )?;
    if let Some(agent) = &header.agent {
      writeln!(self.writer, "agent          {}", self.safe_text(agent))?;
    }
    if let Some(requirements) = header.requirements {
      writeln!(self.writer, "requirements   {requirements}")?;
    }
    writeln!(
      self.writer,
      "verification   {} project check(s)",
      header.verification_checks
    )?;
    writeln!(self.writer, "max cycles     {}\n", header.max_cycles)?;
    self.writer.flush()?;
    Ok(())
  }

  pub(crate) fn render(&mut self, event: &ConsoleEvent) -> Result<()> {
    if self.mode == InformationMode::Quiet && !event.is_outcome_changing() {
      return Ok(());
    }
    if self.mode != InformationMode::Verbose && matches!(event, ConsoleEvent::Diagnostic { .. }) {
      return Ok(());
    }
    match event {
      ConsoleEvent::CycleStarted(cycle) => {
        writeln!(
          self.writer,
          "---------- Cycle {cycle} ----------------------------------------\n"
        )?;
      }
      ConsoleEvent::Milestone { at, label, summary } => {
        self.entry(at, Tone::Milestone, label, summary, &[])?;
      }
      ConsoleEvent::WorkStarted {
        at,
        work,
        worker_id,
        lease_id,
      } => {
        let mut details = Vec::new();
        let authority = work
          .requirement_ids
          .iter()
          .map(ToString::to_string)
          .chain(work.scope.paths.iter().cloned())
          .collect::<Vec<_>>()
          .join(" · ");
        if !authority.is_empty() {
          details.push(authority);
        }
        if self.mode.includes_diagnostics() {
          details.push(format!(
            "worker {} · lease {}",
            worker_id.as_deref().unwrap_or("unknown"),
            lease_id.as_deref().unwrap_or("unknown")
          ));
        }
        self.entry(at, Tone::Active, &work.id, &work.title, &details)?;
      }
      ConsoleEvent::RepairStarted {
        at,
        work_unit_id,
        attempt,
        max_attempts,
        reason,
      } => self.entry(
        at,
        Tone::Warning,
        "REPAIR",
        &format!("{work_unit_id} · attempt {attempt}/{max_attempts}"),
        &[format!("reason: {reason}")],
      )?,
      ConsoleEvent::CandidateCreated { at, execution } => {
        let mut details = vec![format!(
          "base {} · {} changed path(s)",
          self.display_revision(&execution.base_revision),
          execution.changed_paths.len()
        )];
        details.extend(
          execution
            .changed_paths
            .iter()
            .take(MAX_CHANGE_PATHS)
            .cloned(),
        );
        if execution.changed_paths.len() > MAX_CHANGE_PATHS {
          details.push(format!(
            "... {} more",
            execution.changed_paths.len() - MAX_CHANGE_PATHS
          ));
        }
        self.entry(
          at,
          Tone::Milestone,
          "CANDIDATE",
          &format!(
            "{} -> {}",
            execution.lease.work_unit.id,
            self.display_revision(&execution.candidate_revision)
          ),
          &details,
        )?;
      }
      ConsoleEvent::Changes { at, changes } => self.render_changes(at, changes)?,
      ConsoleEvent::Verification {
        at,
        report,
        next_action,
      } => self.render_project_verification(at, report, next_action)?,
      ConsoleEvent::AdvisoryVerification { at, report } => {
        let tone = if report.passed {
          Tone::Success
        } else {
          Tone::Failure
        };
        let summary = if report.passed {
          format!(
            "candidate checks {}/{}",
            report.commands.len(),
            report.commands.len()
          )
        } else {
          format!(
            "candidate verification failed · {}",
            failure_preview(report)
          )
        };
        let details = if self.mode.includes_diagnostics() {
          report
            .commands
            .iter()
            .flat_map(|command| {
              bounded_lines(
                &format!(
                  "{} · exit {:?} · {}ms\n{}\n{}",
                  command.command,
                  command.exit_code,
                  command.duration_ms,
                  command.stdout,
                  command.stderr
                ),
                MAX_VERBOSE_OUTPUT_CHARS,
              )
            })
            .collect()
        } else {
          Vec::new()
        };
        self.entry(at, tone, "VERIFY", &summary, &details)?;
      }
      ConsoleEvent::IntegrationStarted {
        at,
        work_unit_id,
        candidate_revision,
      } => self.entry(
        at,
        Tone::Milestone,
        "INTEGRATE",
        &format!(
          "{work_unit_id} · candidate {}",
          self.display_revision(candidate_revision)
        ),
        &[],
      )?,
      ConsoleEvent::WorkIntegrated {
        at,
        work_unit_id,
        title,
        revision,
        changed_paths,
        elapsed_seconds,
      } => {
        let mut details = Vec::new();
        let mut metadata = Vec::new();
        if *changed_paths > 0 {
          metadata.push(format!("{changed_paths} files"));
        }
        if let Some(seconds) = elapsed_seconds {
          metadata.push(format_duration(*seconds));
        }
        if !metadata.is_empty() {
          details.push(metadata.join(" · "));
        }
        self.entry(
          at,
          Tone::Success,
          work_unit_id,
          &format!(
            "{}integrated -> {}",
            title
              .as_ref()
              .map_or(String::new(), |title| format!("{title} · ")),
            self.display_revision(revision)
          ),
          &details,
        )?;
      }
      ConsoleEvent::IntegrationRejected {
        at,
        work_unit_id,
        reason,
      } => self.entry(
        at,
        Tone::Failure,
        "INTEGRATE",
        work_unit_id,
        std::slice::from_ref(reason),
      )?,
      ConsoleEvent::SemanticAssessment { at, report } => {
        self.render_semantic_assessment(at, report)?;
      }
      ConsoleEvent::SemanticEvidence { at, evidence } => {
        let mut details = wrap_text(&evidence.rationale, 68);
        if !evidence.evidence_refs.is_empty() {
          details.push(format!("evidence: {}", evidence.evidence_refs.join(" · ")));
        }
        let (tone, label) = match evidence.result {
          EvidenceResult::Failed => (Tone::Failure, "GAP"),
          EvidenceResult::Inconclusive => (Tone::Warning, "UNCERTAIN"),
          EvidenceResult::Passed => (Tone::Success, "EVIDENCE"),
        };
        self.entry(at, tone, label, evidence.obligation_id.as_ref(), &details)?;
      }
      ConsoleEvent::StaleEvidence {
        at,
        evidence_id,
        revision,
      } => self.entry(
        at,
        Tone::Warning,
        "STALE",
        evidence_id,
        &[format!(
          "repository advanced to {}",
          self.display_revision(revision)
        )],
      )?,
      ConsoleEvent::Contradiction {
        at,
        obligation_id,
        evidence_count,
      } => self.entry(
        at,
        Tone::Failure,
        "CONTRADICT",
        obligation_id,
        &[format!("{evidence_count} conflicting evidence item(s)")],
      )?,
      ConsoleEvent::Progress(progress) => self.entry(
        &now(),
        Tone::Milestone,
        "PROGRESS",
        &format!(
          "requirements {}/{} · semantic {}/{} · work {}/{} · cycle {}",
          progress.requirements_verified,
          progress.requirements_total,
          progress.semantic_satisfied,
          progress.semantic_total,
          progress.work_completed,
          progress.work_total,
          progress.cycle
        ),
        &[],
      )?,
      ConsoleEvent::CompletionGate(gate) => {
        self.completion_gate = Some(gate.clone());
        self.render_completion_gate(gate)?;
      }
      ConsoleEvent::Error {
        at,
        label,
        summary,
        detail,
      } => self.entry(
        at,
        Tone::Failure,
        label,
        summary,
        &detail.iter().cloned().collect::<Vec<_>>(),
      )?,
      ConsoleEvent::Diagnostic {
        at,
        label,
        summary,
        detail,
      } => self.entry(
        at,
        Tone::Secondary,
        label,
        summary,
        &bounded_lines(detail, MAX_VERBOSE_OUTPUT_CHARS),
      )?,
    }
    self.writer.flush()?;
    Ok(())
  }

  pub(crate) fn summary(&mut self, state: &State, elapsed_seconds: u64) -> Result<()> {
    let (tone, label, explanation) = match state.status {
      RunStatus::Done => (Tone::Success, "DONE", "Repository earned completion"),
      RunStatus::ReviewRequired => (
        Tone::Warning,
        "REVIEW REQUIRED",
        "Requirements are structurally complete but need human approval",
      ),
      RunStatus::Blocked => (
        Tone::Failure,
        "BLOCKED",
        "Completion cannot currently be earned",
      ),
      RunStatus::Failed => (Tone::Failure, "FAILED", "Run failed before completion"),
      RunStatus::Stopped => (Tone::Warning, "STOPPED", "Run stopped before completion"),
      RunStatus::Idle | RunStatus::Running => (
        Tone::Warning,
        "INCOMPLETE",
        "Run ended without a terminal controller state",
      ),
    };
    writeln!(
      self.writer,
      "\n{} {}\n  {explanation}\n",
      self.style.marker(tone),
      self.style.label(label, tone)
    )?;
    if let Some(gate) = &self.completion_gate {
      writeln!(
        self.writer,
        "  revision        {}",
        self.display_revision(&gate.revision)
      )?;
    }
    writeln!(
      self.writer,
      "  elapsed         {}",
      format_duration(elapsed_seconds)
    )?;
    writeln!(self.writer, "  cycles          {}", state.cycle)?;
    writeln!(
      self.writer,
      "  work units      {} completed\n",
      state.completed_work_units.len()
    )?;
    writeln!(
      self.writer,
      "  requirements    {}/{} verified",
      state.requirement_counts.verified, state.requirement_counts.total
    )?;
    writeln!(
      self.writer,
      "  project checks  {}/{} {}",
      state.verification_layers.project_checks_passed,
      state.verification_layers.project_checks_total,
      if state.verification_layers.project_passed {
        "passed"
      } else {
        "not passing"
      }
    )?;
    writeln!(
      self.writer,
      "  semantic        {}/{} satisfied",
      state.verification_layers.semantic_satisfied,
      state.verification_layers.semantic_obligations_total
    )?;
    writeln!(
      self.writer,
      "  contradictions  {}",
      state.verification_layers.contradictions
    )?;
    writeln!(
      self.writer,
      "  uncertain       {}",
      state.verification_layers.semantic_uncertain
    )?;
    writeln!(
      self.writer,
      "  stale           {}",
      state.requirement_counts.stale
    )?;
    if let Some(reason) = state.blocked_reason.as_ref().or(state.last_error.as_ref()) {
      let reason = compact(&sanitize_terminal_text(reason), self.output_limit());
      writeln!(
        self.writer,
        "\n  blocker\n    {}",
        indent_lines(&reason, "    ")
      )?;
    }
    match state.status {
      RunStatus::ReviewRequired => writeln!(
        self.writer,
        "\n  review\n    tenet requirements dump\n\n  approve\n    tenet requirements approve"
      )?,
      RunStatus::Blocked => writeln!(
        self.writer,
        "\n  next\n    Address the blocker, then run `tenet resume`."
      )?,
      RunStatus::Stopped => writeln!(self.writer, "\n  next\n    Run `tenet resume` to continue.")?,
      RunStatus::Idle | RunStatus::Running | RunStatus::Done | RunStatus::Failed => {}
    }
    self.writer.flush()?;
    Ok(())
  }

  fn render_project_verification(
    &mut self,
    at: &str,
    report: &ProjectVerificationRun,
    next_action: &str,
  ) -> Result<()> {
    let passed = report
      .checks
      .iter()
      .filter(|check| check.result.exit_code == Some(0) && !check.result.timed_out)
      .count();
    if report.passed {
      let details = if self.mode.includes_diagnostics() {
        report
          .checks
          .iter()
          .map(|check| {
            format!(
              "{} · {} · {}",
              check.name,
              check.result.command,
              format_millis(check.result.duration_ms)
            )
          })
          .collect()
      } else {
        Vec::new()
      };
      return self.entry(
        at,
        Tone::Success,
        "VERIFY",
        &format!("project checks {passed}/{}", report.checks.len()),
        &details,
      );
    }
    let Some(check) = report
      .checks
      .iter()
      .find(|check| check.result.exit_code != Some(0) || check.result.timed_out)
    else {
      return self.entry(
        at,
        Tone::Failure,
        "VERIFY",
        "project verification failed",
        &[],
      );
    };
    let status = if check.result.timed_out {
      format!("timeout after {}s", check.timeout_secs)
    } else {
      check
        .result
        .exit_code
        .map_or_else(|| "no exit code".into(), |code| code.to_string())
    };
    let mut details = vec![
      format!("check       {}", check.name),
      format!("command     {}", check.result.command),
      format!("exit        {status}"),
      format!("duration    {}", format_millis(check.result.duration_ms)),
    ];
    let output = if check.result.stderr.trim().is_empty() {
      &check.result.stdout
    } else {
      &check.result.stderr
    };
    let limit = if self.mode.includes_diagnostics() {
      MAX_VERBOSE_OUTPUT_CHARS
    } else {
      MAX_DEFAULT_OUTPUT_CHARS
    };
    if !output.trim().is_empty() {
      let excerpt = compact(&sanitize_terminal_text(output), limit);
      details.push(format!(
        "failure     {}",
        indent_lines(&excerpt, "            ")
      ));
    }
    details.push(format!("next        {next_action}"));
    self.entry(
      at,
      Tone::Failure,
      "VERIFY",
      "project verification failed",
      &details,
    )
  }

  fn render_semantic_assessment(
    &mut self,
    at: &str,
    report: &SemanticAssessmentReport,
  ) -> Result<()> {
    let total = report.assessments.len();
    let satisfied = report
      .assessments
      .iter()
      .filter(|item| matches!(item.assessment, AssessmentJudgment::Supported { .. }))
      .count();
    for item in &report.assessments {
      match &item.assessment {
        AssessmentJudgment::Contradicted { rationale, .. } => self.entry(
          at,
          Tone::Failure,
          "SUSPECTED",
          item.obligation_id.as_ref(),
          &wrap_text(rationale, 68),
        )?,
        AssessmentJudgment::Insufficient { reason, .. } => self.entry(
          at,
          Tone::Warning,
          "INSUFFICIENT",
          item.obligation_id.as_ref(),
          &wrap_text(reason, 68),
        )?,
        AssessmentJudgment::Supported { .. } => {}
      }
    }
    self.entry(
      at,
      if satisfied == total {
        Tone::Success
      } else {
        Tone::Warning
      },
      "ASSESS",
      &format!("semantic obligations {satisfied}/{total}"),
      &[],
    )
  }

  fn render_changes(&mut self, at: &str, changes: &[RepositoryChange]) -> Result<()> {
    let details = changes
      .iter()
      .take(MAX_CHANGE_PATHS)
      .map(|change| format!("{} {}", change.status, change.path))
      .chain((changes.len() > MAX_CHANGE_PATHS).then(|| {
        format!(
          "... {} more",
          changes.len().saturating_sub(MAX_CHANGE_PATHS)
        )
      }))
      .collect::<Vec<_>>();
    self.entry(
      at,
      Tone::Success,
      "CHANGES",
      &format!("{} files", changes.len()),
      &details,
    )
  }

  fn render_completion_gate(&mut self, gate: &CompletionGate) -> Result<()> {
    writeln!(self.writer, "\nCompletion gate\n")?;
    for item in &gate.items {
      let (tone, marker) = match item.outcome {
        CompletionGateOutcome::Satisfied => (Tone::Success, Tone::Success),
        CompletionGateOutcome::Unsatisfied => (Tone::Warning, Tone::Warning),
        CompletionGateOutcome::Unknown => (Tone::Warning, Tone::Warning),
      };
      let label = self.safe_text(&item.label);
      let detail = self.safe_text(&item.detail);
      writeln!(
        self.writer,
        "  {} {label:<28} {}",
        self.style.marker(marker),
        self.style.label(&detail, tone)
      )?;
    }
    if gate.earned {
      writeln!(
        self.writer,
        "\n{} {} earned at {}",
        self.style.marker(Tone::Success),
        self.style.label("DONE", Tone::Success),
        self.style.metadata(&self.display_revision(&gate.revision))
      )?;
    } else {
      writeln!(
        self.writer,
        "\n{} {}",
        self.style.marker(Tone::Failure),
        self.style.label("BLOCKED", Tone::Failure)
      )?;
      for blocker in gate.blockers.iter().take(4) {
        writeln!(self.writer, "  {}", self.safe_text(blocker))?;
      }
      if gate.blockers.len() > 4 {
        writeln!(self.writer, "  ... {} more", gate.blockers.len() - 4)?;
      }
    }
    Ok(())
  }

  fn entry(
    &mut self,
    at: &str,
    tone: Tone,
    label: &str,
    summary: &str,
    details: &[String],
  ) -> Result<()> {
    let summary = self.safe_text(summary);
    let details = bounded_lines(&details.join("\n"), self.output_limit());
    let label = format!("{label:<10}");
    writeln!(
      self.writer,
      "{} | {} {} {}",
      self.style.metadata(timestamp(at)),
      self.style.marker(tone),
      self.style.label(&label, tone),
      summary
    )?;
    for detail in details {
      writeln!(self.writer, "         |              {detail}")?;
    }
    writeln!(self.writer)?;
    Ok(())
  }

  fn output_limit(&self) -> usize {
    if self.mode.includes_diagnostics() {
      MAX_VERBOSE_OUTPUT_CHARS
    } else {
      MAX_DEFAULT_OUTPUT_CHARS
    }
  }

  fn safe_text(&self, text: &str) -> String {
    let sanitized = sanitize_terminal_text(text);
    let single_line = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    compact(&single_line, self.output_limit())
  }

  fn display_revision(&self, revision: &str) -> String {
    if self.mode.includes_diagnostics() {
      revision.to_owned()
    } else {
      short_revision(revision)
    }
  }

  #[cfg(test)]
  pub(super) fn into_inner(self) -> W {
    self.writer
  }
}

pub(crate) fn format_duration(seconds: u64) -> String {
  if seconds >= 60 {
    format!("{}m {:02}s", seconds / 60, seconds % 60)
  } else {
    format!("{seconds}s")
  }
}

fn format_millis(milliseconds: u128) -> String {
  if milliseconds >= 1_000 {
    format!("{:.1}s", milliseconds as f64 / 1_000.0)
  } else {
    format!("{milliseconds}ms")
  }
}

fn timestamp(value: &str) -> &str {
  value
    .rsplit('T')
    .next()
    .unwrap_or(value)
    .split('+')
    .next()
    .unwrap_or(value)
    .split('.')
    .next()
    .unwrap_or(value)
    .trim_end_matches('Z')
}

fn short_revision(revision: &str) -> String {
  revision.chars().take(7).collect()
}

pub(super) fn now() -> String {
  chrono::Local::now().format("%H:%M:%S").to_string()
}

fn bounded_lines(text: &str, max_chars: usize) -> Vec<String> {
  compact(&sanitize_terminal_text(text), max_chars)
    .lines()
    .take(24)
    .map(str::to_owned)
    .collect()
}

fn indent_lines(text: &str, prefix: &str) -> String {
  text.replace('\n', &format!("\n{prefix}"))
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
  let mut lines = Vec::new();
  let mut line = String::new();
  for word in text.split_whitespace() {
    if !line.is_empty() && line.len() + word.len() + 1 > width {
      lines.push(line);
      line = String::new();
    }
    if !line.is_empty() {
      line.push(' ');
    }
    line.push_str(word);
  }
  if !line.is_empty() {
    lines.push(line);
  }
  lines
}
