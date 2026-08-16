use std::{
  collections::{BTreeMap, BTreeSet},
  fmt::Write as _,
  future::Future,
  path::{Path, PathBuf},
  sync::Arc,
};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use tenet_domain::{
  config::{config_path, ensure_config, read_config, TENET_DIR},
  events::{EventSink, RunEvent, RunLogger},
  model::{
    CompletedWorkUnit, DiscoveryRecord, DiscoveryStatus, IntegrationPhase, Phase, ReconcileResult,
    RepairProgress, RequirementCatalog, RequirementCounts, RequirementStatus, RunStatus, State,
    VerificationReport, WorkExecution, WorkStatus,
  },
};

use crate::{
  backend::{AgentBackend, BackendContext},
  git,
  graph::WorkGraph,
  integration::{deterministic_order, IntegrationOutcome, Integrator},
  scheduler::{ExecutionUpdate, Scheduler},
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
    match self.run_attempt(cancel).await {
      Ok(state) => Ok(state),
      Err(error) => match self.record_failed_attempt(&error).await {
        Ok(()) => Err(error),
        Err(persist_error) => Err(error.context(format!(
          "persisting latest failed run attempt also failed: {persist_error:#}"
        ))),
      },
    }
  }

  async fn run_attempt(&self, cancel: CancellationToken) -> Result<State> {
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
    let mut state = store::read_state(&self.cwd).await?;
    store::recover_integration(&self.cwd, &mut state).await?;
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
    state.version = State::VERSION;
    state.status = RunStatus::Running;
    state.phase = Phase::Architecting;
    state.run_id = Some(run_id.clone());
    state.cycle = 0;
    state.active_leases.clear();
    state.candidate_integrations.clear();
    state.current_repair = None;
    state.work_statuses.clear();
    state.blocked_reason = None;
    state.last_error = None;
    state.last_summary = "Starting deterministic coordinated run".into();
    self.publish(&events, &mut state).await?;

    let workspaces = WorkspaceManager::new(self.cwd.clone(), &run_id);
    let result = self.run_inner(&context, &mut state).await;
    let cleanup = workspaces.cleanup_run().await;
    match (result, cleanup) {
      (Ok(_), Ok(())) if state.status == RunStatus::Done => {
        self.publish(&events, &mut state).await?;
        events.emit(RunEvent::Finished(state.clone())).await?;
        Ok(state)
      }
      (Ok(final_state), Ok(())) => Ok(final_state),
      (Ok(_), Err(error)) => {
        state.active_leases.clear();
        state.candidate_integrations.clear();
        state.current_repair = None;
        state.status = RunStatus::Failed;
        state.last_error = Some(format!("workspace cleanup failed: {error:#}"));
        state.last_summary = "Run failed during required workspace cleanup".into();
        self.publish(&events, &mut state).await?;
        events.emit(RunEvent::Finished(state.clone())).await?;
        Err(error.context("run workspace cleanup"))
      }
      (Err(error), cleanup) => {
        state.active_leases.clear();
        state.candidate_integrations.clear();
        state.current_repair = None;
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
        events.emit(RunEvent::Finished(state.clone())).await?;
        if cancelled {
          Ok(state)
        } else {
          Err(error)
        }
      }
    }
  }

  async fn record_failed_attempt(&self, error: &anyhow::Error) -> Result<()> {
    let state_path = self.cwd.join(TENET_DIR).join(store::STATE_FILE);
    if !state_path.exists() {
      return Ok(());
    }
    let mut state = store::read_state(&self.cwd).await?;
    if state.status == RunStatus::Failed {
      return Ok(());
    }
    state.status = RunStatus::Failed;
    state.phase = Phase::Initialized;
    state.run_id = Some(format!(
      "preflight-{}-{}",
      Utc::now().format("%Y%m%dT%H%M%SZ"),
      &Uuid::new_v4().to_string()[..8]
    ));
    state.active_leases.clear();
    state.candidate_integrations.clear();
    state.current_repair = None;
    state.last_summary = "Latest run attempt failed during preflight".into();
    state.last_error = Some(error.to_string());
    state.updated_at = Utc::now().to_rfc3339();
    store::write_state(&self.cwd, &state).await
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
      state.current_repair = None;
      state.last_summary = format!("Cycle {cycle}: reconciling repository against requirements");
      self.publish(&context.events, state).await?;

      state.discoveries.retain(|record| {
        record.status == DiscoveryStatus::Active && record.catalog_hash == catalog.spec_hash
      });
      let catalog_for_worker = catalog.clone();
      let completed_for_worker = state.completed_work_units.clone();
      let discoveries_for_worker: Vec<_> = state
        .discoveries
        .iter()
        .map(|record| record.discovery.clone())
        .collect();
      let backend = self.backend.clone();
      let reconciliation = self
        .read_only_worker(
          context,
          "reconciliation",
          move |inspection_context| async move {
            backend
              .reconcile(
                &inspection_context,
                &catalog_for_worker,
                &completed_for_worker,
                &discoveries_for_worker,
              )
              .await
          },
        )
        .await?;
      validate_reconcile(&catalog, &reconciliation)?;
      let graph = WorkGraph::from_reconcile(&catalog, &reconciliation)?;
      let fingerprint = progress_fingerprint(&self.cwd, &catalog, &reconciliation, state).await?;
      if advance_stagnation(state, fingerprint, context.config.stagnation_limit) {
        return self
          .block(
            context,
            state,
            &format!(
              "Stagnation limit ({}) reached without meaningful repository progress",
              context.config.stagnation_limit
            ),
          )
          .await;
      }
      for record in &mut state.discoveries {
        record.status = DiscoveryStatus::Consumed;
      }
      store::write_roadmap(&self.cwd, &reconciliation).await?;
      state.requirement_counts = requirement_counts(&catalog, &reconciliation);
      state.last_summary.clone_from(&reconciliation.summary);
      context
        .events
        .emit(RunEvent::Reconcile(reconciliation.clone()))
        .await?;
      self.publish(&context.events, state).await?;

      if reconciliation.complete && all_satisfied(&catalog, &reconciliation) {
        if let Some(final_state) = self.finalize(context, state, &catalog).await? {
          return Ok(final_state);
        }
        continue;
      }

      // Current reconciliation describes remaining work. Historical execution records are context
      // for the reconciler, never scheduling authority over the current repository.
      let completed = BTreeSet::new();
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
        .await?;
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
      let (updates, mut update_receiver) = mpsc::unbounded_channel();
      let scheduler = Scheduler::new(
        self.cwd.clone(),
        run_id.clone(),
        self.backend.clone(),
        context.clone(),
        Arc::new(catalog.clone()),
        updates,
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
      state.phase = Phase::Implementing;
      state.last_summary = format!("Implementing {} leased work unit(s)", leases.len());
      self.publish(&context.events, state).await?;

      let execution_result = {
        let mut execution = Box::pin(scheduler.execute_leases(leases));
        loop {
          tokio::select! {
            result = &mut execution => break result,
            Some(update) = update_receiver.recv() => {
              match update {
                ExecutionUpdate::Implementing { work_unit_id } => {
                  state.phase = Phase::Implementing;
                  state.current_repair = None;
                  state.last_summary = format!("Implementing {work_unit_id}");
                }
                ExecutionUpdate::Repairing { work_unit_id, attempt } => {
                  state.phase = Phase::Repairing;
                  state.current_repair = Some(RepairProgress {
                    work_unit_id: work_unit_id.clone(),
                    attempt,
                  });
                  state.last_summary = format!("Repairing {work_unit_id}, attempt {attempt}");
                }
              }
              self.publish(&context.events, state).await?;
            }
          }
        }
      };
      let cleanup_result = scheduler.cleanup().await;
      state.active_leases.clear();
      state.current_repair = None;
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
        record_candidate_discoveries(state, &catalog, candidate)?;
      }
      self.publish(&context.events, state).await?;

      let integrated = self
        .integrate_candidates(context, state, &run_id, base_revision, candidates)
        .await?;
      if !integrated {
        return Ok(state.clone());
      }
      state.candidate_integrations.clear();
      state.current_repair = None;
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
    let mut integrator = Integrator::create_with_cancel(
      self.cwd.clone(),
      &workspaces,
      base_revision,
      (*context.config).clone(),
      context.cancel.clone(),
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
        .await?;

      match integrator.integrate(&candidate).await? {
        IntegrationOutcome::Accepted {
          revision,
          verification: _,
          mut transaction,
        } => {
          state.completed_work_units.push(CompletedWorkUnit {
            work_unit: transaction.work_unit.clone(),
            completed_at: Utc::now().to_rfc3339(),
            verification_evidence: transaction.verification_evidence.clone(),
          });
          state
            .work_statuses
            .insert(id.clone(), WorkStatus::Completed);
          self.publish(&context.events, state).await?;
          transaction.phase = IntegrationPhase::StateCommitted;
          transaction.updated_at = Utc::now().to_rfc3339();
          store::write_integration_journal(&self.cwd, &transaction).await?;
          store::remove_integration_journal(&self.cwd).await?;
          context
            .events
            .emit(RunEvent::IntegrationAccepted {
              work_unit_id: id,
              revision,
            })
            .await?;
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
            .await?;
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
    let verified_revision = git::head(&self.cwd).await?;
    state.phase = Phase::Verifying;
    state.last_summary = "Running final deterministic verification".into();
    self.publish(&context.events, state).await?;
    let report = self.verify_revision(context, &verified_revision).await?;
    context
      .events
      .emit(RunEvent::Verification(report.clone()))
      .await?;
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
    let catalog_for_worker = catalog.clone();
    let backend = self.backend.clone();
    let assessment = self
      .read_only_worker(
        context,
        "assessment",
        move |inspection_context| async move {
          backend
            .assess(&inspection_context, &catalog_for_worker)
            .await
        },
      )
      .await?;
    validate_reconcile(catalog, &assessment)?;
    WorkGraph::from_reconcile(catalog, &assessment)?;
    store::write_roadmap(&self.cwd, &assessment).await?;
    state.requirement_counts = requirement_counts(catalog, &assessment);
    state.last_summary.clone_from(&assessment.summary);
    context
      .events
      .emit(RunEvent::Reconcile(assessment.clone()))
      .await?;
    if !(assessment.complete && all_satisfied(catalog, &assessment)) {
      self.publish(&context.events, state).await?;
      return Ok(None);
    }
    if git::head(&self.cwd).await? != verified_revision {
      bail!("canonical revision changed after final deterministic verification");
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

    if !state.active_leases.is_empty() || !state.candidate_integrations.is_empty() {
      bail!("completion requires no active lease or unresolved candidate integration");
    }
    if store::read_integration_journal(&self.cwd).await?.is_some() {
      bail!("completion requires no unresolved integration transaction");
    }
    state.status = RunStatus::Done;
    state.phase = Phase::Complete;
    state.last_summary = "All requirements have evidence and deterministic gates pass".into();
    Ok(Some(state.clone()))
  }

  async fn ensure_catalog(
    &self,
    context: &BackendContext,
    state: &mut State,
  ) -> Result<RequirementCatalog> {
    let (spec, spec_hash) = store::spec_text_and_hash(&self.cwd, &context.config).await?;
    let cached = store::read_catalog(&self.cwd).await?;
    if let Some(catalog) = &cached {
      validate_requirements(&catalog.requirements)
        .context("cached requirement catalog is invalid")?;
      if catalog.spec_hash == spec_hash {
        context
          .events
          .emit(RunEvent::Catalog(catalog.clone()))
          .await?;
        return Ok(catalog.clone());
      }
    }
    if cached.is_some() || !state.completed_work_units.is_empty() || !state.discoveries.is_empty() {
      state.completed_work_units.clear();
      state.work_statuses.clear();
      state.discoveries.clear();
    }
    state.phase = Phase::Architecting;
    state.last_summary = "Deriving requirement catalog from .tenet/spec.md".into();
    self.publish(&context.events, state).await?;
    let backend = self.backend.clone();
    let output = self
      .read_only_worker(context, "architect", move |inspection_context| async move {
        backend.architect(&inspection_context, &spec).await
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
      .await?;
    state.requirement_counts = RequirementCounts {
      total: catalog.requirements.len(),
      ..Default::default()
    };
    self.publish(&context.events, state).await?;
    Ok(catalog)
  }
  async fn verify_revision(
    &self,
    context: &BackendContext,
    revision: &str,
  ) -> Result<VerificationReport> {
    let canonical_before = git::repository_state(&self.cwd).await?;
    let run_id = context
      .runtime_dir
      .file_name()
      .and_then(|value| value.to_str())
      .context("verification run id is unavailable")?;
    let workspaces = WorkspaceManager::new(self.cwd.clone(), run_id);
    let workspace = workspaces
      .create_disposable("final-verification", revision)
      .await?;
    let result =
      verifier::run_verification_cancelled(&workspace, &context.config, &context.cancel).await;
    let cleanup = workspaces.remove(&workspace).await;
    if let Err(cleanup) = cleanup {
      return match result {
        Ok(_) => Err(cleanup).context("discard final verification worktree"),
        Err(error) => Err(error).context(format!(
          "final verification failed; cleanup also failed: {cleanup:#}"
        )),
      };
    }
    if git::repository_state(&self.cwd).await? != canonical_before {
      bail!("final verification command modified canonical repository state");
    }
    result
  }

  async fn read_only_worker<T, F, Fut>(
    &self,
    context: &BackendContext,
    name: &str,
    call: F,
  ) -> Result<T>
  where
    F: FnOnce(BackendContext) -> Fut,
    Fut: Future<Output = Result<T>>,
  {
    let canonical_before = git::repository_state(&self.cwd).await?;
    let run_id = context
      .runtime_dir
      .file_name()
      .and_then(|value| value.to_str())
      .context("read-only worker run id is unavailable")?;
    let workspaces = WorkspaceManager::new(self.cwd.clone(), run_id);
    let workspace = workspaces
      .create_disposable(name, &canonical_before.head)
      .await?;
    let mut inspection_context = context.clone();
    inspection_context.cwd.clone_from(&workspace);
    inspection_context.runtime_dir = context.runtime_dir.join(name);
    inspection_context.worker_id = format!("{}-{name}", context.worker_id);

    let result = call(inspection_context).await;
    let cleanup = workspaces.remove(&workspace).await;
    let canonical_after = git::repository_state(&self.cwd).await;
    match (result, cleanup, canonical_after) {
      (Ok(value), Ok(()), Ok(after)) if after == canonical_before => Ok(value),
      (Ok(_), Ok(()), Ok(_)) => {
        bail!("{name} worker violated the canonical repository read-only invariant")
      }
      (Ok(_), Err(cleanup_error), _) => {
        Err(cleanup_error).context(format!("discard {name} inspection worktree"))
      }
      (Ok(_), Ok(()), Err(state_error)) => {
        Err(state_error).context(format!("verify canonical state after {name} worker"))
      }
      (Err(worker_error), Ok(()), Ok(after)) if after == canonical_before => {
        Err(worker_error).context(format!("{name} worker"))
      }
      (Err(worker_error), cleanup, state) => Err(worker_error).context(format!(
        "{name} worker; isolation cleanup/state verification failed: cleanup={cleanup:?}, state={state:?}"
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
    for status in state.work_statuses.values_mut() {
      *status = WorkStatus::Invalidated;
    }
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
    for status in state.work_statuses.values_mut() {
      if *status != WorkStatus::Completed && *status != WorkStatus::Invalidated {
        *status = WorkStatus::Blocked;
      }
    }
    self.publish(&context.events, state).await?;
    context
      .events
      .emit(RunEvent::Finished(state.clone()))
      .await?;
    Ok(state.clone())
  }

  async fn publish(&self, events: &EventSink, state: &mut State) -> Result<()> {
    state.updated_at = Utc::now().to_rfc3339();
    store::write_state(&self.cwd, state).await?;
    events.emit(RunEvent::State(state.clone())).await?;
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
      || requirement
        .acceptance_criteria
        .iter()
        .any(|criterion| criterion.trim().is_empty())
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
  for assessment in &result.requirements {
    match assessment.status {
      RequirementStatus::Satisfied => {
        if assessment.evidence.is_empty()
          || assessment
            .evidence
            .iter()
            .any(|evidence| evidence.trim().is_empty())
          || !assessment.gaps.is_empty()
        {
          bail!(
            "satisfied requirement {} requires nonblank evidence and no gaps",
            assessment.id
          );
        }
      }
      RequirementStatus::Partial | RequirementStatus::Missing => {
        if assessment.gaps.is_empty() || assessment.gaps.iter().any(|gap| gap.trim().is_empty()) {
          bail!(
            "incomplete requirement {} requires at least one nonblank gap",
            assessment.id
          );
        }
      }
    }
  }
  if result.complete && (!all_satisfied(catalog, result) || !result.work_units.is_empty()) {
    bail!("reconciliation claimed complete without every requirement satisfied and workUnits=[]");
  }
  if !result.complete && result.work_units.is_empty() {
    bail!("reconciliation found incomplete requirements but proposed no work units");
  }

  Ok(())
}
async fn progress_fingerprint(
  cwd: &Path,
  catalog: &RequirementCatalog,
  reconciliation: &ReconcileResult,
  state: &State,
) -> Result<String> {
  let head = git::head(cwd).await?;
  let active_discoveries: Vec<_> = state
    .discoveries
    .iter()
    .filter(|record| record.status == DiscoveryStatus::Active)
    .map(|record| record.fingerprint.as_str())
    .collect();
  let bytes = serde_json::to_vec(&(head, &catalog.spec_hash, reconciliation, active_discoveries))?;
  let digest = Sha256::digest(bytes);
  let mut fingerprint = String::with_capacity(digest.len() * 2);
  for byte in digest {
    write!(fingerprint, "{byte:02x}")?;
  }
  Ok(fingerprint)
}

fn advance_stagnation(state: &mut State, fingerprint: String, limit: u32) -> bool {
  if state.progress_fingerprint.as_deref() == Some(&fingerprint) {
    state.stagnation_count = state.stagnation_count.saturating_add(1);
  } else {
    state.progress_fingerprint = Some(fingerprint);
    state.stagnation_count = 0;
  }
  state.stagnation_count >= limit
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
fn record_candidate_discoveries(
  state: &mut State,
  catalog: &RequirementCatalog,
  candidate: &WorkExecution,
) -> Result<()> {
  for discovered in &candidate.discoveries {
    let identity = serde_json::to_vec(&(
      &catalog.spec_hash,
      &candidate.candidate_revision,
      &candidate.lease.work_unit.id,
      discovered.role,
      &discovered.discovery,
    ))?;
    let digest = Sha256::digest(identity);
    let mut fingerprint = String::with_capacity(digest.len() * 2);
    for byte in digest {
      write!(fingerprint, "{byte:02x}")?;
    }
    if state
      .discoveries
      .iter()
      .any(|record| record.fingerprint == fingerprint && record.status == DiscoveryStatus::Active)
    {
      continue;
    }
    state.discoveries.push(DiscoveryRecord {
      fingerprint,
      discovery: discovered.discovery.clone(),
      catalog_hash: catalog.spec_hash.clone(),
      repository_revision: candidate.candidate_revision.clone(),
      work_unit_id: candidate.lease.work_unit.id.clone(),
      role: discovered.role,
      cycle: state.cycle,
      status: DiscoveryStatus::Active,
    });
  }
  Ok(())
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

pub async fn manual_verify(cwd: &Path) -> Result<VerificationReport> {
  let config = ensure_config(cwd).await?;
  let report = verifier::run_verification(cwd, &config).await?;
  let _ = store::save_evidence(cwd, &format!("manual-{}", Utc::now().timestamp()), &report).await?;
  Ok(report)
}

#[cfg(test)]
mod tests {
  use super::*;
  use tenet_domain::model::{Requirement, RequirementAssessment};

  fn catalog() -> RequirementCatalog {
    RequirementCatalog {
      spec_hash: "spec".into(),
      requirements: vec![Requirement {
        id: "REQ-001".into(),
        title: "Requirement".into(),
        description: "Description".into(),
        acceptance_criteria: vec!["Observable result".into()],
      }],
    }
  }

  fn result(status: RequirementStatus, evidence: Vec<&str>, gaps: Vec<&str>) -> ReconcileResult {
    let complete = status == RequirementStatus::Satisfied;
    ReconcileResult {
      complete,
      summary: "assessment".into(),
      requirements: vec![RequirementAssessment {
        id: "REQ-001".into(),
        status,
        evidence: evidence.into_iter().map(str::to_owned).collect(),
        gaps: gaps.into_iter().map(str::to_owned).collect(),
      }],
      work_units: if complete {
        Vec::new()
      } else {
        vec![tenet_domain::model::WorkUnit {
          id: "A".into(),
          title: "Work".into(),
          objective: "Implement".into(),
          requirement_ids: vec!["REQ-001".into()],
          acceptance_criteria: vec!["Done".into()],
          suggested_checks: Vec::new(),
          depends_on: Vec::new(),
          scope: tenet_domain::model::WorkScope {
            paths: vec!["src/**".into()],
          },
        }]
      },
    }
  }

  #[test]
  fn satisfied_requirement_without_evidence_is_rejected() {
    let error = validate_reconcile(
      &catalog(),
      &result(RequirementStatus::Satisfied, Vec::new(), Vec::new()),
    )
    .expect_err("empty evidence rejected");

    assert!(error.to_string().contains("requires nonblank evidence"));
  }

  #[test]
  fn satisfied_requirement_with_gap_is_rejected() {
    let error = validate_reconcile(
      &catalog(),
      &result(RequirementStatus::Satisfied, vec!["proof"], vec!["gap"]),
    )
    .expect_err("satisfied gap rejected");

    assert!(error.to_string().contains("and no gaps"));
  }

  #[test]
  fn missing_requirement_without_gap_is_rejected() {
    let error = validate_reconcile(
      &catalog(),
      &result(RequirementStatus::Missing, Vec::new(), Vec::new()),
    )
    .expect_err("missing gap rejected");

    assert!(error
      .to_string()
      .contains("requires at least one nonblank gap"));
  }

  #[test]
  fn blank_catalog_acceptance_criterion_is_rejected() {
    let mut invalid = catalog();
    invalid.requirements[0].acceptance_criteria = vec![" ".into()];

    assert!(validate_requirements(&invalid.requirements).is_err());
  }
}

#[cfg(test)]
mod stagnation_tests {
  use super::*;

  #[test]
  fn stagnation_blocks_exactly_after_configured_unchanged_transitions() {
    let mut state = State::fresh();

    assert!(!advance_stagnation(&mut state, "same".into(), 2));
    assert!(!advance_stagnation(&mut state, "same".into(), 2));
    assert!(advance_stagnation(&mut state, "same".into(), 2));
    assert_eq!(state.stagnation_count, 2);
  }

  #[test]
  fn meaningful_progress_resets_stagnation_counter() {
    let mut state = State::fresh();
    advance_stagnation(&mut state, "old".into(), 3);
    advance_stagnation(&mut state, "old".into(), 3);

    assert!(!advance_stagnation(&mut state, "new".into(), 3));
    assert_eq!(state.stagnation_count, 0);
  }
}
