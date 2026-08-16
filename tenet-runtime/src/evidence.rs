use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::Utc;
use globset::{Glob, GlobSetBuilder};

use tenet_domain::{
  evidence::{
    EvidenceGraphState, EvidencePolicy, EvidenceProjection, EvidenceValidity, VerificationState,
  },
  ids::{EvidenceId, RequirementId, VerificationRunId},
  model::{RequirementCatalog, VerificationReport},
};

use crate::git;

pub fn graph_from_catalog(catalog: &RequirementCatalog) -> Result<EvidenceGraphState> {
  let mut graph = EvidenceGraphState::new(&catalog.spec_hash);
  for requirement in &catalog.requirements {
    graph.register_requirement(requirement.id.clone(), requirement.required);
  }
  for criterion in &catalog.acceptance_criteria {
    graph
      .add_criterion(criterion.clone())
      .context("register acceptance criterion")?;
  }
  for obligation in &catalog.verification_obligations {
    scope_matches(&obligation.dependency_scope, &[])
      .context("validate verification obligation dependency scope")?;
    graph
      .add_obligation(obligation.clone())
      .context("register verification obligation")?;
  }
  Ok(graph)
}

pub fn verification_states(
  graph: &EvidenceGraphState,
) -> Result<BTreeMap<RequirementId, VerificationState>> {
  graph
    .requirements
    .iter()
    .map(|requirement_id| {
      graph
        .requirement_verification_state(requirement_id, &EvidencePolicy)
        .map(|state| (requirement_id.clone(), state))
        .map_err(anyhow::Error::new)
    })
    .collect()
}

pub fn criterion_states(
  graph: &EvidenceGraphState,
) -> Result<BTreeMap<tenet_domain::ids::CriterionId, VerificationState>> {
  graph
    .criteria
    .keys()
    .map(|criterion_id| {
      graph
        .criterion_verification_state(criterion_id, &EvidencePolicy)
        .map(|state| (criterion_id.clone(), state))
        .map_err(anyhow::Error::new)
    })
    .collect()
}

pub fn projections(graph: &EvidenceGraphState) -> Result<Vec<EvidenceProjection>> {
  graph
    .requirements
    .iter()
    .map(|requirement_id| {
      graph
        .projection(requirement_id, &EvidencePolicy)
        .map_err(anyhow::Error::new)
    })
    .collect()
}

pub fn record_report(
  graph: &mut EvidenceGraphState,
  revision: &str,
  report: &VerificationReport,
) -> Result<Vec<EvidenceId>> {
  record_report_with_bindings(
    graph,
    revision,
    report,
    std::iter::empty::<(&tenet_domain::ids::ObligationId, &str)>(),
  )
}

pub fn record_report_with_bindings<'a>(
  graph: &mut EvidenceGraphState,
  revision: &str,
  report: &VerificationReport,
  bindings: impl IntoIterator<Item = (&'a tenet_domain::ids::ObligationId, &'a str)>,
) -> Result<Vec<EvidenceId>> {
  let bindings: Vec<_> = bindings.into_iter().collect();
  let run_id = VerificationRunId::new();
  let mut established = Vec::new();
  for command in &report.commands {
    let mut obligation_ids: BTreeSet<_> = graph
      .obligations
      .values()
      .filter(|obligation| obligation.command.trim() == command.command.trim())
      .map(|obligation| obligation.id.clone())
      .collect();
    obligation_ids.extend(
      bindings
        .iter()
        .filter(|(_, bound_command)| bound_command.trim() == command.command.trim())
        .map(|(obligation_id, _)| (*obligation_id).clone()),
    );
    for obligation_id in obligation_ids {
      established.push(
        graph
          .record_command_result(
            &obligation_id,
            revision,
            run_id,
            report.finished_at,
            command,
          )
          .context("record verification command evidence")?,
      );
    }
  }
  Ok(established)
}

