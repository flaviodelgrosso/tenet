use std::{
  collections::{BTreeMap, BTreeSet},
  path::{Path, PathBuf},
  sync::Arc,
};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
  backend::{AgentBackend, BackendContext},
  config::{ensure_config, Config, LOOPS_DIR},
  events::{EventSink, RunEvent, RunLogger},
  model::{
    CompletedWorkUnit, Phase, ReconcileResult, RequirementCatalog, RequirementCounts,
    RequirementStatus, RunStatus, State, VerificationReport, WorkUnit,
  },
  protection,
  store::{self, RunLock},
  verifier,
};

pub struct Controller {
  cwd: PathBuf,
  backend: Arc<dyn AgentBackend>,
  events: EventSink,
}

impl Controller {
  pub fn new(cwd: PathBuf, backend: Arc<dyn AgentBackend>, events: EventSink) -> Self {
    Self {
      cwd,
      backend,
      events,
    }
  }

  pub async fn initialize(&self) -> Result<State> {
    store::ensure_layout(&self.cwd).await?;
    let config = ensure_config(&self.cwd).await?;
    store::ensure_spec(&self.cwd, &config).await?;
    store::ensure_gitignore(&self.cwd).await?;
    store::maybe_init_git(&self.cwd, &config).await?;
    let state_path = self.cwd.join(LOOPS_DIR).join(store::STATE_FILE);
    if !state_path.exists() {
      let state = State::fresh();
      store::write_state(&self.cwd, &state).await?;
    }
    store::read_state(&self.cwd).await
  }

  pub async fn run(&self, cancel: CancellationToken) -> Result<State> {
    self.initialize().await?;
    let _lock = RunLock::acquire(&self.cwd)?;
    let config = Arc::new(ensure_config(&self.cwd).await?);
    let run_id = format!(
      "{}-{}",
      Utc::now().format("%Y%m%dT%H%M%SZ"),
      &Uuid::new_v4().to_string()[..8]
    );
    let logger =
      Arc::new(RunLogger::create(self.cwd.join(LOOPS_DIR).join("runs").join(&run_id)).await?);
    let events = self.events.clone().with_logger(logger);
    let ctx = BackendContext {
      cwd: self.cwd.clone(),
      runtime_dir: self.cwd.join(LOOPS_DIR).join("runtime").join(&run_id),
      config: config.clone(),
      cancel: cancel.clone(),
      events: events.clone(),
    };

    let mut state = store::read_state(&self.cwd)
      .await
      .unwrap_or_else(|_| State::fresh());
    state.status = RunStatus::Running;
    state.phase = Phase::Architecting;
    state.run_id = Some(run_id);
    state.cycle = 0;
    state.current_work_unit = None;
    state.blocked_reason = None;
    state.last_error = None;
    state.last_summary = "Starting autonomous spec-driven run".into();
    self.publish(&events, &mut state).await?;

    let result = self.run_inner(&ctx, &mut state).await;
    match result {
      Ok(final_state) => Ok(final_state),
      Err(error) => {
        let cancelled = cancel.is_cancelled();
        state.status = if cancelled {
          RunStatus::Stopped
        } else {
          RunStatus::Failed
        };
        state.last_error = if cancelled {
          None
        } else {
          Some(error.to_string())
        };
        state.last_summary = if cancelled {
          "Run stopped".into()
        } else {
          "Run failed".into()
        };
        self.publish(&events, &mut state).await?;
        events.emit(RunEvent::Finished(state.clone())).await;
        if cancelled {
          Ok(state)
        } else {
          Err(error)
        }
      }
    }
  }

