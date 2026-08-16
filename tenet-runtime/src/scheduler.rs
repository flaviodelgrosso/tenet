use std::{
  collections::VecDeque,
  path::{Path, PathBuf},
  sync::Arc,
};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use globset::{Glob, GlobSet, GlobSetBuilder};
use tokio::{
  sync::{mpsc, Semaphore},
  task::JoinSet,
};
use uuid::Uuid;

use tenet_domain::{
  events::RunEvent,
  model::{RequirementCatalog, WorkExecution, WorkLease, WorkUnit, WorkerDiscovery, WorkerRole},
};

use crate::{
  backend::{AgentBackend, BackendContext},
  git, protection, verifier,
  workspace::WorkspaceManager,
};

#[derive(Debug, Clone)]
pub enum ExecutionUpdate {
  Implementing { work_unit_id: String },
  Repairing { work_unit_id: String, attempt: u32 },
}

pub struct Scheduler {
  repository: PathBuf,
  backend: Arc<dyn AgentBackend>,
  context: BackendContext,
  catalog: Arc<RequirementCatalog>,
  workspaces: WorkspaceManager,
  updates: mpsc::UnboundedSender<ExecutionUpdate>,
}

impl Scheduler {
  pub fn new(
    repository: PathBuf,
    run_id: String,
    backend: Arc<dyn AgentBackend>,
    context: BackendContext,
    catalog: Arc<RequirementCatalog>,
    updates: mpsc::UnboundedSender<ExecutionUpdate>,
  ) -> Self {
    let workspaces = WorkspaceManager::new(repository.clone(), &run_id);
    Self {
      repository,
      backend,
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

  pub async fn execute_leases(&self, leases: Vec<WorkLease>) -> Result<Vec<WorkExecution>> {
    let mut pending = VecDeque::from(leases);
    let mut executions = Vec::new();

    while !pending.is_empty() {
      if self.context.cancel.is_cancelled() {
        bail!("run cancelled");
      }
      let batch = select_non_conflicting_batch(
        &mut pending,
        self.context.config.execution.max_parallel_workers,
      )?;
      let mut completed = self.execute_batch(batch).await?;
      executions.append(&mut completed);
    }

    executions.sort_by(|left, right| left.lease.work_unit.id.cmp(&right.lease.work_unit.id));
    Ok(executions)
  }

  pub async fn cleanup(&self) -> Result<()> {
    self.workspaces.cleanup_run().await
  }

  async fn execute_batch(&self, batch: Vec<WorkLease>) -> Result<Vec<WorkExecution>> {
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
      let backend = self.backend.clone();
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
          repository, workspaces, backend, context, catalog, lease, updates,
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
  backend: Arc<dyn AgentBackend>,
  mut context: BackendContext,
  catalog: Arc<RequirementCatalog>,
  mut lease: WorkLease,
  updates: mpsc::UnboundedSender<ExecutionUpdate>,
) -> Result<WorkExecution> {
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
    execute_and_commit(&repository, &backend, &context, &catalog, &lease, &updates).await;
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
  backend: &Arc<dyn AgentBackend>,
  context: &BackendContext,
  catalog: &RequirementCatalog,
  lease: &WorkLease,
  updates: &mpsc::UnboundedSender<ExecutionUpdate>,
) -> Result<WorkExecution> {
  let protected_paths = protected_paths(context);
  let protected = protection::snapshot(&lease.workspace, &protected_paths).await?;
  let _ = updates.send(ExecutionUpdate::Implementing {
    work_unit_id: lease.work_unit.id.clone(),
  });
  let worker_summary = backend
    .implement(context, catalog, &lease.work_unit)
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
  let mut verification = verify_candidate(
    repository,
    context,
    &candidate_revision,
    &lease.work_unit.suggested_checks,
  )
  .await?;

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
    let repair_summary = backend
      .repair(context, catalog, &lease.work_unit, &verification)
      .await
      .context("repair worker")?;
    discoveries.extend(
      repair_summary
        .discoveries
        .into_iter()
        .map(|discovery| WorkerDiscovery {
          discovery,
          role: WorkerRole::Repair,
        }),
    );
    reject_protected_changes(&lease.workspace, &protected, "repair worker").await?;
    candidate_revision = commit_candidate(&lease.workspace, lease).await?;
    changed_paths =
      git::changed_paths(repository, &lease.base_revision, &candidate_revision).await?;
    validate_changed_paths(&lease.work_unit, &changed_paths)?;
    verification = verify_candidate(
      repository,
      context,
      &candidate_revision,
      &lease.work_unit.suggested_checks,
    )
    .await?;
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
  Ok(execution)
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
  suggested_checks: &[String],
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
    suggested_checks,
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

fn protected_paths(context: &BackendContext) -> Vec<String> {
  let mut paths = context.config.protected_paths.clone();
  if !paths.iter().any(|path| path == &context.config.spec_file) {
    paths.push(context.config.spec_file.clone());
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
  use tenet_domain::model::WorkScope;

  fn unit(id: &str, paths: &[&str]) -> WorkUnit {
    WorkUnit {
      id: id.into(),
      title: id.into(),
      objective: id.into(),
      requirement_ids: vec!["REQ-001".into()],
      acceptance_criteria: vec!["done".into()],
      suggested_checks: Vec::new(),
      depends_on: Vec::new(),
      scope: WorkScope {
        paths: paths.iter().map(|path| (*path).into()).collect(),
      },
    }
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
