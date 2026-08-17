use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::Utc;
use globset::{Glob, GlobSetBuilder};

use tenet_domain::{
  events::{EventSink, RunEvent},
  evidence::{
    EvidenceGraphState, EvidencePolicy, EvidenceProjection, EvidenceResult, EvidenceValidity,
    VerificationState,
  },
  ids::{CriterionId, EvidenceId, RequirementId},
  model::{RequirementCatalog, VerificationReport},
};

use tenet_runtime::{git, store};

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

pub async fn load(
  cwd: &std::path::Path,
  catalog: &RequirementCatalog,
) -> Result<EvidenceGraphState> {
  let expected = graph_from_catalog(catalog)?;
  store::read_evidence_graph(cwd, &expected).await
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
  report
    .executions
    .iter()
    .map(|execution| {
      graph
        .record_execution_result(revision, report.finished_at, execution)
        .context("record obligation-bound verification evidence")
    })
    .collect()
}

pub async fn establish(
  cwd: &std::path::Path,
  events: &EventSink,
  graph: &mut EvidenceGraphState,
  revision: &str,
  report: &VerificationReport,
) -> Result<()> {
  let requirement_before = verification_states(graph)?;
  let criterion_before = criterion_states(graph)?;
  let ids = record_report(graph, revision, report)?;
  if ids.is_empty() {
    return Ok(());
  }
  store::write_evidence_graph(cwd, graph).await?;
  for id in ids {
    let evidence = graph.evidence[&id].clone();
    let event = if evidence.result == EvidenceResult::Failed {
      RunEvent::EvidenceFailed(evidence.clone())
    } else {
      RunEvent::EvidenceEstablished(evidence.clone())
    };
    events.emit(event).await?;
    let related: Vec<_> = graph
      .evidence
      .values()
      .filter(|item| item.obligation_id == evidence.obligation_id)
      .collect();
    if related.iter().any(|item| EvidencePolicy.authorizes(item))
      && related.iter().any(|item| EvidencePolicy.blocks(item))
    {
      events
        .emit(RunEvent::EvidenceContradiction {
          obligation_id: evidence.obligation_id,
          evidence_ids: related.iter().map(|item| item.id).collect(),
        })
        .await?;
    }
  }
  emit_transitions(events, graph, requirement_before, criterion_before).await
}

pub async fn invalidate(
  cwd: &std::path::Path,
  events: &EventSink,
  graph: &mut EvidenceGraphState,
  revision: &str,
) -> Result<()> {
  let requirement_before = verification_states(graph)?;
  let criterion_before = criterion_states(graph)?;
  let invalidated = invalidate_for_revision(cwd, graph, revision).await?;
  if invalidated.is_empty() {
    return Ok(());
  }
  store::write_evidence_graph(cwd, graph).await?;
  for evidence_id in invalidated {
    events
      .emit(RunEvent::EvidenceInvalidated {
        evidence_id,
        revision: revision.to_owned(),
      })
      .await?;
  }
  emit_transitions(events, graph, requirement_before, criterion_before).await
}