  async fn run_inner(&self, ctx: &BackendContext, state: &mut State) -> Result<State> {
    let mut catalog = self.ensure_catalog(ctx, state).await?;
    let mut previous_satisfied = usize::MAX;
    let mut stagnant_fingerprint = String::new();
    let mut stagnant_count = 0u32;

    for cycle in 1..=ctx.config.max_cycles {
      self.check_cancel(ctx)?;
      catalog = self
        .refresh_catalog_if_spec_changed(ctx, state, catalog)
        .await?;
      state.cycle = cycle;
      state.phase = Phase::Reconciling;
      state.current_work_unit = None;
      state.last_summary = format!("Cycle {cycle}: reconciling implementation against spec");
      self.publish(&ctx.events, state).await?;

      let reconciliation = self
        .backend
        .reconcile(ctx, &catalog, &state.completed_work_units)
        .await
        .context("reconciliation worker")?;
      validate_reconcile(&catalog, &reconciliation)?;
      store::write_roadmap(&self.cwd, &reconciliation).await?;
      state.requirement_counts = requirement_counts(&catalog, &reconciliation);
      state.last_summary = reconciliation.summary.clone();
      ctx
        .events
        .emit(RunEvent::Reconcile(reconciliation.clone()))
        .await;
      self.publish(&ctx.events, state).await?;

      if reconciliation.complete
        && all_satisfied(&catalog, &reconciliation)
        && reconciliation.next_work_unit.is_none()
      {
        let final_unit = final_gate_work_unit(&catalog);
        let report = self
          .verify_with_repairs(ctx, state, &catalog, &final_unit, true)
          .await?;
        if !report.passed {
          let reason = state
            .blocked_reason
            .clone()
            .unwrap_or_else(|| "Final deterministic verification did not converge".into());
          return self.block(ctx, state, &reason).await;
        }

        state.phase = Phase::Assessing;
        state.last_summary = "Independent fresh-context completion assessment".into();
        self.publish(&ctx.events, state).await?;
        let assessment = self
          .backend
          .assess(ctx, &catalog)
          .await
          .context("assessment worker")?;
        validate_reconcile(&catalog, &assessment)?;
        store::write_roadmap(&self.cwd, &assessment).await?;
        state.requirement_counts = requirement_counts(&catalog, &assessment);
        state.last_summary = assessment.summary.clone();
        ctx
          .events
          .emit(RunEvent::Reconcile(assessment.clone()))
          .await;
        self.publish(&ctx.events, state).await?;

        if assessment.complete
          && all_satisfied(&catalog, &assessment)
          && assessment.next_work_unit.is_none()
        {
          if ctx.config.git.require_clean_tree && !store::is_git_clean(&self.cwd).await {
            return self
              .block(ctx, state, "Completion requires a clean Git working tree")
              .await;
          }
          state.status = RunStatus::Done;
          state.phase = Phase::Complete;
          state.current_work_unit = None;
          state.last_summary = "All requirements have evidence and deterministic gates pass".into();
          self.publish(&ctx.events, state).await?;
          ctx.events.emit(RunEvent::Finished(state.clone())).await;
          return Ok(state.clone());
        }
        previous_satisfied = state.requirement_counts.satisfied;
        continue;
      }

      let work_unit = match reconciliation.next_work_unit {
        Some(ref wu) => wu.clone(),
        None => {
          return self
            .block(
              ctx,
              state,
              "Reconciliation found gaps but produced no next work unit",
            )
            .await
        }
      };
      validate_work_unit(&catalog, &work_unit)?;

      let fingerprint = work_fingerprint(&work_unit);
      if fingerprint == stagnant_fingerprint
        && state.requirement_counts.satisfied <= previous_satisfied
      {
        stagnant_count += 1;
      } else {
        stagnant_fingerprint = fingerprint;
        stagnant_count = 0;
      }
      previous_satisfied = state.requirement_counts.satisfied;
      if stagnant_count >= ctx.config.stagnation_limit {
        return self
          .block(
            ctx,
            state,
            &format!("Stagnation circuit breaker tripped on {}", work_unit.id),
          )
          .await;
      }

      state.phase = Phase::Implementing;
      state.current_work_unit = Some(work_unit.clone());
      state.last_summary = format!("Implementing {}: {}", work_unit.id, work_unit.title);
      self.publish(&ctx.events, state).await?;

      let protected = protection::snapshot(&self.cwd, &protected_paths(&ctx.config)).await?;
      self
        .backend
        .implement(ctx, &catalog, &work_unit)
        .await
        .context("implementation worker")?;
      store::ensure_gitignore(&self.cwd).await?;
      let changed = protection::restore_changes(&self.cwd, &protected).await?;
      if !changed.is_empty() {
        return self
          .block(
            ctx,
            state,
            &format!(
              "Worker modified controller-protected files; restored: {}",
              changed.join(", ")
            ),
          )
          .await;
      }

      let report = self
        .verify_with_repairs(ctx, state, &catalog, &work_unit, false)
        .await?;
      if !report.passed {
        let reason = state
          .blocked_reason
          .clone()
          .unwrap_or_else(|| format!("Verification did not converge for {}", work_unit.id));
        return self.block(ctx, state, &reason).await;
      }
      let evidence_path = store::save_evidence(
        &self.cwd,
        &format!("{}-{}", state.cycle, work_unit.id),
        &report,
      )
      .await?;
      state.completed_work_units.push(CompletedWorkUnit {
        work_unit: work_unit.clone(),
        completed_at: Utc::now().to_rfc3339(),
        verification_evidence: evidence_path
          .strip_prefix(&self.cwd)
          .unwrap_or(&evidence_path)
          .display()
          .to_string(),
      });
      state.current_work_unit = None;
      if ctx.config.git.auto_commit {
        store::auto_commit(&self.cwd, &format!("loops: complete {}", work_unit.id)).await?;
      }
      self.publish(&ctx.events, state).await?;
    }

    self
      .block(
        ctx,
        state,
        &format!("Maximum cycle count ({}) reached", ctx.config.max_cycles),
      )
      .await
  }

