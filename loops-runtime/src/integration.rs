use std::path::PathBuf;

use anyhow::{Context, Result};

use loops_domain::{
  config::Config,
  model::{VerificationReport, WorkExecution},
};

use crate::{git, verifier, workspace::WorkspaceManager};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationOutcome {
  Accepted {
    revision: String,
    verification: VerificationReport,
  },
  StaleBase,
  MergeConflict {
    paths: Vec<String>,
  },
  VerificationFailed {
    report: VerificationReport,
  },
  RegressionDetected {
    report: VerificationReport,
  },
}

pub struct Integrator {
  repository: PathBuf,
  workspace: PathBuf,
  accepted_revision: String,
  config: Config,
}

impl Integrator {
  pub async fn create(
    repository: PathBuf,
    workspaces: &WorkspaceManager,
    revision: String,
    config: Config,
  ) -> Result<Self> {
    let workspace = workspaces.create_integration(&revision).await?;
    Ok(Self {
      repository,
      workspace,
      accepted_revision: revision,
      config,
    })
  }

  pub fn accepted_revision(&self) -> &str {
    &self.accepted_revision
  }

  pub async fn integrate(&mut self, candidate: &WorkExecution) -> Result<IntegrationOutcome> {
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

    if self.config.integration.verify_each_candidate
      && !candidate.lease.work_unit.suggested_checks.is_empty()
    {
      let report = verifier::run_suggested_checks(
        &self.workspace,
        &self.config,
        &candidate.lease.work_unit.suggested_checks,
      )
      .await?;
      if !report.passed {
        git::reset_hard(&self.workspace, &self.accepted_revision).await?;
        return Ok(IntegrationOutcome::VerificationFailed { report });
      }
    }

    let regression = verifier::run_verification(&self.workspace, &self.config).await?;
    if !regression.passed {
      git::reset_hard(&self.workspace, &self.accepted_revision).await?;
      return Ok(IntegrationOutcome::RegressionDetected { report: regression });
    }

    let revision = git::head(&self.workspace).await?;
    git::fast_forward(&self.repository, &revision)
      .await
      .context("advance canonical repository to accepted integration")?;
    self.accepted_revision.clone_from(&revision);
    Ok(IntegrationOutcome::Accepted {
      revision,
      verification: regression,
    })
  }

  pub async fn cleanup(self, workspaces: &WorkspaceManager) -> Result<()> {
    workspaces.remove(&self.workspace).await
  }
}

pub fn deterministic_order(candidates: &mut [WorkExecution]) {
  candidates.sort_by(|left, right| left.lease.work_unit.id.cmp(&right.lease.work_unit.id));
}
