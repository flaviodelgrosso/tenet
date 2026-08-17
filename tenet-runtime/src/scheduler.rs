use std::{
  collections::VecDeque,
  path::{Path, PathBuf},
  sync::Arc,
};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use globset::{Glob, GlobSet, GlobSetBuilder};
use tokio::{
  sync::{mpsc, Semaphore},
  task::JoinSet,
};
use uuid::Uuid;

use tenet_domain::{
  config::Config,
  events::RunEvent,
  model::{
    RequirementCatalog, VerificationReport, WorkExecution, WorkLease, WorkUnit, WorkerDiscovery,
    WorkerRole, WorkerSummary,
  },
};

use crate::{backend::BackendContext, git, protection, verifier, workspace::WorkspaceManager};

#[derive(Debug, Clone)]
pub enum ExecutionUpdate {
  Implementing { work_unit_id: String },
  Repairing { work_unit_id: String, attempt: u32 },
}

#[derive(Debug)]
pub struct BlockedExecution {
  pub lease: WorkLease,
  pub discoveries: Vec<WorkerDiscovery>,
}

#[derive(Debug, Default)]
pub struct SchedulerOutcome {
  pub executions: Vec<WorkExecution>,
  pub blocked: Vec<BlockedExecution>,
}

enum LeaseOutcome {
  Verified(Box<WorkExecution>),
  Blocked(Box<BlockedExecution>),
}

/// Supplies candidate mutations while the scheduler owns execution mechanics.
#[async_trait]
pub trait CandidateExecutor: Send + Sync {
  async fn execute(
    &self,
    context: &BackendContext,
    catalog: &RequirementCatalog,
    work_unit: &WorkUnit,
    previous_verification: Option<&VerificationReport>,
  ) -> Result<WorkerSummary>;
}

pub struct Scheduler {
  repository: PathBuf,
  executor: Arc<dyn CandidateExecutor>,
  context: BackendContext,
  catalog: Arc<RequirementCatalog>,
  workspaces: WorkspaceManager,
  updates: mpsc::UnboundedSender<ExecutionUpdate>,
}

impl Scheduler {
  pub fn new(
    repository: PathBuf,
    run_id: String,
    executor: Arc<dyn CandidateExecutor>,
    context: BackendContext,
    catalog: Arc<RequirementCatalog>,
    updates: mpsc::UnboundedSender<ExecutionUpdate>,
  ) -> Self {
    let workspaces = WorkspaceManager::new(repository.clone(), &run_id);
    Self {
      repository,
      executor,
      context,
      catalog,
      workspaces,
      updates,
    }
  }

  pub fn issue_leases(&self, mut frontier: Vec<WorkUnit>, base_revision: &str) -> Vec<WorkLease> {
    frontier.sort_by(|left, right| left.id.cmp(&right.id));
    frontier
      .into_iter()
      .map(|unit| self.issue_lease(unit, base_revision))
      .collect()
  }

  pub async fn execute_leases(&self, leases: Vec<WorkLease>) -> Result<SchedulerOutcome> {
    let mut pending = VecDeque::from(leases);
    let mut outcome = SchedulerOutcome::default();

    while !pending.is_empty() {
      if self.context.cancel.is_cancelled() {
        bail!("run cancelled");
      }
      let batch = select_non_conflicting_batch(
        &mut pending,
        self.context.config.execution.max_parallel_workers,
      )?;
      for completed in self.execute_batch(batch).await? {
        match completed {
          LeaseOutcome::Verified(execution) => outcome.executions.push(*execution),
          LeaseOutcome::Blocked(blocked) => outcome.blocked.push(*blocked),
        }
      }
    }

    outcome
      .executions
      .sort_by(|left, right| left.lease.work_unit.id.cmp(&right.lease.work_unit.id));
    outcome
      .blocked
      .sort_by(|left, right| left.lease.work_unit.id.cmp(&right.lease.work_unit.id));
    Ok(outcome)
  }

  pub async fn cleanup(&self) -> Result<()> {
    self.workspaces.cleanup_run().await
  }

