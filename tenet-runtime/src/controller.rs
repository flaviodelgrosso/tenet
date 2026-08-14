use std::{
  collections::{BTreeMap, BTreeSet},
  future::Future,
  path::{Path, PathBuf},
  sync::Arc,
};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use tenet_domain::{
  config::{config_path, ensure_config, read_config, Config, TENET_DIR},
  events::{EventSink, RunEvent, RunLogger},
  model::{
    CompletedWorkUnit, Phase, ReconcileResult, RequirementCatalog, RequirementCounts,
    RequirementStatus, RunStatus, State, VerificationReport, WorkExecution, WorkStatus,
  },
};

use crate::{
  backend::{AgentBackend, BackendContext},
  git,
  graph::WorkGraph,
  integration::{deterministic_order, IntegrationOutcome, Integrator},
  protection,
  scheduler::Scheduler,
  store::{self, RunLock},
  verifier,
  workspace::WorkspaceManager,
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
    let state_path = self.cwd.join(TENET_DIR).join(store::STATE_FILE);
    if !state_path.exists() {
      store::write_state(&self.cwd, &State::fresh()).await?;
    }
    store::read_state(&self.cwd).await
  }

  pub async fn run(&self, cancel: CancellationToken) -> Result<State> {
    let path = config_path(&self.cwd);
    if !path.exists() {
      bail!("no tenet.toml launch source configured; run `tenet init`, then select a Registry agent or configure a custom ACP command")
    }
    let configured = read_config(&self.cwd).await?;
    configured.agent.validate_launch_source()?;
    let launch = self.backend.resolve_launch(&self.cwd, &configured).await?;
    self.initialize().await?;
    let _lock = RunLock::acquire(&self.cwd)?;
    let config = Arc::new(ensure_config(&self.cwd).await?);
    git::head(&self.cwd).await?;
    if !git::is_clean(&self.cwd).await? {
      bail!("worktree execution requires a clean canonical working tree");
    }

    let run_id = format!(
      "{}-{}",
      Utc::now().format("%Y%m%dT%H%M%SZ"),
      &Uuid::new_v4().to_string()[..8]
    );
    let logger =
      Arc::new(RunLogger::create(self.cwd.join(TENET_DIR).join("runs").join(&run_id)).await?);
    let events = self.events.clone().with_logger(logger);
    let context = BackendContext {
      cwd: self.cwd.clone(),
      runtime_dir: self.cwd.join(TENET_DIR).join("runtime").join(&run_id),
      config: config.clone(),
      cancel: cancel.clone(),
      events: events.clone(),
      launch,
      worker_id: format!("controller-{run_id}"),
      lease_id: None,
      work_unit_id: None,
    };
    let mut state = store::read_state(&self.cwd).await?;
    state.version = State::VERSION;
    state.status = RunStatus::Running;
    state.phase = Phase::Architecting;
    state.run_id = Some(run_id.clone());
    state.cycle = 0;
    state.active_leases.clear();
    state.candidate_integrations.clear();
    state.work_statuses.clear();
    state.blocked_reason = None;
    state.last_error = None;
    state.last_summary = "Starting deterministic coordinated run".into();
    self.publish(&events, &mut state).await?;

    let workspaces = WorkspaceManager::new(self.cwd.clone(), &run_id);
    let result = self.run_inner(&context, &mut state).await;
    let cleanup = workspaces.cleanup_run().await;
    match (result, cleanup) {
      (Ok(final_state), Ok(())) => Ok(final_state),
      (Ok(_), Err(error)) => Err(error.context("run workspace cleanup")),
      (Err(error), cleanup) => {
        state.active_leases.clear();
        state.candidate_integrations.clear();
        let cancelled = cancel.is_cancelled();
        state.status = if cancelled {
          RunStatus::Stopped
        } else {
          RunStatus::Failed
        };
        state.last_error = (!cancelled).then(|| error.to_string());
        state.last_summary = if cancelled {
          "Run stopped".into()
        } else {
          "Run failed".into()
        };
        if let Err(cleanup_error) = cleanup {
          state.last_error = Some(format!(
            "{}; workspace cleanup failed: {cleanup_error:#}",
            state.last_error.unwrap_or_else(|| error.to_string())
          ));
        }
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

  async fn run_inner(&self, context: &BackendContext, state: &mut State) -> Result<State> {
    let mut catalog = self.ensure_catalog(context, state).await?;

    for cycle in 1..=context.config.max_cycles {
      self.check_cancel(context)?;
      catalog = self
        .refresh_catalog_if_spec_changed(context, state, catalog)
        .await?;
      state.cycle = cycle;
      state.phase = Phase::Reconciling;
      state.active_leases.clear();
      state.candidate_integrations.clear();
      state.last_summary = format!("Cycle {cycle}: reconciling repository against requirements");
      self.publish(&context.events, state).await?;

      let reconciliation = self
        .read_only_worker(context, "reconciliation", || {
          self.backend.reconcile(
            context,
            &catalog,
            &state.completed_work_units,
            &state.discoveries,
          )
        })
        .await?;
      validate_reconcile(&catalog, &reconciliation)?;
      let graph = WorkGraph::from_reconcile(&catalog, &reconciliation)?;
      store::write_roadmap(&self.cwd, &reconciliation).await?;
      state.requirement_counts = requirement_counts(&catalog, &reconciliation);
      state.last_summary.clone_from(&reconciliation.summary);
      context
        .events
        .emit(RunEvent::Reconcile(reconciliation.clone()))
        .await;
      self.publish(&context.events, state).await?;

      if reconciliation.complete && all_satisfied(&catalog, &reconciliation) {
        if let Some(final_state) = self.finalize(context, state, &catalog).await? {
          return Ok(final_state);
        }
        continue;
      }

      let completed: BTreeSet<_> = state
        .completed_work_units
        .iter()
        .map(|item| item.work_unit.id.clone())
        .collect();
      let active: BTreeSet<_> = state
        .active_leases
        .values()
        .map(|lease| lease.work_unit.id.clone())
        .collect();
      let frontier = graph.ready_frontier(&completed, &active);
      if frontier.is_empty() {
        return self
          .block(
            context,
            state,
            "Reconciliation found gaps but the validated work graph has no ready work units",
          )
          .await;
      }
      context
        .events
        .emit(RunEvent::ReadyFrontier(frontier.clone()))
        .await;
      for unit in graph.units() {
        state.work_statuses.insert(
          unit.id.clone(),
          if completed.contains(&unit.id) {
            WorkStatus::Completed
          } else {
            WorkStatus::Pending
          },
        );
      }
      for unit in &frontier {
        state
          .work_statuses
          .insert(unit.id.clone(), WorkStatus::Ready);
      }

      let base_revision = git::head(&self.cwd).await?;
      if !git::is_clean(&self.cwd).await? {
        return self
          .block(
            context,
            state,
            "Canonical working tree became dirty before scheduling",
          )
          .await;
      }
      let run_id = state.run_id.clone().context("run id missing")?;
      let scheduler = Scheduler::new(
        self.cwd.clone(),
        run_id.clone(),
        self.backend.clone(),
        context.clone(),
        Arc::new(catalog.clone()),
      );
      let leases = scheduler.issue_leases(frontier, &base_revision);
      state.active_leases = leases
        .iter()
        .map(|lease| (lease.id.clone(), lease.clone()))
        .collect();
      for lease in &leases {
        state
          .work_statuses
          .insert(lease.work_unit.id.clone(), WorkStatus::Running);
      }
      state.phase = Phase::Scheduling;
      state.last_summary = format!("Executing {} ready work unit(s)", leases.len());
      self.publish(&context.events, state).await?;

      let execution_result = scheduler.execute_leases(leases).await;
      let cleanup_result = scheduler.cleanup().await;
      state.active_leases.clear();
      let mut candidates = match (execution_result, cleanup_result) {
        (Ok(candidates), Ok(())) => candidates,
        (Err(error), Ok(())) => return Err(error),
        (Ok(_), Err(error)) => return Err(error.context("scheduler cleanup")),
        (Err(error), Err(cleanup)) => {
          return Err(error.context(format!("scheduler cleanup also failed: {cleanup:#}")))
        }
      };
      deterministic_order(&mut candidates);
      state.candidate_integrations = candidates.clone();
      for candidate in &candidates {
        state
          .work_statuses
          .insert(candidate.lease.work_unit.id.clone(), WorkStatus::Candidate);
        state
          .discoveries
          .extend(candidate.worker_summary.discoveries.iter().cloned());
      }
      self.publish(&context.events, state).await?;

      let integrated = self
        .integrate_candidates(context, state, &run_id, base_revision, candidates)
        .await?;
      if !integrated {
        return Ok(state.clone());
      }
      state.candidate_integrations.clear();
      self.publish(&context.events, state).await?;
    }

    self
      .block(
        context,
        state,
        &format!(
          "Maximum cycle count ({}) reached",
          context.config.max_cycles
        ),
      )
      .await
  }

  async fn integrate_candidates(
    &self,
    context: &BackendContext,
    state: &mut State,
    run_id: &str,
    base_revision: String,
    candidates: Vec<WorkExecution>,
  ) -> Result<bool> {
    let workspaces = WorkspaceManager::new(self.cwd.clone(), run_id);
    let mut integrator = Integrator::create(
      self.cwd.clone(),
      &workspaces,
      base_revision,
      (*context.config).clone(),
    )
    .await?;

    let mut accepted = true;
    for candidate in candidates {
      self.check_cancel(context)?;
      let id = candidate.lease.work_unit.id.clone();
      state.phase = Phase::Integrating;
      state
        .work_statuses
        .insert(id.clone(), WorkStatus::Integrating);
      state.last_summary = format!("Integrating {id}");
      self.publish(&context.events, state).await?;
      context
        .events
        .emit(RunEvent::IntegrationStarted {
          work_unit_id: id.clone(),
          candidate_revision: candidate.candidate_revision.clone(),
        })
        .await;

      match integrator.integrate(&candidate).await? {
        IntegrationOutcome::Accepted {
          revision,
          verification,
        } => {
          let evidence_path =
            store::save_evidence(&self.cwd, &format!("{}-{id}", state.cycle), &verification)
              .await?;
          state.completed_work_units.push(CompletedWorkUnit {
            work_unit: candidate.lease.work_unit.clone(),
            completed_at: Utc::now().to_rfc3339(),
            verification_evidence: relative_path(&self.cwd, &evidence_path),
          });
          state
            .work_statuses
            .insert(id.clone(), WorkStatus::Completed);
          context
            .events
            .emit(RunEvent::IntegrationAccepted {
              work_unit_id: id,
              revision,
            })
            .await;
        }
        outcome => {
          accepted = false;
          let reason = integration_failure(&outcome);
          state.work_statuses.insert(id.clone(), WorkStatus::Failed);
          context
            .events
            .emit(RunEvent::IntegrationRejected {
              work_unit_id: id,
              reason: reason.clone(),
            })
            .await;
          self.block(context, state, &reason).await?;
          break;
        }
      }
    }

    integrator.cleanup(&workspaces).await?;
    Ok(accepted)
  }

  async fn finalize(
    &self,
    context: &BackendContext,
    state: &mut State,
    catalog: &RequirementCatalog,
  ) -> Result<Option<State>> {
    state.phase = Phase::Verifying;
    state.last_summary = "Running final deterministic verification".into();
    self.publish(&context.events, state).await?;
    let report = verifier::run_verification(&self.cwd, &context.config).await?;
    context
      .events
      .emit(RunEvent::Verification(report.clone()))
      .await;
    if !report.passed {
      return Ok(Some(
        self
          .block(context, state, "Final deterministic verification failed")
          .await?,
      ));
    }

    state.phase = Phase::Assessing;
    state.last_summary = "Independent fresh-context completion assessment".into();
    self.publish(&context.events, state).await?;
    let assessment = self
      .read_only_worker(context, "assessment", || {
        self.backend.assess(context, catalog)
      })
      .await?;
    validate_reconcile(catalog, &assessment)?;
    WorkGraph::from_reconcile(catalog, &assessment)?;
    store::write_roadmap(&self.cwd, &assessment).await?;
    state.requirement_counts = requirement_counts(catalog, &assessment);
    state.last_summary.clone_from(&assessment.summary);
    context
      .events
      .emit(RunEvent::Reconcile(assessment.clone()))
      .await;
    if !(assessment.complete && all_satisfied(catalog, &assessment)) {
      self.publish(&context.events, state).await?;
      return Ok(None);
    }
    if !git::is_clean(&self.cwd).await? {
      return Ok(Some(
        self
          .block(
            context,
            state,
            "Completion requires a clean Git working tree",
          )
          .await?,
      ));
    }

    state.status = RunStatus::Done;
    state.phase = Phase::Complete;
    state.last_summary = "All requirements have evidence and deterministic gates pass".into();
    self.publish(&context.events, state).await?;
    context.events.emit(RunEvent::Finished(state.clone())).await;
    Ok(Some(state.clone()))
  }

  async fn ensure_catalog(
    &self,
    context: &BackendContext,
    state: &mut State,
  ) -> Result<RequirementCatalog> {
    let (spec, spec_hash) = store::spec_text_and_hash(&self.cwd, &context.config).await?;
    if let Some(catalog) = store::read_catalog(&self.cwd).await? {
      if catalog.spec_hash == spec_hash {
        context
          .events
          .emit(RunEvent::Catalog(catalog.clone()))
          .await;
        return Ok(catalog);
      }
    }
    state.phase = Phase::Architecting;
    state.last_summary = "Deriving requirement catalog from .tenet/spec.md".into();
    self.publish(&context.events, state).await?;
    let output = self
      .read_only_worker(context, "architect", || {
        self.backend.architect(context, &spec)
      })
      .await?;
    validate_requirements(&output.requirements)?;
    let catalog = RequirementCatalog {
      spec_hash,
      requirements: output.requirements,
    };
    store::write_catalog(&self.cwd, &catalog).await?;
    context
      .events
      .emit(RunEvent::Catalog(catalog.clone()))
      .await;
    state.requirement_counts = RequirementCounts {
      total: catalog.requirements.len(),
      ..Default::default()
    };
    self.publish(&context.events, state).await?;
    Ok(catalog)
  }

  async fn read_only_worker<T, F, Fut>(
    &self,
    context: &BackendContext,
    name: &str,
    call: F,
  ) -> Result<T>
  where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T>>,
  {
    let protected = protection::snapshot(&self.cwd, &protected_paths(&context.config)).await?;
    let result = call().await;
    let restoration = protection::restore_changes(&self.cwd, &protected).await;
    match (result, restoration) {
      (Ok(value), Ok(changed)) if changed.is_empty() => Ok(value),
      (Ok(_), Ok(changed)) => bail!(
        "{name} worker violated read-only contract; restored: {}",
        changed.join(", ")
      ),
      (Ok(_), Err(cleanup_error)) => Err(cleanup_error).context(format!(
        "{name} worker cleanup failed while enforcing read-only contract"
      )),
      (Err(worker_error), Ok(_)) => Err(worker_error).context(format!("{name} worker")),
      (Err(worker_error), Err(cleanup_error)) => Err(worker_error).context(format!(
        "{name} worker; read-only cleanup also failed: {cleanup_error:#}"
      )),
    }
  }

  async fn refresh_catalog_if_spec_changed(
    &self,
    context: &BackendContext,
    state: &mut State,
    catalog: RequirementCatalog,
  ) -> Result<RequirementCatalog> {
    let (_, hash) = store::spec_text_and_hash(&self.cwd, &context.config).await?;
    if hash == catalog.spec_hash {
      return Ok(catalog);
    }
    state.completed_work_units.clear();
    state.work_statuses.clear();
    state.discoveries.clear();
    self.ensure_catalog(context, state).await
  }

  async fn block(
    &self,
    context: &BackendContext,
    state: &mut State,
    reason: &str,
  ) -> Result<State> {
    state.status = RunStatus::Blocked;
    state.blocked_reason = Some(reason.into());
    state.last_summary = reason.into();
    self.publish(&context.events, state).await?;
    context.events.emit(RunEvent::Finished(state.clone())).await;
    Ok(state.clone())
  }

  async fn publish(&self, events: &EventSink, state: &mut State) -> Result<()> {
    state.updated_at = Utc::now().to_rfc3339();
    store::write_state(&self.cwd, state).await?;
    events.emit(RunEvent::State(state.clone())).await;
    Ok(())
  }

  fn check_cancel(&self, context: &BackendContext) -> Result<()> {
    if context.cancel.is_cancelled() {
      bail!("run cancelled");
    }
    Ok(())
  }
}

fn validate_requirements(requirements: &[tenet_domain::model::Requirement]) -> Result<()> {
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
  let expected: BTreeSet<_> = catalog
    .requirements
    .iter()
    .map(|item| item.id.as_str())
    .collect();
  let actual: BTreeSet<_> = result
    .requirements
    .iter()
    .map(|item| item.id.as_str())
    .collect();
  if expected != actual || actual.len() != result.requirements.len() {
    bail!("reconciliation assessment IDs do not match the requirement catalog");
  }
  if result.complete && (!all_satisfied(catalog, result) || !result.work_units.is_empty()) {
    bail!("reconciliation claimed complete without every requirement satisfied and workUnits=[]");
  }
  if !result.complete && result.work_units.is_empty() {
    bail!("reconciliation found incomplete requirements but proposed no work units");
  }
  Ok(())
}

fn all_satisfied(catalog: &RequirementCatalog, result: &ReconcileResult) -> bool {
  result.requirements.len() == catalog.requirements.len()
    && result
      .requirements
      .iter()
      .all(|item| item.status == RequirementStatus::Satisfied)
}

fn requirement_counts(catalog: &RequirementCatalog, result: &ReconcileResult) -> RequirementCounts {
  let by_id: BTreeMap<_, _> = result
    .requirements
    .iter()
    .map(|item| (item.id.as_str(), &item.status))
    .collect();
  let mut counts = RequirementCounts {
    total: catalog.requirements.len(),
    ..Default::default()
  };
  for requirement in &catalog.requirements {
    match by_id.get(requirement.id.as_str()).copied() {
      Some(RequirementStatus::Satisfied) => counts.satisfied += 1,
      Some(RequirementStatus::Partial) => counts.partial += 1,
      _ => counts.missing += 1,
    }
  }
  counts
}

fn integration_failure(outcome: &IntegrationOutcome) -> String {
  match outcome {
    IntegrationOutcome::StaleBase => "Candidate base is not an ancestor of integration HEAD".into(),
    IntegrationOutcome::MergeConflict { paths } => {
      format!("Candidate merge conflict: {}", paths.join(", "))
    }
    IntegrationOutcome::VerificationFailed { .. } => {
      "Candidate verification failed during integration".into()
    }
    IntegrationOutcome::RegressionDetected { .. } => {
      "Candidate introduced a deterministic verification regression".into()
    }
    IntegrationOutcome::Accepted { .. } => "Candidate accepted".into(),
  }
}

fn protected_paths(config: &Config) -> Vec<String> {
  let mut paths = config.protected_paths.clone();
  if !paths.iter().any(|path| path == &config.spec_file) {
    paths.push(config.spec_file.clone());
  }
  paths
}

fn relative_path(root: &Path, path: &Path) -> String {
  path
    .strip_prefix(root)
    .unwrap_or(path)
    .display()
    .to_string()
}

pub async fn manual_verify(cwd: &Path) -> Result<VerificationReport> {
  let config = ensure_config(cwd).await?;
  let report = verifier::run_verification(cwd, &config).await?;
  let _ = store::save_evidence(cwd, &format!("manual-{}", Utc::now().timestamp()), &report).await?;
  Ok(report)
}