  async fn ensure_catalog(
    &self,
    ctx: &BackendContext,
    state: &mut State,
  ) -> Result<RequirementCatalog> {
    let (spec, spec_hash) = store::spec_text_and_hash(&self.cwd, &ctx.config).await?;
    if let Some(catalog) = store::read_catalog(&self.cwd).await? {
      if catalog.spec_hash == spec_hash {
        return Ok(catalog);
      }
    }
    state.phase = Phase::Architecting;
    state.last_summary = "Deriving requirement catalog from spec.md".into();
    self.publish(&ctx.events, state).await?;
    let output = self
      .backend
      .architect(ctx, &spec)
      .await
      .context("architect worker")?;
    validate_requirements(&output.requirements)?;
    let catalog = RequirementCatalog {
      spec_hash,
      requirements: output.requirements,
    };
    store::write_catalog(&self.cwd, &catalog).await?;
    state.requirement_counts = RequirementCounts {
      total: catalog.requirements.len(),
      ..Default::default()
    };
    self.publish(&ctx.events, state).await?;
    Ok(catalog)
  }

  async fn refresh_catalog_if_spec_changed(
    &self,
    ctx: &BackendContext,
    state: &mut State,
    catalog: RequirementCatalog,
  ) -> Result<RequirementCatalog> {
    let (_, hash) = store::spec_text_and_hash(&self.cwd, &ctx.config).await?;
    if hash == catalog.spec_hash {
      return Ok(catalog);
    }
    state.completed_work_units.clear();
    self.ensure_catalog(ctx, state).await
  }

  async fn verify_with_repairs(
    &self,
    ctx: &BackendContext,
    state: &mut State,
    catalog: &RequirementCatalog,
    work_unit: &WorkUnit,
    final_gate: bool,
  ) -> Result<VerificationReport> {
    state.phase = Phase::Verifying;
    state.last_summary = format!("Verifying {}", work_unit.id);
    self.publish(&ctx.events, state).await?;

    let mut report = verifier::run_verification(&self.cwd, &ctx.config).await?;
    ctx
      .events
      .emit(RunEvent::Verification(report.clone()))
      .await;
    let label = if final_gate {
      "final".to_owned()
    } else {
      work_unit.id.clone()
    };
    let _ = store::save_evidence(&self.cwd, &format!("verify-{label}-attempt-0"), &report).await?;
    if report.passed {
      state.last_summary = format!("Verification passed for {}", work_unit.id);
      self.publish(&ctx.events, state).await?;
      return Ok(report);
    }

    for attempt in 1..=ctx.config.max_repair_attempts {
      self.check_cancel(ctx)?;
      state.phase = Phase::Repairing;
      state.last_summary = format!(
        "Repairing {} (attempt {}/{})",
        work_unit.id, attempt, ctx.config.max_repair_attempts
      );
      self.publish(&ctx.events, state).await?;

      let protected = protection::snapshot(&self.cwd, &protected_paths(&ctx.config)).await?;
      self
        .backend
        .repair(ctx, catalog, work_unit, &report)
        .await
        .context("repair worker")?;
      store::ensure_gitignore(&self.cwd).await?;
      let changed = protection::restore_changes(&self.cwd, &protected).await?;
      if !changed.is_empty() {
        let reason = format!(
          "Repair worker modified controller-protected files; restored: {}",
          changed.join(", ")
        );
        state.status = RunStatus::Blocked;
        state.blocked_reason = Some(reason.clone());
        state.last_summary = reason;
        self.publish(&ctx.events, state).await?;
        return Ok(report);
      }

      state.phase = Phase::Verifying;
      self.publish(&ctx.events, state).await?;
      report = verifier::run_verification(&self.cwd, &ctx.config).await?;
      ctx
        .events
        .emit(RunEvent::Verification(report.clone()))
        .await;
      let _ = store::save_evidence(
        &self.cwd,
        &format!("verify-{label}-attempt-{attempt}"),
        &report,
      )
      .await?;
      if report.passed {
        state.last_summary = format!("Verification passed for {}", work_unit.id);
        self.publish(&ctx.events, state).await?;
        return Ok(report);
      }
    }
    Ok(report)
  }

  async fn block(&self, ctx: &BackendContext, state: &mut State, reason: &str) -> Result<State> {
    state.status = RunStatus::Blocked;
    state.blocked_reason = Some(reason.into());
    state.last_summary = reason.into();
    self.publish(&ctx.events, state).await?;
    ctx.events.emit(RunEvent::Finished(state.clone())).await;
    Ok(state.clone())
  }

  async fn publish(&self, events: &EventSink, state: &mut State) -> Result<()> {
    state.updated_at = Utc::now().to_rfc3339();
    store::write_state(&self.cwd, state).await?;
    events.emit(RunEvent::State(state.clone())).await;
    Ok(())
  }

