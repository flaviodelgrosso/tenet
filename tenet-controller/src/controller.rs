use std::{
  collections::{BTreeMap, BTreeSet},
  future::Future,
  path::{Path, PathBuf},
  sync::Arc,
};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use tenet_domain::{
  config::{config_path, ensure_config, read_config, TENET_DIR},
  events::{
    CompletionGate, CompletionGateItem, CompletionGateOutcome, EventSink, RunEvent, RunLogger,
  },
  evidence::{
    EvidenceGraphState, EvidencePolicy, ImplementationState, ObligationAssessment,
    SemanticAssessmentProposal, SemanticAssessmentReport, VerificationState,
  },
  model::{
    AgentReconciliationProposal, CompletedWorkUnit, Discovery, DiscoveryStatus, IntegrationPhase,
    Phase, ReconcileResult, RepairProgress, RequirementCatalog, RequirementCounts, RunStatus,
    State, VerificationLayers, VerificationReport, WorkExecution, WorkStatus, WorkerDiscovery,
  },
  verification::ProjectVerificationRun,
};

use tenet_runtime::{
  backend::BackendContext,
  git,
  graph::WorkGraph,
  integration::{IntegrationOutcome, Integrator},
  scheduler::{CandidateExecutor, ExecutionUpdate, Scheduler},
  store::{self, RunLock},
  verifier,
  workspace::WorkspaceManager,
};