  async fn execute_batch(&self, batch: Vec<WorkLease>) -> Result<Vec<LeaseOutcome>> {
    let semaphore = Arc::new(Semaphore::new(
      self.context.config.execution.max_parallel_workers,
    ));
    let batch_cancel = self.context.cancel.child_token();
    let mut tasks = JoinSet::new();

    for lease in batch {
      self
        .context
        .events
        .emit(RunEvent::LeaseIssued(lease.clone()))
        .await?;
      let permit_pool = semaphore.clone();
      let executor = self.executor.clone();
      let mut context = self.context.clone();
      context.cancel = batch_cancel.clone();
      let catalog = self.catalog.clone();
      let repository = self.repository.clone();
      let workspaces = self.workspaces.clone();
      let updates = self.updates.clone();
      tasks.spawn(async move {
        let _permit = permit_pool
          .acquire_owned()
          .await
          .map_err(|_| anyhow!("worker semaphore closed"))?;
        execute_lease(
          repository, workspaces, executor, context, catalog, lease, updates,
        )
        .await
      });
    }

    let mut outputs = Vec::new();
    let mut first_error = None;
    while let Some(joined) = tasks.join_next().await {
      match joined {
        Ok(Ok(execution)) => outputs.push(execution),
        Ok(Err(error)) if first_error.is_none() => {
          batch_cancel.cancel();
          first_error = Some(error);
        }
        Err(error) if first_error.is_none() => {
          batch_cancel.cancel();
          first_error = Some(anyhow!("worker task failed: {error}"));
        }
        _ => {}
      }
    }
    if let Some(error) = first_error {
      return Err(error);
    }
    Ok(outputs)
  }

  fn issue_lease(&self, work_unit: WorkUnit, base_revision: &str) -> WorkLease {
    let safe_id = work_unit.id.replace(['/', '\\'], "-");
    let id = format!("{safe_id}-{}", &Uuid::new_v4().to_string()[..8]);
    WorkLease {
      workspace: self.workspaces.worker_path(&id),
      worker_id: format!("worker-{id}"),
      id,
      work_unit,
      base_revision: base_revision.to_owned(),
      issued_at: Utc::now().to_rfc3339(),
    }
  }
}

async fn execute_lease(
  repository: PathBuf,
  workspaces: WorkspaceManager,
  executor: Arc<dyn CandidateExecutor>,
  mut context: BackendContext,
  catalog: Arc<RequirementCatalog>,
  mut lease: WorkLease,
  updates: mpsc::UnboundedSender<ExecutionUpdate>,
) -> Result<LeaseOutcome> {
  let workspace = workspaces
    .create_worker(&lease.id, &lease.base_revision)
    .await?;
  lease.workspace.clone_from(&workspace);
  context.cwd.clone_from(&workspace);
  context.runtime_dir = context.runtime_dir.join(&lease.id);
  context.worker_id.clone_from(&lease.worker_id);
  context.lease_id = Some(lease.id.clone());
  context.work_unit_id = Some(lease.work_unit.id.clone());
  context
    .events
    .emit(RunEvent::WorkspaceCreated {
      lease_id: lease.id.clone(),
      path: workspace.clone(),
    })
    .await?;
  context
    .events
    .emit(RunEvent::WorkerStarted {
      worker_id: lease.worker_id.clone(),
      lease_id: lease.id.clone(),
      work_unit_id: lease.work_unit.id.clone(),
    })
    .await?;

  let result =
    execute_and_commit(&repository, &executor, &context, &catalog, &lease, &updates).await;
  let cleanup = workspaces.remove(&workspace).await;
  context
    .events
    .emit(RunEvent::WorkspaceRemoved {
      lease_id: lease.id.clone(),
      path: workspace,
    })
    .await?;
  match (result, cleanup) {
    (Ok(execution), Ok(())) => Ok(execution),
    (Err(error), Ok(())) => Err(error),
    (Ok(_), Err(cleanup)) => Err(cleanup.context("worker workspace cleanup")),
    (Err(error), Err(cleanup)) => {
      Err(error.context(format!("worker workspace cleanup also failed: {cleanup:#}")))
    }
  }
}

