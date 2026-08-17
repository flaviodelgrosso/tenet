use std::{
  collections::{BTreeMap, BTreeSet},
  fmt::Write as _,
  future::Future,
  path::{Path, PathBuf},
  sync::Arc,
};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use tenet_domain::{
  config::{config_path, ensure_config, read_config, TENET_DIR},
  events::{EventSink, RunEvent, RunLogger},
  evidence::{EvidenceGraphState, EvidencePolicy, ImplementationState, VerificationState},
  ids::VerificationRunId,
  model::{
    CompletedWorkUnit, DiscoveryRecord, DiscoveryStatus, IntegrationPhase, Phase, ReconcileResult,
    RepairProgress, RequirementCatalog, RequirementCounts, RunStatus, State, VerificationReport,
    WorkExecution, WorkStatus,
  },
};

use tenet_runtime::{
  backend::BackendContext,
  git,
  graph::WorkGraph,
  integration::{deterministic_order, IntegrationOutcome, Integrator},
  scheduler::{CandidateExecutor, ExecutionUpdate, Scheduler},
  store::{self, RunLock},
  verifier,
  workspace::WorkspaceManager,
};

use crate::{
  catalog,
  completion::{CompletionContext, CompletionDecision, CompletionPolicy},
  evidence as evidence_graph,
  ports::agent::AgentBackend,
  verification,
};

pub struct Controller {
  cwd: PathBuf,
  backend: Arc<dyn AgentBackend>,
  events: EventSink,
}

struct ScheduledAgent {
  backend: Arc<dyn AgentBackend>,
}

#[async_trait]
impl CandidateExecutor for ScheduledAgent {
  async fn execute(
    &self,
    context: &BackendContext,
    catalog: &RequirementCatalog,
    work_unit: &tenet_domain::model::WorkUnit,
    previous_verification: Option<&VerificationReport>,
  ) -> Result<tenet_domain::model::WorkerSummary> {
    match previous_verification {
      Some(report) => {
        self
          .backend
          .repair(context, catalog, work_unit, report)
          .await
      }
      None => self.backend.implement(context, catalog, work_unit).await,
    }
  }
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
    let mut evidence_graph = evidence_graph::load(&self.cwd, &catalog).await?;
    store::write_evidence_graph(&self.cwd, &evidence_graph).await?;