async fn emit_transitions(
  events: &EventSink,
  graph: &EvidenceGraphState,
  requirement_before: BTreeMap<RequirementId, VerificationState>,
  criterion_before: BTreeMap<CriterionId, VerificationState>,
) -> Result<()> {
  for (criterion_id, current) in criterion_states(graph)? {
    let previous = criterion_before
      .get(&criterion_id)
      .copied()
      .unwrap_or(VerificationState::Unverified);
    if previous != current {
      events
        .emit(RunEvent::CriterionVerificationChanged {
          criterion_id,
          previous,
          current,
        })
        .await?;
    }
  }
  for (requirement_id, current) in verification_states(graph)? {
    let previous = requirement_before
      .get(&requirement_id)
      .copied()
      .unwrap_or(VerificationState::Unverified);
    if previous != current {
      events
        .emit(RunEvent::RequirementVerificationChanged {
          requirement_id,
          previous,
          current,
        })
        .await?;
    }
  }
  Ok(())
}
pub async fn invalidate_for_revision(
  cwd: &std::path::Path,
  graph: &mut EvidenceGraphState,
  revision: &str,
) -> Result<Vec<EvidenceId>> {
  let mut changed_by_revision = BTreeMap::new();
  let mut affected = std::collections::BTreeSet::new();
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
    if changed_paths.is_empty() {
      continue;
    }
    let unsafe_scope =
      !evidence.dependency_scope_authority.is_trusted() || evidence.dependency_scope.is_empty();
    let scope_affected =
      unsafe_scope || scope_matches(&evidence.dependency_scope, changed_paths).unwrap_or(true);
    if scope_affected {
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
    ids::{CriterionId, ObligationId, VerificationRunId},
    model::{CommandResult, Requirement},
    verification::{
      DependencyScopeAuthority, VerificationAuthority, VerificationExecutionResult,
      VerificationSpec,
    },
    worker::CatalogCoverage,
  };

  use super::*;

  fn spec() -> VerificationSpec {
    VerificationSpec {
      program: "true".into(),
      args: Vec::new(),
      working_directory: ".".into(),
      environment: Default::default(),
    }
  }

  fn catalog(scope_authority: DependencyScopeAuthority) -> RequirementCatalog {
    RequirementCatalog {
      spec_hash: "spec".into(),
      requirements: vec![Requirement {
        id: RequirementId::from("REQ-001"),
        title: "Auth".into(),
        description: "Authenticate requests".into(),
        required: true,
        source_refs: Vec::new(),
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
        spec: spec(),
        authority: VerificationAuthority::ProjectConfigured,
        dependency_scope: vec!["src/auth.rs".into()],
        dependency_scope_authority: scope_authority,
      }],
      coverage: CatalogCoverage {
        normative_fragments: Vec::new(),
        uncovered_fragment_ids: Vec::new(),
      },
    }
  }

  fn execution(obligation_id: &str) -> VerificationExecutionResult {
    VerificationExecutionResult {
      run_id: VerificationRunId::new(),
      obligation_id: ObligationId::from(obligation_id),
      spec: spec(),
      authority: VerificationAuthority::ProjectConfigured,
      result: CommandResult {
        command: spec().identity(),
        exit_code: Some(0),
        timed_out: false,
        duration_ms: 1,
        stdout: "ok".into(),
        stderr: String::new(),
      },
    }
  }

  fn verified_graph(
    revision: &str,
    scope_authority: DependencyScopeAuthority,
  ) -> EvidenceGraphState {
    let mut graph = graph_from_catalog(&catalog(scope_authority)).expect("graph");
    let finished_at = Utc.with_ymd_and_hms(2026, 8, 16, 12, 0, 0).unwrap();
    let report = VerificationReport {
      passed: true,
      started_at: finished_at,
      finished_at,
      commands: Vec::new(),
      executions: vec![execution("REQ-001/AC-01/VO-01")],
      warnings: Vec::new(),
    };
    record_report(&mut graph, revision, &report).expect("record evidence");
    graph
  }

  fn commit(cwd: &std::path::Path, message: &str) -> String {
    for args in [
      ["add", "-A"].as_slice(),
      ["commit", "-m", message].as_slice(),
    ] {
      let status = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git command");
      assert!(status.success());
    }
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
      assert!(std::process::Command::new("git")
        .args(args)
        .current_dir(repository.path())
        .status()
        .expect("configure git")
        .success());
    }
    std::fs::create_dir_all(repository.path().join("src")).expect("source directory");
    std::fs::write(repository.path().join("src/auth.rs"), "base").expect("auth source");
    std::fs::write(repository.path().join("README.md"), "base").expect("readme");
    commit(repository.path(), "base");
    repository
  }

  #[test]
  fn execution_records_only_its_explicit_obligation() {
    let mut catalog = catalog(DependencyScopeAuthority::ProjectConfigured);
    catalog.acceptance_criteria.push(AcceptanceCriterion {
      id: CriterionId::from("REQ-001/AC-02"),
      requirement_id: RequirementId::from("REQ-001"),
      description: "Second criterion".into(),
      mandatory: true,
    });
    let mut second = catalog.verification_obligations[0].clone();
    second.id = ObligationId::from("REQ-001/AC-02/VO-01");
    second.criterion_id = CriterionId::from("REQ-001/AC-02");
    catalog.verification_obligations.push(second);
    let mut graph = graph_from_catalog(&catalog).expect("graph");
    let now = Utc.with_ymd_and_hms(2026, 8, 16, 12, 0, 0).unwrap();
    let report = VerificationReport {
      passed: true,
      started_at: now,
      finished_at: now,
      commands: Vec::new(),
      executions: vec![execution("REQ-001/AC-01/VO-01")],
      warnings: Vec::new(),
    };
    record_report(&mut graph, "abc123", &report).expect("record report");

    assert_eq!(graph.evidence.len(), 1);
    assert_eq!(
      graph.criterion_verification_state(&CriterionId::from("REQ-001/AC-02"), &EvidencePolicy),
      Ok(VerificationState::Unverified)
    );
  }

  #[tokio::test]
  async fn agent_proposed_narrow_scope_invalidates_on_unrelated_change() {
    let repository = repository();
    let base = git::head(repository.path()).await.expect("base revision");
    let mut graph = verified_graph(&base, DependencyScopeAuthority::AgentProposed);
    std::fs::write(repository.path().join("README.md"), "changed").expect("change readme");
    let revision = commit(repository.path(), "change readme");

    let invalidated = invalidate_for_revision(repository.path(), &mut graph, &revision)
      .await
      .expect("invalidate");
    assert_eq!(invalidated.len(), 1);
  }

  #[tokio::test]
  async fn unknown_scope_invalidates_on_repository_change() {
    let repository = repository();
    let base = git::head(repository.path()).await.expect("base revision");
    let mut graph = verified_graph(&base, DependencyScopeAuthority::Unknown);
    std::fs::write(repository.path().join("README.md"), "changed").expect("change readme");
    let revision = commit(repository.path(), "change readme");

    let invalidated = invalidate_for_revision(repository.path(), &mut graph, &revision)
      .await
      .expect("invalidate");
    assert_eq!(invalidated.len(), 1);
  }

  #[tokio::test]
  async fn trusted_scope_preserves_evidence_for_unrelated_change() {
    let repository = repository();
    let base = git::head(repository.path()).await.expect("base revision");
    let mut graph = verified_graph(&base, DependencyScopeAuthority::ProjectConfigured);
    std::fs::write(repository.path().join("README.md"), "changed").expect("change readme");
    let revision = commit(repository.path(), "change readme");

    let invalidated = invalidate_for_revision(repository.path(), &mut graph, &revision)
      .await
      .expect("invalidate");
    assert!(invalidated.is_empty());
  }
}