use crate::{
  catalog,
  completion::{CompletionBlocker, CompletionContext, CompletionDecision, CompletionPolicy},
  decision, evidence as evidence_graph,
  ports::agent::{AgentBackend, ReconciliationRequest, SemanticAssessmentRequest},
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
    discoveries: &[Discovery],
    previous_verification: Option<&VerificationReport>,
  ) -> Result<tenet_domain::model::WorkerSummary> {
    match previous_verification {
      Some(report) => {
        self
          .backend
          .repair(context, catalog, work_unit, discoveries, report)
          .await
      }
      None => {
        self
          .backend
          .implement(context, catalog, work_unit, discoveries)
          .await
      }
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
    self.initialize().await?;
    let _lock = RunLock::acquire(&self.cwd)?;
    let config = Arc::new(ensure_config(&self.cwd).await?);
    git::head(&self.cwd).await?;
    let mut state = store::read_state(&self.cwd).await?;
    store::recover_integration(&self.cwd, &mut state, &config).await?;
    if !git::is_clean(&self.cwd).await? {
      bail!("worktree execution requires a clean canonical working tree");
    }
    let launch = self.backend.resolve_launch(&self.cwd, &configured).await?;

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
    if store::has_unfinished_integration(&self.cwd).await? {
      bail!("cannot replace current run state while an integration transaction requires recovery");
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
    state.work_statuses.clear();
    state.last_summary = "Latest run attempt failed during preflight".into();
    state.last_error = Some(error.to_string());
    state.updated_at = Utc::now().to_rfc3339();
    store::write_state(&self.cwd, &state).await
  }

  async fn run_inner(&self, context: &BackendContext, state: &mut State) -> Result<State> {
    let mut catalog = self.ensure_catalog(context, state).await?;
    if !store::catalog_is_approved(&self.cwd, &catalog).await? {
      return self.review_required(context, state, &catalog).await;
    }
    if context.config.verification.checks.is_empty() {
      return self
        .block(
          context,
          state,
          "No trusted project verification checks are configured.\n\nConfigure at least one:\n\n[[verification.checks]]\nname = \"project verification\"\ncommand = [\"./verify\"]",
        )
        .await;
    }
    let mut evidence_graph = evidence_graph::load(&self.cwd, &catalog).await?;
    store::write_evidence_graph(&self.cwd, &evidence_graph).await?;

    for cycle in 1..=context.config.max_cycles {
      self.check_cancel(context)?;
      catalog = self
        .refresh_catalog_if_spec_changed(context, state, catalog)
        .await?;
      if !store::catalog_is_approved(&self.cwd, &catalog).await? {
        return self.review_required(context, state, &catalog).await;
      }
      evidence_graph = evidence_graph::load(&self.cwd, &catalog).await?;
      store::write_evidence_graph(&self.cwd, &evidence_graph).await?;
      let current_revision = git::head(&self.cwd).await?;
      prune_deferred_candidates(&self.cwd, state, &catalog, &current_revision).await?;
      let suite_hash = context.config.verification.suite_hash()?;
      evidence_graph::invalidate(
        &self.cwd,
        &context.events,
        &mut evidence_graph,
        &current_revision,
        &suite_hash,
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
      let catalog_for_worker = decision::reconciliation_catalog(&catalog);
      let requirement_handles_for_worker = decision::reconciliation_handles(&catalog);
      let completed_for_worker = state.completed_work_units.clone();
      let discoveries_for_worker: Vec<_> = state
        .discoveries
        .iter()
        .map(|record| record.discovery.clone())
        .collect();
      let evidence_for_worker = evidence_graph::projections(
        &evidence_graph,
        EvidencePolicy::new(&current_revision, &suite_hash),
      )?;
      let backend = self.backend.clone();
      let reconciliation = self
        .validated_read_only_proposal(
          context,
          &catalog,
          &current_revision,
          "reconciliation",
          move |inspection_context, feedback| {
            let backend = backend.clone();
            let catalog = catalog_for_worker.clone();
            let completed = completed_for_worker.clone();
            let requirement_handles = requirement_handles_for_worker.clone();
            let discoveries = discoveries_for_worker.clone();
            let evidence = evidence_for_worker.clone();
            async move {
              backend
                .reconcile(
                  &inspection_context,
                  ReconciliationRequest {
                    catalog: &catalog,
                    requirement_handles: &requirement_handles,
                    recent_completed: &completed,
                    discoveries: &discoveries,
                    evidence: &evidence,
                  },
                  feedback.as_deref(),
                )
                .await
            }
          },
        )
        .await?;
      let graph = WorkGraph::from_reconcile(&catalog, &reconciliation)?;
      let active_discoveries: Vec<_> = state
        .discoveries
        .iter()
        .filter(|record| record.status == DiscoveryStatus::Active)
        .map(|record| record.fingerprint.clone())
        .collect();
      let fingerprint = decision::progress_fingerprint(
        &current_revision,
        &catalog.spec_hash,
        &reconciliation,
        &active_discoveries,
      )?;
      let stagnation = decision::advance_stagnation(
        state.progress_fingerprint.as_deref(),
        state.stagnation_count,
        fingerprint,
        context.config.stagnation_limit,
      );
      state.progress_fingerprint = Some(stagnation.fingerprint);
      state.stagnation_count = stagnation.count;
      // Historical execution records are reconciler context, never current scheduling authority.
      let completed = BTreeSet::new();
      let active: BTreeSet<_> = state
        .active_leases
        .values()
        .map(|lease| lease.work_unit.id.clone())
        .collect();
      let frontier = graph.ready_frontier(&completed, &active);
      let next = decision::decide_reconciliation(
        state.status.clone(),
        !reconciliation.work_units.is_empty(),
        frontier,
        stagnation
          .blocked
          .then_some(context.config.stagnation_limit),
      );
      match &next {
        decision::NextAction::Block(reason) => return self.block(context, state, reason).await,
        decision::NextAction::Finish => return Ok(state.clone()),
        _ => {}
      }
      for record in &mut state.discoveries {
        let deferred = state.deferred_candidates.iter().any(|candidate| {
          candidate.catalog_hash == record.catalog_hash
            && candidate.base_revision == record.repository_revision
            && candidate.lease.work_unit.id == record.work_unit_id
            && matches!(record.discovery, Discovery::ScopeExpansion { .. })
        });
        record.status = if deferred {
          DiscoveryStatus::Active
        } else {
          DiscoveryStatus::Consumed
        };
      }
      store::write_roadmap(
        &self.cwd,
        state.run_id.as_deref().context("run id missing")?,
        state.cycle,
        &current_revision,
        &catalog.spec_hash,
        &reconciliation,
      )
      .await?;
      state.requirement_counts = requirement_counts(
        &catalog,
        &evidence_graph,
        EvidencePolicy::new(&current_revision, &suite_hash),
      )?;
      state.requirement_counts.missing_implementation = reconciliation
        .requirements
        .iter()
        .filter(|assessment| assessment.implementation_state != ImplementationState::Present)
        .count();
      state.last_summary.clone_from(&reconciliation.summary);
      context
        .events
        .emit(RunEvent::Reconcile(reconciliation.clone()))
        .await?;
      self.publish(&context.events, state).await?;

      let frontier = match next {
        decision::NextAction::VerifyProject => {
          if let Some(final_state) = self
            .finalize(context, state, &catalog, &mut evidence_graph)
            .await?
          {
            return Ok(final_state);
          }
          continue;
        }
        decision::NextAction::ExecuteFrontier(frontier) => frontier,
        decision::NextAction::Block(_)
        | decision::NextAction::Finish
        | decision::NextAction::IntegrateCandidates(_)
        | decision::NextAction::BeginNextCycle => {
          bail!("reconciliation policy returned an action invalid for this driver step")
        }
      };
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

      let scheduling_repository = git::repository_state(&self.cwd).await?;
      if !scheduling_repository.status.is_empty() {
        return self
          .block(
            context,
            state,
            "Canonical working tree became dirty before scheduling",
          )
          .await;
      }
      if scheduling_repository.head != current_revision {
        state.last_summary =
          "Canonical revision changed after reconciliation; beginning a new cycle".into();
        self.publish(&context.events, state).await?;
        continue;
      }
      let base_revision = current_revision.clone();
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
      let authorization = decision::authorize_frontier(
        frontier,
        &state.deferred_candidates,
        &catalog.spec_hash,
        &base_revision,
      )?;
      let scheduled_frontier = authorization.work_units;
      let selected_candidates = authorization.deferred_candidates;
      if scheduled_frontier.is_empty() {
        self.publish(&context.events, state).await?;
        continue;
      }
      let leases = scheduler.issue_leases(scheduled_frontier, &base_revision);
      let mut deferred_by_lease = BTreeMap::new();
      let mut selected_refs = Vec::new();
      for (lease, candidate) in leases.iter().zip(selected_candidates) {
        if let Some(candidate) = candidate {
          selected_refs.push(candidate.git_ref.clone());
          deferred_by_lease.insert(lease.id.clone(), candidate);
        }
      }
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
        let mut execution = Box::pin(scheduler.execute_leases(leases, deferred_by_lease));
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
      let outcome = match (execution_result, cleanup_result) {
        (Ok(outcome), Ok(())) => outcome,
        (Err(error), Ok(())) => return Err(error),
        (Ok(_), Err(error)) => return Err(error.context("scheduler cleanup")),
        (Err(error), Err(cleanup)) => {
          return Err(error.context(format!("scheduler cleanup also failed: {cleanup:#}")))
        }
      };
      let next = decision::decide_execution(state.status.clone(), outcome.executions);
      let candidates = match &next {
        decision::NextAction::IntegrateCandidates(candidates) => candidates.clone(),
        decision::NextAction::BeginNextCycle | decision::NextAction::Finish => Vec::new(),
        _ => bail!("execution policy returned an action invalid for this driver step"),
      };
      state.candidate_integrations = candidates.clone();
      for blocked in outcome.blocked {
        state
          .work_statuses
          .insert(blocked.lease.work_unit.id.clone(), WorkStatus::Pending);
        decision::record_discoveries(
          state,
          &catalog.spec_hash,
          &blocked.lease.base_revision,
          &blocked.lease.work_unit.id,
          &blocked.discoveries,
        )?;
        if let Some(candidate) = blocked.candidate {
          state
            .deferred_candidates
            .retain(|existing| existing.git_ref != candidate.git_ref);
          state.deferred_candidates.push(candidate);
        }
      }
      for candidate in &candidates {
        state
          .work_statuses
          .insert(candidate.lease.work_unit.id.clone(), WorkStatus::Candidate);
        decision::record_discoveries(
          state,
          &catalog.spec_hash,
          &candidate.candidate_revision,
          &candidate.lease.work_unit.id,
          &candidate.discoveries,
        )?;
      }
      self.publish(&context.events, state).await?;

      match next {
        decision::NextAction::BeginNextCycle => {
          retire_deferred_candidates(&self.cwd, state, &selected_refs).await?;
          self.publish(&context.events, state).await?;
          continue;
        }
        decision::NextAction::Finish => return Ok(state.clone()),
        decision::NextAction::IntegrateCandidates(candidates) => {
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
        }
        _ => bail!("execution policy returned an action invalid for integration"),
      }
      retire_deferred_candidates(&self.cwd, state, &selected_refs).await?;
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
          let suite_hash = context.config.verification.suite_hash()?;
          evidence_graph::invalidate(
            &self.cwd,
            &context.events,
            evidence_graph,
            &revision,
            &suite_hash,
          )
          .await?;
          evidence_graph::record_project_verification(&self.cwd, evidence_graph, &verification)
            .await?;
          context
            .events
            .emit(RunEvent::ProjectVerification(verification.clone()))
            .await?;
          state.completed_work_units.push(CompletedWorkUnit {
            work_unit: transaction.work_unit.clone(),
            completed_at: Utc::now().to_rfc3339(),
            verification_run_id: transaction.verification_run_id,
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
          if let IntegrationOutcome::ProjectVerificationFailed { report } = &outcome {
            state.verification_layers = project_layers(report);
            context
              .events
              .emit(RunEvent::ProjectVerification(report.clone()))
              .await?;
          }
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
    let suite_hash = context.config.verification.suite_hash()?;
    state.phase = Phase::Verifying;
    state.last_summary = "Running controller-owned project verification".into();
    self.publish(&context.events, state).await?;
    let project_report = self.verify_revision(context, &verified_revision).await?;
    evidence_graph::record_project_verification(&self.cwd, evidence_graph, &project_report).await?;
    context
      .events
      .emit(RunEvent::ProjectVerification(project_report.clone()))
      .await?;
    state.verification_layers = project_layers(&project_report);
    state.verification_layers.semantic_obligations_total = catalog
      .verification_obligations
      .iter()
      .filter(|obligation| obligation.required)
      .count();
    self.publish(&context.events, state).await?;
    if !project_report.passed {
      self
        .evaluate_and_publish_completion_gate(
          context,
          state,
          catalog,
          evidence_graph,
          &project_report,
          &suite_hash,
        )
        .await?;
    }
    if !project_report.passed {
      return Ok(Some(
        self
          .block(context, state, "Final project verification failed")
          .await?,
      ));
    }

    state.phase = Phase::Assessing;
    state.last_summary = "Running independent semantic verification".into();
    self.publish(&context.events, state).await?;
    let policy = EvidencePolicy::new(&verified_revision, &suite_hash);
    let catalog_for_worker = decision::semantic_assessment_catalog(catalog);
    let project_for_worker = project_report.clone();
    let obligation_handles_for_worker = decision::semantic_assessment_handles(catalog);
    let evidence_for_worker = evidence_graph::projections(evidence_graph, policy)?;
    let backend = self.backend.clone();
    let semantic_report = self
      .validated_semantic_assessment(
        context,
        catalog,
        &verified_revision,
        move |inspection_context, feedback| {
          let backend = backend.clone();
          let catalog = catalog_for_worker.clone();
          let project = project_for_worker.clone();
          let evidence = evidence_for_worker.clone();
          let obligation_handles = obligation_handles_for_worker.clone();
          async move {
            backend
              .assess(
                &inspection_context,
                SemanticAssessmentRequest {
                  catalog: &catalog,
                  obligation_handles: &obligation_handles,
                  project_verification: &project,
                  evidence: &evidence,
                },
                feedback.as_deref(),
              )
              .await
          }
        },
      )
      .await?;
    context
      .events
      .emit(RunEvent::SemanticAssessment(semantic_report.clone()))
      .await?;
    let assessment_worker_id = format!("{}-assessment", context.worker_id);
    evidence_graph::establish_semantic_assessment(
      &self.cwd,
      &context.events,
      evidence_graph,
      &verified_revision,
      &suite_hash,
      &assessment_worker_id,
      &semantic_report,
    )
    .await?;
    let policy = EvidencePolicy::new(&verified_revision, &suite_hash);
    state.requirement_counts = requirement_counts(catalog, evidence_graph, policy)?;
    apply_semantic_layers(&mut state.verification_layers, evidence_graph, policy);
    state.last_summary.clone_from(&semantic_report.summary);

    let gaps: Vec<_> = semantic_report
      .assessments
      .iter()
      .filter_map(|assessment| match &assessment.assessment {
        ObligationAssessment::Gap { description } => Some(WorkerDiscovery {
          discovery: Discovery::VerificationBlocker {
            description: format!("{}: {description}", assessment.obligation_id),
          },
          role: tenet_domain::model::WorkerRole::Assess,
        }),
        ObligationAssessment::Satisfied { .. } | ObligationAssessment::Uncertain { .. } => None,
      })
      .collect();
    if !gaps.is_empty() {
      decision::record_discoveries(
        state,
        &catalog.spec_hash,
        &verified_revision,
        "semantic-assessment",
        &gaps,
      )?;
      self.publish(&context.events, state).await?;
      return Ok(None);
    }
    if semantic_report.assessments.iter().any(|assessment| {
      matches!(
        assessment.assessment,
        ObligationAssessment::Uncertain { .. }
      )
    }) {
      self
        .evaluate_and_publish_completion_gate(
          context,
          state,
          catalog,
          evidence_graph,
          &project_report,
          &suite_hash,
        )
        .await?;
      return Ok(Some(
        self
          .block(
            context,
            state,
            "Independent semantic verification is uncertain; specification clarification or stronger evidence is required",
          )
          .await?,
      ));
    }

    let decision = self
      .evaluate_and_publish_completion_gate(
        context,
        state,
        catalog,
        evidence_graph,
        &project_report,
        &suite_hash,
      )
      .await?;
    match decision::apply_completion_decision(state, &decision) {
      decision::NextAction::Finish => Ok(Some(state.clone())),
      decision::NextAction::Block(reason) => Ok(Some(self.block(context, state, &reason).await?)),
      _ => bail!("completion policy returned an action invalid for this driver step"),
    }
  }

  async fn evaluate_and_publish_completion_gate(
    &self,
    context: &BackendContext,
    state: &State,
    catalog: &RequirementCatalog,
    evidence_graph: &EvidenceGraphState,
    project_report: &ProjectVerificationRun,
    suite_hash: &str,
  ) -> Result<CompletionDecision> {
    let current_revision = git::head(&self.cwd).await?;
    let repository_clean = git::is_clean(&self.cwd).await?;
    let pending_journal = store::read_integration_journal(&self.cwd).await?.is_some();
    let decision = CompletionPolicy.evaluate(&CompletionContext {
      catalog,
      evidence: evidence_graph,
      project_verification: project_report,
      current_suite_hash: suite_hash,
      current_revision: &current_revision,
      repository_clean,
      has_active_leases: !state.active_leases.is_empty(),
      has_pending_integrations: !state.candidate_integrations.is_empty()
        || !state.deferred_candidates.is_empty()
        || pending_journal,
    });
    context
      .events
      .emit(RunEvent::CompletionGate(completion_gate(
        &decision,
        state,
        current_revision,
      )))
      .await?;
    Ok(decision)
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
    if had_cached_catalog
      || !state.completed_work_units.is_empty()
      || !state.discoveries.is_empty()
      || !state.deferred_candidates.is_empty()
    {
      let references = decision::plan_spec_invalidation(state)?;
      retire_deferred_candidates(&self.cwd, state, &references).await?;
      decision::apply_spec_invalidation(state);
    }
    let catalog_authority = catalog::CatalogAuthority::derive(&spec);
    let architect_batches = catalog_authority.architect_batches();
    state.last_summary = format!(
      "Deriving requirement catalog from {} in {} deterministic architect batch(es)",
      context.config.spec_file,
      architect_batches.len()
    );
    self.publish(&context.events, state).await?;
    let mut catalog_batches = Vec::with_capacity(architect_batches.len());
    for batch in architect_batches {
      let mut feedback = None;
      let mut attempt = 0;
      let candidate = loop {
        let architect_spec = match &feedback {
          Some(feedback) => format!(
            "{}\nDeterministic catalog validation rejected the previous result for this batch:\n{feedback}\nRegenerate this batch catalog and cover every assigned sourceRef token in at least one requirement sourceRefs entry.",
            batch.annotated_specification
          ),
          None => batch.annotated_specification.clone(),
        };
        let backend = self.backend.clone();
        let output = self
          .read_only_worker(
            context,
            "architect",
            None,
            move |inspection_context| async move {
              backend
                .architect(&inspection_context, &architect_spec)
                .await
            },
          )
          .await?;
        let candidate = catalog_authority
          .build_batch(&batch, spec_hash.clone(), output)
          .and_then(|candidate| {
            catalog::validate(&candidate)?;
            catalog::validate_batch_coverage(&candidate, &batch.fragment_ids)?;
            Ok(candidate)
          });
        match candidate {
          Ok(candidate) => break candidate,
          Err(error) if attempt == context.config.agent.completion_retries => return Err(error),
          Err(error) => {
            attempt += 1;
            feedback = Some(format!("{error:#}"));
          }
        }
      };
      catalog_batches.push(candidate);
    }
    let catalog = catalog_authority.merge_batches(spec_hash, catalog_batches)?;
    catalog::validate(&catalog)?;
    catalog::validate_derived_coverage(&catalog)?;
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
  ) -> Result<ProjectVerificationRun> {
    let run_id = context
      .runtime_dir
      .file_name()
      .and_then(|value| value.to_str())
      .context("verification run id is unavailable")?;
    let workspaces = WorkspaceManager::new(self.cwd.clone(), run_id);
    verifier::run_project_verification_isolated(
      &self.cwd,
      &workspaces,
      revision,
      &context.config,
      "final-project-verification",
      &context.cancel,
    )
    .await
  }

  async fn validated_read_only_proposal<F, Fut>(
    &self,
    context: &BackendContext,
    catalog: &RequirementCatalog,
    expected_revision: &str,
    name: &str,
    generate: F,
  ) -> Result<ReconcileResult>
  where
    F: Fn(BackendContext, Option<String>) -> Fut + Clone,
    Fut: Future<Output = Result<AgentReconciliationProposal>>,
  {
    let mut feedback = None;
    for attempt in 0..=context.config.agent.completion_retries {
      let generate = generate.clone();
      let attempt_feedback = feedback.clone();
      let proposal = self
        .read_only_worker(
          context,
          name,
          Some(expected_revision),
          move |inspection_context| generate(inspection_context, attempt_feedback),
        )
        .await?;
      let result = decision::materialize_reconciliation(catalog, proposal).and_then(|result| {
        decision::validate_reconciliation(&result)?;
        WorkGraph::from_reconcile(catalog, &result)?;
        Ok(result)
      });
      match result {
        Ok(result) => return Ok(result),
        Err(error) if attempt == context.config.agent.completion_retries => return Err(error),
        Err(error) => {
          feedback = Some(decision::reconciliation_retry_feedback(&error));
        }
      }
    }
    unreachable!("semantic proposal retry loop always returns")
  }

  async fn validated_semantic_assessment<F, Fut>(
    &self,
    context: &BackendContext,
    catalog: &RequirementCatalog,
    expected_revision: &str,
    generate: F,
  ) -> Result<SemanticAssessmentReport>
  where
    F: Fn(BackendContext, Option<String>) -> Fut + Clone,
    Fut: Future<Output = Result<SemanticAssessmentProposal>>,
  {
    let mut feedback = None;
    for attempt in 0..=context.config.agent.completion_retries {
      let generate = generate.clone();
      let attempt_feedback = feedback.clone();
      let proposal = self
        .read_only_worker(
          context,
          "assessment",
          Some(expected_revision),
          move |inspection_context| generate(inspection_context, attempt_feedback),
        )
        .await?;
      let result =
        decision::materialize_semantic_assessment(catalog, proposal).and_then(|result| {
          verification::validate_semantic_assessment(catalog, &result)?;
          Ok(result)
        });
      match result {
        Ok(result) => return Ok(result),
        Err(error) if attempt == context.config.agent.completion_retries => return Err(error),
        Err(error) => {
          feedback = Some(format!(
            "{error:#}\nReturn exactly one semantic judgment for every controller-selected obligation with its supplied obligationHandle."
          ));
        }
      }
    }
    unreachable!("semantic assessment retry loop always returns")
  }

  async fn read_only_worker<T, F, Fut>(
    &self,
    context: &BackendContext,
    name: &str,
    expected_revision: Option<&str>,
    call: F,
  ) -> Result<T>
  where
    F: FnOnce(BackendContext) -> Fut,
    Fut: Future<Output = Result<T>>,
  {
    let canonical_before = git::repository_state(&self.cwd).await?;
    if let Some(expected_revision) = expected_revision {
      if canonical_before.head != expected_revision {
        bail!(
          "{name} inspection revision changed before workspace creation (expected {expected_revision}, observed {})",
          canonical_before.head
        );
      }
    }
    let run_id = context
      .runtime_dir
      .file_name()
      .and_then(|value| value.to_str())
      .context("read-only worker run id is unavailable")?;
    let workspaces = WorkspaceManager::new(self.cwd.clone(), run_id);
    let workspace = workspaces
      .create_disposable(name, &canonical_before.head)
      .await?;
    workspaces
      .materialize_repository_file(&workspace, &context.config.spec_file)
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
    let references = decision::plan_spec_invalidation(state)?;
    retire_deferred_candidates(&self.cwd, state, &references).await?;
    decision::apply_spec_invalidation(state);
    self.ensure_catalog(context, state).await
  }

  async fn review_required(
    &self,
    context: &BackendContext,
    state: &mut State,
    catalog: &RequirementCatalog,
  ) -> Result<State> {
    state.status = RunStatus::ReviewRequired;
    state.phase = Phase::ReviewingRequirements;
    state.active_leases.clear();
    state.candidate_integrations.clear();
    state.current_repair = None;
    state.blocked_reason = None;
    state.last_error = None;
    state.requirement_counts = RequirementCounts {
      total: catalog.requirements.len(),
      ..Default::default()
    };
    state.last_summary =
      "Requirement catalog requires human approval before autonomous execution".into();
    self.publish(&context.events, state).await?;
    context.events.emit(RunEvent::Message(format!(
      "Requirement catalog generated.\n\n{} requirements\n{} acceptance criteria\n{} verification obligations\nSpecification coverage: complete\n\nHuman approval is required before autonomous execution.\n\nReview:\n  tenet requirements dump\n\nApprove:\n  tenet requirements approve",
      catalog.requirements.len(),
      catalog.acceptance_criteria.len(),
      catalog.verification_obligations.len(),
    ))).await?;
    context
      .events
      .emit(RunEvent::Finished(state.clone()))
      .await?;
    Ok(state.clone())
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

async fn prune_deferred_candidates(
  cwd: &Path,
  state: &mut State,
  catalog: &RequirementCatalog,
  revision: &str,
) -> Result<()> {
  let classification = decision::classify_deferred_candidates(
    std::mem::take(&mut state.deferred_candidates),
    &catalog.spec_hash,
    revision,
  );
  for candidate in &classification.retained {
    let resolved = git::resolve_ref(cwd, &candidate.git_ref).await?;
    if resolved != candidate.candidate_revision {
      bail!("deferred candidate Git ref does not match persisted revision");
    }
  }
  for reference in &classification.stale_refs {
    git::delete_ref(cwd, reference).await?;
  }
  state.deferred_candidates = classification.retained;
  Ok(())
}

async fn retire_deferred_candidates(
  cwd: &Path,
  state: &mut State,
  references: &[String],
) -> Result<()> {
  for reference in references {
    git::delete_ref(cwd, reference).await?;
  }
  state
    .deferred_candidates
    .retain(|candidate| !references.contains(&candidate.git_ref));
  Ok(())
}

fn requirement_counts(
  catalog: &RequirementCatalog,
  graph: &EvidenceGraphState,
  policy: EvidencePolicy<'_>,
) -> Result<RequirementCounts> {
  let mut counts = RequirementCounts {
    total: catalog.requirements.len(),
    ..Default::default()
  };
  for requirement in &catalog.requirements {
    match graph.requirement_verification_state(&requirement.id, policy)? {
      VerificationState::Verified => counts.verified += 1,
      VerificationState::PartiallyVerified => counts.partially_verified += 1,
      VerificationState::Unverified => counts.unverified += 1,
      VerificationState::Uncertain => counts.uncertain += 1,
      VerificationState::Stale => counts.stale += 1,
      VerificationState::Contradicted => counts.contradicted += 1,
    }
  }
  Ok(counts)
}

fn completion_gate(
  decision: &CompletionDecision,
  state: &State,
  revision: String,
) -> CompletionGate {
  let blockers = match decision {
    CompletionDecision::Done => &[][..],
    CompletionDecision::NotReady(blockers) => blockers.as_slice(),
  };
  let outcome = |blocked| {
    if blocked {
      CompletionGateOutcome::Unsatisfied
    } else {
      CompletionGateOutcome::Satisfied
    }
  };
  let blocked = |predicate: fn(&CompletionBlocker) -> bool| blockers.iter().any(predicate);
  let item = |label: &str, blocked: bool, detail: String| CompletionGateItem {
    label: label.into(),
    outcome: outcome(blocked),
    detail,
  };
  let coverage_incomplete = blocked(|blocker| {
    matches!(
      blocker,
      CompletionBlocker::SpecificationCoverageIncomplete(_)
    )
  });
  let items = vec![
    item(
      "specification coverage",
      coverage_incomplete,
      if coverage_incomplete {
        "incomplete".into()
      } else {
        "complete".into()
      },
    ),
    item(
      "project verification",
      blocked(|blocker| {
        matches!(
          blocker,
          CompletionBlocker::ProjectVerificationFailed
            | CompletionBlocker::ProjectVerificationStale
        )
      }),
      format!(
        "{}/{}",
        state.verification_layers.project_checks_passed,
        state.verification_layers.project_checks_total
      ),
    ),
    item(
      "requirements",
      blocked(|blocker| {
        matches!(
          blocker,
          CompletionBlocker::RequirementUnverified(_) | CompletionBlocker::CriterionUnverified(_)
        )
      }),
      format!(
        "{}/{} verified",
        state.requirement_counts.verified, state.requirement_counts.total
      ),
    ),
    item(
      "semantic obligations",
      blocked(|blocker| {
        matches!(
          blocker,
          CompletionBlocker::SemanticGap(_) | CompletionBlocker::SemanticUncertain(_)
        )
      }),
      format!(
        "{}/{} satisfied",
        state.verification_layers.semantic_satisfied,
        state.verification_layers.semantic_obligations_total
      ),
    ),
    item(
      "repository clean",
      blocked(|blocker| matches!(blocker, CompletionBlocker::RepositoryDirty)),
      String::new(),
    ),
    item(
      "canonical revision stable",
      blocked(|blocker| {
        matches!(
          blocker,
          CompletionBlocker::ProjectVerificationStale
            | CompletionBlocker::RepositoryChangedAfterVerification
        )
      }),
      String::new(),
    ),
    item(
      "no active leases",
      blocked(|blocker| matches!(blocker, CompletionBlocker::ActiveLease)),
      String::new(),
    ),
    item(
      "no pending integration",
      blocked(|blocker| matches!(blocker, CompletionBlocker::PendingIntegration)),
      String::new(),
    ),
  ];
  CompletionGate {
    revision,
    earned: matches!(decision, CompletionDecision::Done),
    items,
    blockers: blockers.iter().map(ToString::to_string).collect(),
  }
}

fn project_layers(report: &ProjectVerificationRun) -> VerificationLayers {
  VerificationLayers {
    project_checks_total: report.checks.len(),
    project_checks_passed: report
      .checks
      .iter()
      .filter(|check| check.result.exit_code == Some(0) && !check.result.timed_out)
      .count(),
    project_passed: report.passed,
    ..VerificationLayers::default()
  }
}

fn apply_semantic_layers(
  layers: &mut VerificationLayers,
  graph: &EvidenceGraphState,
  policy: EvidencePolicy<'_>,
) {
  let counts = graph.semantic_counts(policy);
  layers.semantic_obligations_total = counts.total;
  layers.semantic_satisfied = counts.satisfied;
  layers.semantic_gaps = counts.gaps;
  layers.semantic_uncertain = counts.uncertain;
  layers.contradictions = counts.gaps;
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
    IntegrationOutcome::ProjectVerificationFailed { report } => {
      project_verification_failure_reason(report)
    }
    IntegrationOutcome::Accepted { .. } => "Candidate accepted".into(),
  }
}

fn project_verification_failure_reason(report: &ProjectVerificationRun) -> String {
  let Some(check) = report
    .checks
    .iter()
    .find(|check| check.result.exit_code != Some(0) || check.result.timed_out)
  else {
    return "Candidate failed mandatory project verification before all checks completed".into();
  };
  let status = if check.result.timed_out {
    "timed out".into()
  } else if let Some(exit_code) = check.result.exit_code {
    format!("exited with code {exit_code}")
  } else {
    "terminated without an exit code".into()
  };
  format!(
    "Candidate failed mandatory project verification: '{}' {status}",
    check.name
  )
}

pub async fn manual_verify(cwd: &Path) -> Result<ProjectVerificationRun> {
  let config = ensure_config(cwd).await?;
  let revision = git::head(cwd).await?;
  let run_id = format!("manual-{}", Uuid::new_v4());
  let workspaces = WorkspaceManager::new(cwd.to_path_buf(), run_id.clone());
  let report = verifier::run_project_verification_isolated(
    cwd,
    &workspaces,
    &revision,
    &config,
    "manual-project-verification",
    &CancellationToken::new(),
  )
  .await?;
  store::record_manual_verification(cwd, &run_id, &report).await?;
  Ok(report)
}

#[cfg(test)]
mod tests {
  use super::*;
  use tenet_domain::{
    evidence::{AcceptanceCriterion, VerificationObligation},
    ids::{ArchitectSourceRef, CriterionId, ObligationId, RequirementId, VerificationRunId},
    model::{ArchitectOutput, ArchitectRequirement, Requirement, RequirementAssessment},
    verification::{CommandResult, ProjectCheckResult, VerificationSpec},
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
        description: "Required behavior is observable".into(),
        required: true,
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

  fn validate_reconcile(_catalog: &RequirementCatalog, result: &ReconcileResult) -> Result<()> {
    decision::validate_reconciliation(result)
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
  fn present_implementation_with_gaps_is_rejected() {
    let error = validate_reconcile(
      &catalog(),
      &result(ImplementationState::Present, vec!["missing behavior"], true),
    )
    .expect_err("present implementation with gaps rejected");

    assert!(error
      .to_string()
      .contains("present implementation REQ-001 cannot report implementation gaps"));
  }

  #[test]
  fn partial_implementation_with_a_concrete_gap_is_valid() {
    assert!(validate_reconcile(
      &catalog(),
      &result(ImplementationState::Partial, vec!["missing behavior"], true,)
    )
    .is_ok());
  }

  #[test]
  fn partial_implementation_without_a_gap_is_rejected() {
    assert!(validate_reconcile(
      &catalog(),
      &result(ImplementationState::Partial, Vec::new(), true)
    )
    .is_err());
  }

  #[test]
  fn absent_implementation_without_a_gap_is_rejected() {
    assert!(validate_reconcile(
      &catalog(),
      &result(ImplementationState::Absent, Vec::new(), true)
    )
    .is_err());
  }

  #[test]
  fn unknown_implementation_without_an_explanatory_gap_is_rejected() {
    assert!(validate_reconcile(
      &catalog(),
      &result(ImplementationState::Unknown, Vec::new(), true)
    )
    .is_err());
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
      requirements: vec![ArchitectRequirement {
        id: requirement.id,
        title: requirement.title,
        description: requirement.description,
        required: requirement.required,
        source_refs: requirement
          .source_refs
          .into_iter()
          .enumerate()
          .map(|(index, _)| ArchitectSourceRef::from(format!("B0001-F{:02}", index + 1)))
          .collect(),
      }],
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
    let built = catalog::build("Description", "spec".into(), output).expect("build catalog");

    assert!(built.requirements[0].required);
    assert!(built.acceptance_criteria[0].mandatory);
    assert!(built.verification_obligations[0].required);
  }

  #[test]
  fn architect_source_token_is_expanded_from_controller_metadata() {
    let specification = "Description";
    let fragment = derive_normative_fragments(specification)
      .into_iter()
      .next()
      .expect("normative fragment");
    let requirement = catalog().requirements.remove(0);

    let built = catalog::build(specification, "spec".into(), architect_output(requirement))
      .expect("build catalog");

    assert_eq!(
      built.requirements[0].source_refs,
      vec![fragment.reference()]
    );
    assert!(catalog::validate_coverage(&built, specification).is_ok());
  }

  #[test]
  fn architect_unknown_source_token_is_rejected() {
    let requirement = catalog().requirements.remove(0);
    let mut output = architect_output(requirement);
    output.requirements[0].source_refs = vec![ArchitectSourceRef::from("B9999-F01")];

    let error = catalog::build("Description", "spec".into(), output)
      .expect_err("unknown source token must fail closed");

    assert!(error
      .to_string()
      .contains("references unknown architect source token B9999-F01"));
  }

  #[test]
  fn omitted_normative_fragment_blocks_catalog_coverage() {
    let specification = "First normative statement.\n\nSecond normative statement.";
    let fragments = derive_normative_fragments(specification);
    let mut requirement = catalog().requirements.remove(0);
    requirement.source_refs = vec![fragments[0].reference()];
    let built = catalog::build(specification, "spec".into(), architect_output(requirement))
      .expect("build catalog");

    assert!(catalog::validate(&built).is_ok());
    assert!(catalog::validate_coverage(&built, specification).is_err());
    assert_eq!(
      built.coverage.uncovered_fragment_ids,
      vec![fragments[1].id.clone()]
    );
  }

  #[test]
  fn project_verification_failure_names_failed_check_and_exit_code() {
    let spec = VerificationSpec {
      program: "npm".into(),
      args: vec!["run".into(), "build".into()],
      working_directory: ".".into(),
      environment: BTreeMap::new(),
    };
    let report = ProjectVerificationRun {
      run_id: VerificationRunId::new(),
      revision: "candidate".into(),
      suite_hash: "suite".into(),
      checks: vec![ProjectCheckResult {
        name: "build".into(),
        spec: spec.clone(),
        timeout_secs: 300,
        result: CommandResult {
          command: spec.identity(),
          exit_code: Some(127),
          timed_out: false,
          duration_ms: 1,
          stdout: String::new(),
          stderr: "tsc: command not found".into(),
        },
      }],
      passed: false,
      started_at: Utc::now(),
      finished_at: Utc::now(),
    };

    assert_eq!(
      project_verification_failure_reason(&report),
      "Candidate failed mandatory project verification: 'build' exited with code 127"
    );
  }

  #[test]
  fn completion_gate_projects_policy_blockers_without_redeciding_them() {
    let mut state = State::fresh();
    state.requirement_counts.total = 1;
    state.verification_layers.project_checks_total = 1;
    state.verification_layers.project_checks_passed = 1;
    let decision = CompletionDecision::NotReady(vec![
      CompletionBlocker::SemanticGap(ObligationId::from("REQ-001/AC-01/VO-01")),
      CompletionBlocker::RepositoryDirty,
    ]);

    let gate = completion_gate(&decision, &state, "abc1234".into());

    assert!(!gate.earned);
    assert_eq!(gate.blockers.len(), 2);
    assert!(gate.items.iter().any(|item| {
      item.label == "semantic obligations" && item.outcome == CompletionGateOutcome::Unsatisfied
    }));
    assert!(gate.items.iter().any(|item| {
      item.label == "repository clean" && item.outcome == CompletionGateOutcome::Unsatisfied
    }));
  }

  #[test]
  fn completion_gate_marks_every_condition_satisfied_only_for_done() {
    let gate = completion_gate(&CompletionDecision::Done, &State::fresh(), "abc1234".into());

    assert!(gate.earned);
    assert!(gate
      .items
      .iter()
      .all(|item| item.outcome == CompletionGateOutcome::Satisfied));
  }
}
