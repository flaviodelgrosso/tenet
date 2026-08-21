use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use tenet_domain::{
  config::Config,
  model::{
    CandidateCheck, IntegrationPhase, IntegrationTransaction, VerificationReport, WorkExecution,
  },
  verification::ProjectVerificationRun,
};
use tenet_storage::Storage;

use crate::{git, store, verifier, workspace::WorkspaceManager};

// Give concurrently observing cancellation a bounded window after PREPARED becomes durable.
const PREPARED_CANCELLATION_GRACE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationOutcome {
  Accepted {
    revision: String,
    verification: ProjectVerificationRun,
    transaction: Box<IntegrationTransaction>,
  },
  StaleBase,
  MergeConflict {
    paths: Vec<String>,
  },
  VerificationFailed {
    report: VerificationReport,
  },
  ProjectVerificationFailed {
    report: ProjectVerificationRun,
  },
}

pub struct Integrator {
  repository: PathBuf,
  workspace: PathBuf,
  accepted_revision: String,
  config: Config,
  workspaces: WorkspaceManager,
  cancel: CancellationToken,
}

impl Integrator {
  pub async fn create(
    repository: PathBuf,
    workspaces: &WorkspaceManager,
    revision: String,
    config: Config,
  ) -> Result<Self> {
    Self::create_with_cancel(
      repository,
      workspaces,
      revision,
      config,
      CancellationToken::new(),
    )
    .await
  }

  pub async fn create_with_cancel(
    repository: PathBuf,
    workspaces: &WorkspaceManager,
    revision: String,
    config: Config,
    cancel: CancellationToken,
  ) -> Result<Self> {
    let workspace = workspaces.create_integration(&revision).await?;
    Ok(Self {
      repository,
      workspace,
      accepted_revision: revision,
      workspaces: workspaces.clone(),
      config,
      cancel,
    })
  }

  pub fn accepted_revision(&self) -> &str {
    &self.accepted_revision
  }

  pub async fn integrate(&mut self, candidate: &WorkExecution) -> Result<IntegrationOutcome> {
    self.check_cancel()?;
    if !git::is_ancestor(
      &self.workspace,
      &candidate.base_revision,
      &self.accepted_revision,
    )
    .await?
    {
      return Ok(IntegrationOutcome::StaleBase);
    }

    git::reset_hard(&self.workspace, &self.accepted_revision).await?;
    if !git::cherry_pick(&self.workspace, &candidate.candidate_revision).await? {
      let paths = git::conflict_paths(&self.workspace).await?;
      git::abort_cherry_pick(&self.workspace).await?;
      git::reset_hard(&self.workspace, &self.accepted_revision).await?;
      return Ok(IntegrationOutcome::MergeConflict { paths });
    }

    let revision = git::head(&self.workspace).await?;
    if !candidate.lease.work_unit.suggested_checks.is_empty() {
      let report = self
        .verify_revision(&revision, &candidate.lease.work_unit.suggested_checks)
        .await?;
      if !report.passed {
        git::reset_hard(&self.workspace, &self.accepted_revision).await?;
        return Ok(IntegrationOutcome::VerificationFailed { report });
      }
    }

    let regression = verifier::run_project_verification_isolated(
      &self.repository,
      &self.workspaces,
      &revision,
      &self.config,
      "integration-project-verification",
      &self.cancel,
    )
    .await?;
    if !regression.passed {
      git::reset_hard(&self.workspace, &self.accepted_revision).await?;
      return Ok(IntegrationOutcome::ProjectVerificationFailed { report: regression });
    }

    let run_id = self.workspaces.run_id();
    Storage::open(&self.repository)
      .await?
      .record_project_verification(run_id, &regression)
      .await?;
    let transaction_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let mut transaction = IntegrationTransaction {
      version: IntegrationTransaction::VERSION,
      id: transaction_id,
      run_id: run_id.into(),
      work_unit: candidate.lease.work_unit.clone(),
      candidate_revision: candidate.candidate_revision.clone(),
      old_head: self.accepted_revision.clone(),
      new_head: revision.clone(),
      phase: IntegrationPhase::Prepared,
      verification_run_id: regression.run_id,
      verification_hash: store::project_verification_hash(&regression)?,
      created_at: now.clone(),
      updated_at: now,
    };
    store::write_integration_journal(&self.repository, &transaction).await?;
    tokio::select! {
      () = self.cancel.cancelled() => self.check_cancel()?,
      () = tokio::time::sleep(PREPARED_CANCELLATION_GRACE) => self.check_cancel()?,
    }
    if git::head(&self.repository).await? != self.accepted_revision {
      anyhow::bail!("canonical HEAD changed before integration transaction commit");
    }
    git::fast_forward(&self.repository, &revision)
      .await
      .context("advance canonical repository to accepted integration")?;
    transaction.phase = IntegrationPhase::GitCommitted;
    transaction.updated_at = Utc::now().to_rfc3339();
    store::write_integration_journal(&self.repository, &transaction).await?;
    self.accepted_revision.clone_from(&revision);
    Ok(IntegrationOutcome::Accepted {
      revision,
      verification: regression,
      transaction: Box::new(transaction),
    })
  }

  async fn verify_revision(
    &self,
    revision: &str,
    suggested_checks: &[CandidateCheck],
  ) -> Result<VerificationReport> {
    let canonical_before = git::repository_state(&self.repository).await?;
    let workspace = self
      .workspaces
      .create_disposable("integration-verification", revision)
      .await?;
    let result = verifier::run_suggested_checks_cancelled(
      &workspace,
      &self.config,
      suggested_checks.iter().map(|check| check.command.as_str()),
      &self.cancel,
    )
    .await;
    let cleanup = self.workspaces.remove(&workspace).await;
    if let Err(cleanup) = cleanup {
      return match result {
        Ok(_) => Err(cleanup).context("discard integration verification worktree"),
        Err(error) => Err(error).context(format!(
          "integration verification failed; cleanup also failed: {cleanup:#}"
        )),
      };
    }
    if git::repository_state(&self.repository).await? != canonical_before {
      anyhow::bail!("integration verification command modified canonical repository state");
    }
    result
  }

  fn check_cancel(&self) -> Result<()> {
    if self.cancel.is_cancelled() {
      anyhow::bail!("run cancelled before canonical integration advancement");
    }
    Ok(())
  }

  pub async fn cleanup(self, workspaces: &WorkspaceManager) -> Result<()> {
    workspaces.remove(&self.workspace).await
  }
}

pub fn deterministic_order(candidates: &mut [WorkExecution]) {
  candidates.sort_by(|left, right| left.lease.work_unit.id.cmp(&right.lease.work_unit.id));
}