    for cycle in 1..=context.config.max_cycles {
      self.check_cancel(context)?;
      catalog = self
        .refresh_catalog_if_spec_changed(context, state, catalog)
        .await?;
      evidence_graph = evidence_graph::load(&self.cwd, &catalog).await?;
      store::write_evidence_graph(&self.cwd, &evidence_graph).await?;
      let current_revision = git::head(&self.cwd).await?;
      evidence_graph::invalidate(
        &self.cwd,
        &context.events,
        &mut evidence_graph,
        &current_revision,
      )
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
      let evidence_for_worker = evidence_graph::projections(&evidence_graph)?;
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
                &evidence_for_worker,
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
      state.requirement_counts = requirement_counts(&catalog, &reconciliation, &evidence_graph)?;
      state.last_summary.clone_from(&reconciliation.summary);
      context
        .events
        .emit(RunEvent::Reconcile(reconciliation.clone()))
        .await?;
      self.publish(&context.events, state).await?;

      if reconciliation.work_units.is_empty() {
        if let Some(final_state) = self
          .finalize(context, state, &catalog, &mut evidence_graph)
          .await?
        {
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
        Arc::new(ScheduledAgent {
          backend: self.backend.clone(),
        }),
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
        .integrate_candidates(
          context,
          state,
          &run_id,
          base_revision,
          candidates,
          &mut evidence_graph,
        )
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
    evidence_graph: &mut EvidenceGraphState,
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
          verification,
          mut transaction,
        } => {
          evidence_graph::invalidate(&self.cwd, &context.events, evidence_graph, &revision).await?;
          evidence_graph::establish(
            &self.cwd,
            &context.events,
            evidence_graph,
            &revision,
            &candidate.verification,
          )
          .await?;
          evidence_graph::establish(
            &self.cwd,
            &context.events,
            evidence_graph,
            &revision,
            &verification,
          )
          .await?;
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
    evidence_graph: &mut EvidenceGraphState,
  ) -> Result<Option<State>> {
    let verified_revision = git::head(&self.cwd).await?;
    state.phase = Phase::Verifying;
    state.last_summary = "Running final deterministic verification".into();
    self.publish(&context.events, state).await?;
    let report = self
      .verify_revision(context, &verified_revision, catalog)
      .await?;
    context
      .events
      .emit(RunEvent::Verification(report.clone()))
      .await?;
    evidence_graph::establish(
      &self.cwd,
      &context.events,
      evidence_graph,
      &verified_revision,
      &report,
    )
    .await?;
    if !report.passed {
      return Ok(Some(
        self
          .block(context, state, "Final deterministic verification failed")
          .await?,
      ));
    }
    if !evidence_graph.all_required_verified(&EvidencePolicy) {
      return Ok(Some(
        self
          .block(
            context,
            state,
            "Final verification did not establish every mandatory verification obligation",
          )
          .await?,
      ));
    }

    state.phase = Phase::Assessing;
    state.last_summary = "Independent skeptical evidence-gap assessment".into();
    self.publish(&context.events, state).await?;
    let catalog_for_worker = catalog.clone();
    let evidence_for_worker = evidence_graph::projections(evidence_graph)?;
    let backend = self.backend.clone();
    let assessment = self
      .read_only_worker(
        context,
        "assessment",
        move |inspection_context| async move {
          backend
            .assess(
              &inspection_context,
              &catalog_for_worker,
              &evidence_for_worker,
            )
            .await
        },
      )
      .await?;
    validate_reconcile(catalog, &assessment)?;
    WorkGraph::from_reconcile(catalog, &assessment)?;
    store::write_roadmap(&self.cwd, &assessment).await?;
    state.requirement_counts = requirement_counts(catalog, &assessment, evidence_graph)?;
    state.last_summary.clone_from(&assessment.summary);
    context
      .events
      .emit(RunEvent::Reconcile(assessment.clone()))
      .await?;
    if !assessment.work_units.is_empty() {
      self.publish(&context.events, state).await?;
      return Ok(None);
    }
    let current_revision = git::head(&self.cwd).await?;
    let repository_clean = git::is_clean(&self.cwd).await?;
    let pending_journal = store::read_integration_journal(&self.cwd).await?.is_some();
    let decision = CompletionPolicy.evaluate(&CompletionContext {
      catalog,
      evidence: evidence_graph,
      assessment: &assessment,
      deterministic_gate_passed: report.passed,
      verified_revision: &verified_revision,
      current_revision: &current_revision,
      repository_clean,
      has_active_leases: !state.active_leases.is_empty(),
      has_pending_integrations: !state.candidate_integrations.is_empty() || pending_journal,
    });
    match decision {
      CompletionDecision::Done => {
        state.status = RunStatus::Done;
        state.phase = Phase::Complete;
        state.last_summary =
          "All requirements have valid revision-bound evidence and deterministic gates pass".into();
        Ok(Some(state.clone()))
      }
      CompletionDecision::NotReady(blockers) => {
        let message = blockers
          .iter()
          .map(ToString::to_string)
          .collect::<Vec<_>>()
          .join("; ");
        Ok(Some(self.block(context, state, &message).await?))
      }
    }
  }

  async fn ensure_catalog(
    &self,
    context: &BackendContext,
    state: &mut State,
  ) -> Result<RequirementCatalog> {
    let catalog::Inspection {
      specification: spec,
      specification_hash: spec_hash,
      authoritative,
      had_cached_catalog,
    } = catalog::inspect(&self.cwd, &context.config).await?;
    if let Some(catalog) = authoritative {
      context
        .events
        .emit(RunEvent::Catalog(catalog.clone()))
        .await?;
      return Ok(catalog);
    }
    if had_cached_catalog || !state.completed_work_units.is_empty() || !state.discoveries.is_empty()
    {
      state.completed_work_units.clear();
      state.work_statuses.clear();
      state.discoveries.clear();
    }
    let architect_spec = catalog::annotated_specification(&spec);
    state.last_summary = "Deriving requirement catalog from .tenet/spec.md".into();
    self.publish(&context.events, state).await?;
    let backend = self.backend.clone();
    let output = self
      .read_only_worker(context, "architect", move |inspection_context| async move {
        backend
          .architect(&inspection_context, &architect_spec)
          .await
      })
      .await?;
    let catalog = catalog::build(&spec, spec_hash, output, &context.config)?;
    catalog::validate(&catalog)?;
    catalog::validate_coverage(&catalog, &spec)?;
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
    catalog: &RequirementCatalog,
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
    let requests = verification::required_requests(catalog, VerificationRunId::new());
    let result = verifier::run_execution_requests_cancelled(
      &workspace,
      &context.config,
      &requests,
      &context.cancel,
    )
    .await;
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
    if !catalog::specification_changed(&self.cwd, &context.config, &catalog).await? {
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

fn validate_reconcile(catalog: &RequirementCatalog, result: &ReconcileResult) -> Result<()> {
  let expected: BTreeSet<_> = catalog
    .requirements
    .iter()
    .map(|item| item.id.clone())
    .collect();
  let actual: BTreeSet<_> = result
    .requirements
    .iter()
    .map(|item| item.requirement_id.clone())
    .collect();
  if expected != actual || actual.len() != result.requirements.len() {
    bail!("reconciliation assessment IDs do not match the requirement catalog");
  }
  let criterion_requirements: BTreeMap<_, _> = catalog
    .acceptance_criteria
    .iter()
    .map(|criterion| (criterion.id.clone(), criterion.requirement_id.clone()))
    .collect();
  let obligation_criteria: BTreeMap<_, _> = catalog
    .verification_obligations
    .iter()
    .map(|obligation| (obligation.id.clone(), obligation.criterion_id.clone()))
    .collect();
  let mut implementation_gap = false;
  for assessment in &result.requirements {
    if assessment
      .observations
      .iter()
      .chain(&assessment.missing_implementation)
      .any(|value| value.trim().is_empty())
    {
      bail!(
        "{} contains a blank implementation observation",
        assessment.requirement_id
      );
    }
    match assessment.implementation_state {
      ImplementationState::Present if !assessment.missing_implementation.is_empty() => {
        bail!(
          "present implementation {} cannot report implementation gaps",
          assessment.requirement_id
        )
      }
      ImplementationState::Partial | ImplementationState::Absent | ImplementationState::Unknown => {
        implementation_gap = true;
        if assessment.missing_implementation.is_empty() {
          bail!(
            "incomplete implementation {} requires a concrete gap",
            assessment.requirement_id
          );
        }
      }
      ImplementationState::Present => {}
    }
    for obligation_id in &assessment.missing_evidence {
      let Some(criterion_id) = obligation_criteria.get(obligation_id) else {
        bail!(
          "{} reports an unknown missing evidence obligation",
          assessment.requirement_id
        );
      };
      if criterion_requirements.get(criterion_id) != Some(&assessment.requirement_id) {
        bail!(
          "{} reports missing evidence owned by another requirement",
          assessment.requirement_id
        );
      }
    }
  }
  for work in &result.work_units {
    for criterion_id in &work.criterion_ids {
      let requirement_id = criterion_requirements
        .get(criterion_id)
        .context("work unit targets an unknown acceptance criterion")?;
      if !work.requirement_ids.contains(requirement_id) {
        bail!(
          "{} criterion relationships do not match its requirements",
          work.id
        );
      }
    }
    for obligation_id in &work.verification_obligation_ids {
      let criterion_id = obligation_criteria
        .get(obligation_id)
        .context("work unit targets an unknown verification obligation")?;
      if !work.criterion_ids.contains(criterion_id) {
        bail!(
          "{} obligation relationships do not match its criteria",
          work.id
        );
      }
    }
    if work.suggested_checks.iter().any(|check| {
      !work
        .verification_obligation_ids
        .contains(&check.obligation_id)
    }) {
      bail!(
        "{} binds a check outside its verification obligations",
        work.id
      );
    }
  }
  if implementation_gap && result.work_units.is_empty() {
    bail!("implementation gaps require at least one proposed work unit");
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

fn requirement_counts(
  catalog: &RequirementCatalog,
  result: &ReconcileResult,
  graph: &EvidenceGraphState,
) -> Result<RequirementCounts> {
  let implementations: BTreeMap<_, _> = result
    .requirements
    .iter()
    .map(|item| (item.requirement_id.clone(), item.implementation_state))
    .collect();
  let mut counts = RequirementCounts {
    total: catalog.requirements.len(),
    ..Default::default()
  };
  for requirement in &catalog.requirements {
    match graph.requirement_verification_state(&requirement.id, &EvidencePolicy)? {
      VerificationState::Verified => counts.verified += 1,
      VerificationState::PartiallyVerified => counts.partially_verified += 1,
      VerificationState::Unverified => counts.unverified += 1,
      VerificationState::Stale => counts.stale += 1,
      VerificationState::Contradicted => counts.contradicted += 1,
    }
    if implementations.get(&requirement.id).is_some_and(|state| {
      matches!(
        state,
        ImplementationState::Partial | ImplementationState::Absent | ImplementationState::Unknown
      )
    }) {
      counts.missing_implementation += 1;
    }
  }
  Ok(counts)
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
  let catalog = store::read_catalog(cwd)
    .await?
    .context("manual verification requires an initialized requirement catalog")?;
  catalog::validate(&catalog)?;
  let (specification, _) = store::spec_text_and_hash(cwd, &config).await?;
  catalog::validate_coverage(&catalog, &specification)?;
  let revision = git::head(cwd).await?;
  let requests = verification::required_requests(&catalog, VerificationRunId::new());
  let report =
    verifier::run_execution_requests_cancelled(cwd, &config, &requests, &CancellationToken::new())
      .await?;
  let mut graph = evidence_graph::load(cwd, &catalog).await?;
  evidence_graph::invalidate_for_revision(cwd, &mut graph, &revision).await?;
  evidence_graph::record_report(&mut graph, &revision, &report)?;
  store::write_evidence_graph(cwd, &graph).await?;
  Ok(report)
}

#[cfg(test)]
mod tests {
  use super::*;
  use tenet_domain::{
    config::Config,
    evidence::{AcceptanceCriterion, VerificationKind, VerificationObligation},
    ids::{CriterionId, ObligationId, RequirementId},
    model::{ArchitectOutput, Requirement, RequirementAssessment},
    verification::{DependencyScopeAuthority, VerificationAuthority, VerificationSpec},
    worker::{derive_normative_fragments, CatalogCoverage},
  };

  fn catalog() -> RequirementCatalog {
    let fragments = derive_normative_fragments("Description");
    let requirements = vec![Requirement {
      id: RequirementId::from("REQ-001"),
      title: "Requirement".into(),
      description: "Description".into(),
      required: true,
      source_refs: vec![fragments[0].reference()],
    }];
    RequirementCatalog {
      spec_hash: "spec".into(),
      coverage: CatalogCoverage::derive("Description", &requirements),
      requirements,
      acceptance_criteria: vec![AcceptanceCriterion {
        id: CriterionId::from("REQ-001/AC-01"),
        requirement_id: RequirementId::from("REQ-001"),
        description: "Observable result".into(),
        mandatory: true,
      }],
      verification_obligations: vec![VerificationObligation {
        id: ObligationId::from("REQ-001/AC-01/VO-01"),
        criterion_id: CriterionId::from("REQ-001/AC-01"),
        description: "Run test".into(),
        kind: VerificationKind::AutomatedTest,
        required: true,
        spec: VerificationSpec {
          program: "cargo".into(),
          args: vec!["test".into()],
          working_directory: ".".into(),
          environment: Default::default(),
        },
        authority: VerificationAuthority::ProjectConfigured,
        dependency_scope: vec!["src/**".into()],
        dependency_scope_authority: DependencyScopeAuthority::ProjectConfigured,
      }],
    }
  }

  fn result(state: ImplementationState, gaps: Vec<&str>, with_work: bool) -> ReconcileResult {
    ReconcileResult {
      summary: "assessment".into(),
      requirements: vec![RequirementAssessment {
        requirement_id: RequirementId::from("REQ-001"),
        implementation_state: state,
        observations: vec!["src/lib.rs contains the implementation".into()],
        missing_implementation: gaps.into_iter().map(str::to_owned).collect(),
        missing_evidence: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
      }],
      work_units: if with_work {
        vec![tenet_domain::model::WorkUnit {
          id: "A".into(),
          title: "Work".into(),
          objective: "Implement".into(),
          requirement_ids: vec![RequirementId::from("REQ-001")],
          criterion_ids: vec![CriterionId::from("REQ-001/AC-01")],
          verification_obligation_ids: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
          suggested_checks: Vec::new(),
          depends_on: Vec::new(),
          scope: tenet_domain::model::WorkScope {
            paths: vec!["src/**".into()],
          },
        }]
      } else {
        Vec::new()
      },
    }
  }

  #[test]
  fn present_implementation_with_missing_evidence_is_a_valid_proposal() {
    assert!(validate_reconcile(
      &catalog(),
      &result(ImplementationState::Present, Vec::new(), false)
    )
    .is_ok());
  }

  #[test]
  fn incomplete_implementation_requires_a_concrete_gap() {
    let error = validate_reconcile(
      &catalog(),
      &result(ImplementationState::Absent, Vec::new(), true),
    )
    .expect_err("missing gap rejected");

    assert!(error.to_string().contains("requires a concrete gap"));
  }

  #[test]
  fn implementation_gap_requires_proposed_work() {
    let error = validate_reconcile(
      &catalog(),
      &result(
        ImplementationState::Partial,
        vec!["missing behavior"],
        false,
      ),
    )
    .expect_err("missing work rejected");

    assert!(error
      .to_string()
      .contains("require at least one proposed work unit"));
  }

  #[test]
  fn blank_catalog_acceptance_criterion_is_rejected() {
    let mut invalid = catalog();
    invalid.acceptance_criteria[0].description = " ".into();

    assert!(catalog::validate(&invalid).is_err());
  }
  fn architect_output(requirement: Requirement) -> ArchitectOutput {
    ArchitectOutput {
      requirements: vec![requirement],
      acceptance_criteria: catalog().acceptance_criteria,
      verification_obligations: catalog().verification_obligations,
    }
  }

  #[test]
  fn architect_optional_flags_are_normalized_to_required() {
    let mut requirement = catalog().requirements.remove(0);
    requirement.required = false;
    let mut output = architect_output(requirement);
    output.acceptance_criteria[0].mandatory = false;
    output.verification_obligations[0].required = false;
    let built = catalog::build("Description", "spec".into(), output, &Config::default())
      .expect("build catalog");

    assert!(built.requirements[0].required);
    assert!(built.acceptance_criteria[0].mandatory);
    assert!(built.verification_obligations[0].required);
    assert_eq!(
      built.verification_obligations[0].authority,
      VerificationAuthority::AgentProposed
    );
  }

  #[test]
  fn omitted_normative_fragment_blocks_catalog_coverage() {
    let specification = "First normative statement.\n\nSecond normative statement.";
    let fragments = derive_normative_fragments(specification);
    let mut requirement = catalog().requirements.remove(0);
    requirement.source_refs = vec![fragments[0].reference()];
    let built = catalog::build(
      specification,
      "spec".into(),
      architect_output(requirement),
      &Config::default(),
    )
    .expect("build catalog");

    assert!(catalog::validate(&built).is_ok());
    assert!(catalog::validate_coverage(&built, specification).is_err());
    assert_eq!(
      built.coverage.uncovered_fragment_ids,
      vec![fragments[1].id.clone()]
    );
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