async fn execute_and_commit(
  repository: &Path,
  executor: &Arc<dyn CandidateExecutor>,
  context: &BackendContext,
  catalog: &RequirementCatalog,
  lease: &WorkLease,
  updates: &mpsc::UnboundedSender<ExecutionUpdate>,
) -> Result<LeaseOutcome> {
  let protected_paths = protected_paths(&context.config);
  let protected = protection::snapshot(&lease.workspace, &protected_paths).await?;
  let _ = updates.send(ExecutionUpdate::Implementing {
    work_unit_id: lease.work_unit.id.clone(),
  });
  let worker_summary = executor
    .execute(context, catalog, &lease.work_unit, None)
    .await
    .context("implementation worker")?;
  let mut discoveries: Vec<_> = worker_summary
    .discoveries
    .iter()
    .cloned()
    .map(|discovery| WorkerDiscovery {
      discovery,
      role: WorkerRole::Implement,
    })
    .collect();
  reject_protected_changes(&lease.workspace, &protected, "worker").await?;

  let mut candidate_revision = commit_candidate(&lease.workspace, lease).await?;
  let mut changed_paths =
    git::changed_paths(repository, &lease.base_revision, &candidate_revision).await?;
  validate_changed_paths(&lease.work_unit, &changed_paths)?;
  let mut verification =
    verify_candidate(repository, context, &candidate_revision, &lease.work_unit).await?;

  for attempt in 1..=context.config.max_repair_attempts {
    if verification.passed {
      break;
    }
    if context.cancel.is_cancelled() {
      bail!("run cancelled");
    }
    git::reset_soft(&lease.workspace, &lease.base_revision).await?;
    let _ = updates.send(ExecutionUpdate::Repairing {
      work_unit_id: lease.work_unit.id.clone(),
      attempt,
    });
    let repair_summary = executor
      .execute(context, catalog, &lease.work_unit, Some(&verification))
      .await
      .context("repair worker")?;
    let repair_discoveries: Vec<_> = repair_summary
      .discoveries
      .into_iter()
      .map(|discovery| WorkerDiscovery {
        discovery,
        role: WorkerRole::Repair,
      })
      .collect();
    let verification_blocked = repair_discoveries.iter().any(|discovered| {
      matches!(
        discovered.discovery,
        tenet_domain::model::Discovery::VerificationBlocker { .. }
      )
    });
    discoveries.extend(repair_discoveries);
    reject_protected_changes(&lease.workspace, &protected, "repair worker").await?;
    if verification_blocked {
      context
        .events
        .emit(RunEvent::Verification(verification.clone()))
        .await?;
      return Ok(LeaseOutcome::Blocked(Box::new(BlockedExecution {
        lease: lease.clone(),
        discoveries,
      })));
    }
    candidate_revision = commit_candidate(&lease.workspace, lease).await?;
    changed_paths =
      git::changed_paths(repository, &lease.base_revision, &candidate_revision).await?;
    validate_changed_paths(&lease.work_unit, &changed_paths)?;
    verification =
      verify_candidate(repository, context, &candidate_revision, &lease.work_unit).await?;
  }
  context
    .events
    .emit(RunEvent::Verification(verification.clone()))
    .await?;
  if !verification.passed {
    bail!("verification failed for {}", lease.work_unit.id);
  }

  let execution = WorkExecution {
    lease: lease.clone(),
    worker_summary,
    verification,
    base_revision: lease.base_revision.clone(),
    candidate_revision,
    changed_paths,
    discoveries,
  };
  for discovered in &execution.discoveries {
    context
      .events
      .emit(RunEvent::DependencyDiscovered {
        lease_id: lease.id.clone(),
        discovery: discovered.discovery.clone(),
      })
      .await?;
  }
  context
    .events
    .emit(RunEvent::CandidateProduced(execution.clone()))
    .await?;
  Ok(LeaseOutcome::Verified(Box::new(execution)))
}

async fn reject_protected_changes(
  workspace: &Path,
  protected: &protection::Snapshot,
  role: &str,
) -> Result<()> {
  let violated = protection::restore_changes(workspace, protected).await?;
  if !violated.is_empty() {
    bail!(
      "{role} modified controller-protected files: {}",
      violated.join(", ")
    );
  }
  Ok(())
}

async fn commit_candidate(workspace: &Path, lease: &WorkLease) -> Result<String> {
  git::commit_all(
    workspace,
    &format!(
      "tenet: candidate {} (lease {})",
      lease.work_unit.id, lease.id
    ),
  )
  .await
}

async fn verify_candidate(
  repository: &Path,
  context: &BackendContext,
  revision: &str,
  work_unit: &WorkUnit,
) -> Result<tenet_domain::model::VerificationReport> {
  let canonical_before = git::repository_state(repository).await?;
  let run_id = context
    .runtime_dir
    .parent()
    .and_then(Path::file_name)
    .and_then(|value| value.to_str())
    .context("worker run id is unavailable")?;
  let workspaces = WorkspaceManager::new(repository.to_path_buf(), run_id);
  let workspace = workspaces
    .create_disposable("verification", revision)
    .await?;
  let result = verifier::run_verification_with_checks_cancelled(
    &workspace,
    &context.config,
    work_unit.suggested_commands(),
    &context.cancel,
  )
  .await;
  let cleanup = workspaces.remove(&workspace).await;
  if let Err(cleanup) = cleanup {
    return match result {
      Ok(_) => Err(cleanup).context("discard candidate verification worktree"),
      Err(error) => Err(error).context(format!(
        "candidate verification failed; cleanup also failed: {cleanup:#}"
      )),
    };
  }
  let canonical_after = git::repository_state(repository).await?;
  if canonical_after != canonical_before {
    bail!("verification command modified canonical repository state");
  }
  result
}