pub async fn invalidate_for_revision(
  cwd: &std::path::Path,
  graph: &mut EvidenceGraphState,
  revision: &str,
) -> Result<Vec<EvidenceId>> {
  let mut changed_by_revision = BTreeMap::new();
  let mut affected = BTreeSet::new();
  for evidence in graph.evidence.values() {
    if !matches!(evidence.validity, EvidenceValidity::Valid) || evidence.revision == revision {
      continue;
    }
    let changed_paths = match changed_by_revision.get(&evidence.revision) {
      Some(paths) => paths,
      None => {
        let paths = git::changed_paths(cwd, &evidence.revision, revision).await?;
        changed_by_revision.insert(evidence.revision.clone(), paths);
        &changed_by_revision[&evidence.revision]
      }
    };
    if scope_matches(&evidence.dependency_scope, changed_paths)? {
      affected.insert(evidence.id);
    }
  }
  Ok(graph.invalidate_where(revision, Utc::now(), |evidence| {
    affected.contains(&evidence.id)
  }))
}

fn scope_matches(scope: &[String], changed_paths: &[String]) -> Result<bool> {
  let mut builder = GlobSetBuilder::new();
  for pattern in scope {
    builder.add(Glob::new(pattern).with_context(|| format!("invalid evidence scope {pattern}"))?);
  }
  let matcher = builder
    .build()
    .context("compile evidence dependency scope")?;
  Ok(changed_paths.iter().any(|path| matcher.is_match(path)))
}

#[cfg(test)]
mod tests {
  use chrono::TimeZone;
  use tenet_domain::{
    evidence::{AcceptanceCriterion, VerificationKind, VerificationObligation},
    ids::{CriterionId, ObligationId},
    model::{CommandResult, Requirement},
  };

  use super::*;

  fn catalog() -> RequirementCatalog {
    RequirementCatalog {
      spec_hash: "spec".into(),
      requirements: vec![Requirement {
        id: RequirementId::from("REQ-001"),
        title: "Auth".into(),
        description: "Authenticate requests".into(),
        required: true,
      }],
      acceptance_criteria: vec![AcceptanceCriterion {
        id: CriterionId::from("REQ-001/AC-01"),
        requirement_id: RequirementId::from("REQ-001"),
        description: "Expired tokens fail".into(),
        mandatory: true,
      }],
      verification_obligations: vec![VerificationObligation {
        id: ObligationId::from("REQ-001/AC-01/VO-01"),
        criterion_id: CriterionId::from("REQ-001/AC-01"),
        description: "Run the expired token test".into(),
        kind: VerificationKind::AutomatedTest,
        required: true,
        command: "cargo test expired_token".into(),
        dependency_scope: vec!["src/auth.rs".into()],
      }],
    }
  }

  #[test]
  fn report_commands_create_individual_revision_bound_evidence() {
    let mut graph = graph_from_catalog(&catalog()).expect("graph");
    let finished_at = Utc.with_ymd_and_hms(2026, 8, 16, 12, 0, 0).unwrap();
    let report = VerificationReport {
      passed: true,
      started_at: finished_at,
      finished_at,
      commands: vec![CommandResult {
        command: "cargo test expired_token".into(),
        exit_code: Some(0),
        timed_out: false,
        duration_ms: 1,
        stdout: "ok".into(),
        stderr: String::new(),
      }],
      warnings: Vec::new(),
    };

    let ids = record_report(&mut graph, "abc123", &report).expect("record report");
    assert_eq!(graph.evidence[&ids[0]].revision, "abc123");
  }

  #[test]
  fn explicit_candidate_check_binding_creates_obligation_evidence() {
    let mut graph = graph_from_catalog(&catalog()).expect("graph");
    let finished_at = Utc.with_ymd_and_hms(2026, 8, 16, 12, 0, 0).unwrap();
    let report = VerificationReport {
      passed: true,
      started_at: finished_at,
      finished_at,
      commands: vec![CommandResult {
        command: "custom candidate check".into(),
        exit_code: Some(0),
        timed_out: false,
        duration_ms: 1,
        stdout: "ok".into(),
        stderr: String::new(),
      }],
      warnings: Vec::new(),
    };
    let obligation_id = ObligationId::from("REQ-001/AC-01/VO-01");

    let ids = record_report_with_bindings(
      &mut graph,
      "abc123",
      &report,
      [(&obligation_id, "custom candidate check")],
    )
    .expect("record bound check");

    assert_eq!(graph.evidence[&ids[0]].obligation_id, obligation_id);
  }