  fn check_cancel(&self, ctx: &BackendContext) -> Result<()> {
    if ctx.cancel.is_cancelled() {
      bail!("run cancelled");
    }
    Ok(())
  }
}

fn validate_requirements(requirements: &[crate::model::Requirement]) -> Result<()> {
  if requirements.is_empty() {
    bail!("architect produced no requirements");
  }
  for (index, requirement) in requirements.iter().enumerate() {
    let expected = format!("REQ-{:03}", index + 1);
    if requirement.id != expected {
      bail!(
        "requirement id {} is unstable; expected {expected}",
        requirement.id
      );
    }
    if requirement.title.trim().is_empty()
      || requirement.description.trim().is_empty()
      || requirement.acceptance_criteria.is_empty()
    {
      bail!(
        "{} is missing title, description, or acceptance criteria",
        requirement.id
      );
    }
  }
  Ok(())
}

fn validate_reconcile(catalog: &RequirementCatalog, result: &ReconcileResult) -> Result<()> {
  let expected: BTreeSet<_> = catalog.requirements.iter().map(|r| r.id.as_str()).collect();
  let actual: BTreeSet<_> = result.requirements.iter().map(|r| r.id.as_str()).collect();
  if expected != actual {
    bail!("reconciliation assessment IDs do not match the requirement catalog");
  }
  if result.complete && (!all_satisfied(catalog, result) || result.next_work_unit.is_some()) {
    bail!(
      "reconciliation claimed complete without every requirement satisfied and nextWorkUnit=null"
    );
  }
  if let Some(work) = &result.next_work_unit {
    validate_work_unit(catalog, work)?;
  }
  Ok(())
}

fn validate_work_unit(catalog: &RequirementCatalog, work: &WorkUnit) -> Result<()> {
  if work.id.trim().is_empty() || work.title.trim().is_empty() || work.objective.trim().is_empty() {
    bail!("work unit is missing id, title, or objective");
  }
  if !work
    .id
    .chars()
    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    || work.id == "."
    || work.id == ".."
  {
    bail!("work unit id contains unsafe path characters: {}", work.id);
  }
  if work.requirement_ids.is_empty() {
    bail!("{} targets no requirements", work.id);
  }
  let known: BTreeSet<_> = catalog.requirements.iter().map(|r| r.id.as_str()).collect();
  for id in &work.requirement_ids {
    if !known.contains(id.as_str()) {
      bail!("{} targets unknown requirement {id}", work.id);
    }
  }
  Ok(())
}

fn all_satisfied(catalog: &RequirementCatalog, result: &ReconcileResult) -> bool {
  if result.requirements.len() != catalog.requirements.len() {
    return false;
  }
  result
    .requirements
    .iter()
    .all(|r| r.status == RequirementStatus::Satisfied)
}

fn requirement_counts(catalog: &RequirementCatalog, result: &ReconcileResult) -> RequirementCounts {
  let mut by_id = BTreeMap::new();
  for item in &result.requirements {
    by_id.insert(item.id.as_str(), &item.status);
  }
  let mut counts = RequirementCounts {
    total: catalog.requirements.len(),
    ..Default::default()
  };
  for req in &catalog.requirements {
    match by_id.get(req.id.as_str()).copied() {
      Some(RequirementStatus::Satisfied) => counts.satisfied += 1,
      Some(RequirementStatus::Partial) => counts.partial += 1,
      _ => counts.missing += 1,
    }
  }
  counts
}

fn work_fingerprint(work: &WorkUnit) -> String {
  format!(
    "{}|{}|{}",
    work.title,
    work.objective,
    work.requirement_ids.join(",")
  )
}

fn final_gate_work_unit(catalog: &RequirementCatalog) -> WorkUnit {
  WorkUnit {
        id: "FINAL-GATE".into(),
        title: "Final deterministic acceptance".into(),
        objective: "Repair any remaining issue that prevents the project's deterministic acceptance gates from passing.".into(),
        requirement_ids: catalog.requirements.iter().map(|r| r.id.clone()).collect(),
        acceptance_criteria: vec!["All configured and auto-detected deterministic gates pass without weakening them.".into()],
        suggested_checks: Vec::new(),
    }
}

fn protected_paths(config: &Config) -> Vec<String> {
  let mut paths = config.protected_paths.clone();
  if !paths.iter().any(|p| p == &config.spec_file) {
    paths.push(config.spec_file.clone());
  }
  paths
}

pub async fn manual_verify(cwd: &Path) -> Result<VerificationReport> {
  let config = ensure_config(cwd).await?;
  let report = verifier::run_verification(cwd, &config).await?;
  let _ = store::save_evidence(cwd, &format!("manual-{}", Utc::now().timestamp()), &report).await?;
  Ok(report)
}