fn validate_changed_paths(unit: &WorkUnit, changed_paths: &[String]) -> Result<()> {
  let allowed = compile_scope(&unit.scope.paths)?;
  let unauthorized: Vec<_> = changed_paths
    .iter()
    .filter(|path| !allowed.is_match(path))
    .cloned()
    .collect();
  if !unauthorized.is_empty() {
    bail!(
      "candidate {} changed paths outside its declared scope: {}",
      unit.id,
      unauthorized.join(", ")
    );
  }
  Ok(())
}

const MANDATORY_PROTECTED_PATHS: [&str; 3] = ["AGENTS.md", "tenet.toml", ".tenet"];

fn protected_paths(config: &Config) -> Vec<String> {
  let mut paths: Vec<_> = MANDATORY_PROTECTED_PATHS
    .into_iter()
    .map(str::to_owned)
    .collect();
  paths.push(config.spec_file.clone());
  for additional in &config.additional_protected_paths {
    if !paths.iter().any(|path| path == additional) {
      paths.push(additional.clone());
    }
  }
  paths
}

fn select_non_conflicting_batch(
  pending: &mut VecDeque<WorkLease>,
  limit: usize,
) -> Result<Vec<WorkLease>> {
  let mut selected = Vec::new();
  let count = pending.len();
  for _ in 0..count {
    let Some(candidate) = pending.pop_front() else {
      break;
    };
    let conflict = selected
      .iter()
      .try_fold(false, |conflict, lease: &WorkLease| {
        Ok::<_, anyhow::Error>(conflict || scopes_conflict(&lease.work_unit, &candidate.work_unit)?)
      })?;
    if selected.len() < limit && !conflict {
      selected.push(candidate);
    } else {
      pending.push_back(candidate);
    }
  }
  if selected.is_empty() {
    if let Some(candidate) = pending.pop_front() {
      selected.push(candidate);
    }
  }
  Ok(selected)
}

pub fn scopes_conflict(left: &WorkUnit, right: &WorkUnit) -> Result<bool> {
  let left_set = compile_scope(&left.scope.paths)?;
  let right_set = compile_scope(&right.scope.paths)?;
  let left_samples = scope_samples(&left.scope.paths);
  let right_samples = scope_samples(&right.scope.paths);
  Ok(
    left_samples.iter().any(|path| right_set.is_match(path))
      || right_samples.iter().any(|path| left_set.is_match(path)),
  )
}

fn compile_scope(patterns: &[String]) -> Result<GlobSet> {
  let mut builder = GlobSetBuilder::new();
  for pattern in patterns {
    builder.add(Glob::new(pattern).with_context(|| format!("invalid scope glob {pattern}"))?);
  }
  builder.build().context("build scope matcher")
}

fn scope_samples(patterns: &[String]) -> Vec<String> {
  patterns
    .iter()
    .flat_map(|pattern| {
      let prefix = pattern
        .split(['*', '?', '[', '{'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('/');
      [
        pattern.clone(),
        prefix.to_owned(),
        format!("{prefix}/__tenet_scope_probe__"),
      ]
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;
  use tenet_domain::{
    ids::{CriterionId, ObligationId, RequirementId},
    model::WorkScope,
  };

  fn unit(id: &str, paths: &[&str]) -> WorkUnit {
    WorkUnit {
      id: id.into(),
      title: id.into(),
      objective: id.into(),
      requirement_ids: vec![RequirementId::from("REQ-001")],
      criterion_ids: vec![CriterionId::from("REQ-001/AC-01")],
      verification_obligation_ids: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
      suggested_checks: Vec::new(),
      depends_on: Vec::new(),
      scope: WorkScope {
        paths: paths.iter().map(|path| (*path).into()).collect(),
      },
    }
  }

  #[test]
  fn protected_paths_always_include_controller_policy_and_user_additions() {
    let config = Config {
      spec_file: "requirements/spec.md".into(),
      additional_protected_paths: vec!["secrets".into(), "tenet.toml".into()],
      ..Config::default()
    };

    assert_eq!(
      protected_paths(&config),
      [
        "AGENTS.md",
        "tenet.toml",
        ".tenet",
        "requirements/spec.md",
        "secrets",
      ]
    );
  }

  #[test]
  fn scopes_conflict_for_overlapping_globs() {
    let result = scopes_conflict(
      &unit("A", &["src/auth/**"]),
      &unit("B", &["src/auth/login.rs"]),
    )
    .expect("valid globs");

    assert!(result);
  }

  #[test]
  fn scopes_do_not_conflict_for_separate_trees() {
    let result = scopes_conflict(
      &unit("A", &["src/auth/**"]),
      &unit("B", &["src/payments/**"]),
    )
    .expect("valid globs");

    assert!(!result);
  }
}