  fn commit(cwd: &std::path::Path, message: &str) -> String {
    let status = std::process::Command::new("git")
      .args(["add", "-A"])
      .current_dir(cwd)
      .status()
      .expect("git add");
    assert!(status.success());
    let status = std::process::Command::new("git")
      .args(["commit", "-m", message])
      .current_dir(cwd)
      .status()
      .expect("git commit");
    assert!(status.success());
    std::process::Command::new("git")
      .args(["rev-parse", "HEAD"])
      .current_dir(cwd)
      .output()
      .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
      .expect("git head")
  }

  fn repository() -> tempfile::TempDir {
    let repository = tempfile::tempdir().expect("temporary repository");
    for args in [
      vec!["init"],
      vec!["config", "user.name", "Tenet Test"],
      vec!["config", "user.email", "tenet@example.invalid"],
    ] {
      let status = std::process::Command::new("git")
        .args(args)
        .current_dir(repository.path())
        .status()
        .expect("configure git");
      assert!(status.success());
    }
    std::fs::create_dir_all(repository.path().join("src")).expect("source directory");
    std::fs::write(repository.path().join("src/auth.rs"), "base").expect("auth source");
    std::fs::write(repository.path().join("README.md"), "base").expect("readme");
    commit(repository.path(), "base");
    repository
  }

  fn verified_graph(revision: &str) -> EvidenceGraphState {
    let mut graph = graph_from_catalog(&catalog()).expect("graph");
    let finished_at = Utc.with_ymd_and_hms(2026, 8, 16, 12, 0, 0).unwrap();
    record_report(
      &mut graph,
      revision,
      &VerificationReport {
        passed: true,
        started_at: finished_at,
        finished_at,
        commands: vec![CommandResult {
          command: "cargo test expired_token".into(),
          exit_code: Some(0),
          timed_out: false,
          duration_ms: 1,
          stdout: "ok".into(),
          stderr: String::new(),
        }],
        warnings: Vec::new(),
      },
    )
    .expect("record evidence");
    graph
  }

  #[tokio::test]
  async fn relevant_repository_change_stales_evidence() {
    let repository = repository();
    let base = git::head(repository.path()).await.expect("base revision");
    let mut graph = verified_graph(&base);
    std::fs::write(repository.path().join("src/auth.rs"), "changed").expect("change auth");
    let revision = commit(repository.path(), "change auth");

    let invalidated = invalidate_for_revision(repository.path(), &mut graph, &revision)
      .await
      .expect("invalidate");

    assert_eq!(invalidated.len(), 1);
    assert_eq!(
      graph.requirement_verification_state(&RequirementId::from("REQ-001"), &EvidencePolicy),
      Ok(VerificationState::Stale)
    );
  }

  #[tokio::test]
  async fn unrelated_repository_change_preserves_graph_and_evidence() {
    let repository = repository();
    let base = git::head(repository.path()).await.expect("base revision");
    let mut graph = verified_graph(&base);
    let structure = (
      graph.requirements.clone(),
      graph.criteria.clone(),
      graph.obligations.clone(),
    );
    std::fs::write(repository.path().join("README.md"), "changed").expect("change readme");
    let revision = commit(repository.path(), "change readme");

    let invalidated = invalidate_for_revision(repository.path(), &mut graph, &revision)
      .await
      .expect("invalidate");

    assert!(invalidated.is_empty());
    assert_eq!(
      structure,
      (
        graph.requirements.clone(),
        graph.criteria.clone(),
        graph.obligations.clone()
      )
    );
    assert_eq!(
      graph.requirement_verification_state(&RequirementId::from("REQ-001"), &EvidencePolicy),
      Ok(VerificationState::Verified)
    );
  }

  #[test]
  fn unrelated_paths_do_not_match_dependency_scope() {
    assert!(!scope_matches(&["src/auth.rs".into()], &["README.md".into()]).expect("scope"));
  }

  #[test]
  fn related_paths_match_dependency_scope() {
    assert!(scope_matches(&["src/**".into()], &["src/auth.rs".into()]).expect("scope"));
  }
}
