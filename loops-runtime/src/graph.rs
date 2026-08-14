use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};
use petgraph::{
  algo::is_cyclic_directed,
  stable_graph::{NodeIndex, StableDiGraph},
  Direction,
};

use loops_domain::model::{ReconcileResult, RequirementCatalog, WorkUnit};

/// Controller-owned dependency graph proposed by reconciliation and validated before scheduling.
pub struct WorkGraph {
  graph: StableDiGraph<WorkUnit, ()>,
  by_id: BTreeMap<String, NodeIndex>,
}

impl WorkGraph {
  pub fn from_reconcile(catalog: &RequirementCatalog, result: &ReconcileResult) -> Result<Self> {
    let known_requirements: BTreeSet<_> = catalog
      .requirements
      .iter()
      .map(|item| item.id.as_str())
      .collect();
    let mut graph = StableDiGraph::new();
    let mut by_id = BTreeMap::new();

    for unit in &result.work_units {
      validate_unit(unit, &known_requirements)?;
      if by_id.contains_key(&unit.id) {
        bail!("duplicate work unit id {}", unit.id);
      }
      let index = graph.add_node(unit.clone());
      by_id.insert(unit.id.clone(), index);
    }

    for unit in &result.work_units {
      let target = by_id[&unit.id];
      for dependency in &unit.depends_on {
        if dependency == &unit.id {
          bail!("{} depends on itself", unit.id);
        }
        let Some(&source) = by_id.get(dependency) else {
          bail!("{} depends on unknown work unit {dependency}", unit.id);
        };
        graph.add_edge(source, target, ());
      }
    }

    let value = Self { graph, by_id };
    value.validate()?;
    Ok(value)
  }

  pub fn validate(&self) -> Result<()> {
    if is_cyclic_directed(&self.graph) {
      bail!("work graph contains a dependency cycle");
    }
    Ok(())
  }

  pub fn ready_frontier(
    &self,
    completed: &BTreeSet<String>,
    active: &BTreeSet<String>,
  ) -> Vec<WorkUnit> {
    let mut ready: Vec<_> = self
      .by_id
      .iter()
      .filter(|(id, _)| !completed.contains(*id) && !active.contains(*id))
      .filter(|(_, index)| {
        self
          .graph
          .neighbors_directed(**index, Direction::Incoming)
          .all(|dependency| completed.contains(&self.graph[dependency].id))
      })
      .map(|(_, index)| self.graph[*index].clone())
      .collect();
    ready.sort_by(|left, right| left.id.cmp(&right.id));
    ready
  }

  pub fn units(&self) -> impl Iterator<Item = &WorkUnit> {
    self.by_id.values().map(|index| &self.graph[*index])
  }
}

fn validate_unit(unit: &WorkUnit, known_requirements: &BTreeSet<&str>) -> Result<()> {
  if unit.id.trim().is_empty() || unit.title.trim().is_empty() || unit.objective.trim().is_empty() {
    bail!("work unit is missing id, title, or objective");
  }
  if !unit
    .id
    .chars()
    .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    || matches!(unit.id.as_str(), "." | "..")
  {
    bail!("work unit id contains unsafe path characters: {}", unit.id);
  }
  if unit.requirement_ids.is_empty() {
    bail!("{} targets no requirements", unit.id);
  }
  if unit.acceptance_criteria.is_empty() {
    bail!("{} has no acceptance criteria", unit.id);
  }
  if unit.scope.paths.is_empty() || unit.scope.paths.iter().any(|path| path.trim().is_empty()) {
    bail!("{} has an empty declared scope", unit.id);
  }
  for check in &unit.suggested_checks {
    if check.trim().is_empty() || check.contains(['\r', '\n', '`']) {
      bail!(
        "{} has an invalid suggested check; expected one executable shell command without prose or Markdown backticks: {check}",
        unit.id
      );
    }
  }
  for requirement in &unit.requirement_ids {
    if !known_requirements.contains(requirement.as_str()) {
      bail!("{} targets unknown requirement {requirement}", unit.id);
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use loops_domain::model::{Requirement, RequirementAssessment, RequirementStatus, WorkScope};

  fn catalog() -> RequirementCatalog {
    RequirementCatalog {
      spec_hash: "hash".into(),
      requirements: vec![Requirement {
        id: "REQ-001".into(),
        title: "Requirement".into(),
        description: "Description".into(),
        acceptance_criteria: vec!["Observable".into()],
      }],
    }
  }

  fn unit(id: &str, dependencies: &[&str]) -> WorkUnit {
    WorkUnit {
      id: id.into(),
      title: id.into(),
      objective: format!("Implement {id}"),
      requirement_ids: vec!["REQ-001".into()],
      acceptance_criteria: vec!["Passes".into()],
      suggested_checks: Vec::new(),
      depends_on: dependencies.iter().map(|value| (*value).into()).collect(),
      scope: WorkScope {
        paths: vec![format!("src/{id}/**")],
      },
    }
  }

  fn reconcile(units: Vec<WorkUnit>) -> ReconcileResult {
    ReconcileResult {
      complete: false,
      summary: "work remains".into(),
      requirements: vec![RequirementAssessment {
        id: "REQ-001".into(),
        status: RequirementStatus::Missing,
        evidence: Vec::new(),
        gaps: vec!["missing".into()],
      }],
      work_units: units,
    }
  }

  #[test]
  fn ready_frontier_follows_diamond_dependencies() {
    let graph = WorkGraph::from_reconcile(
      &catalog(),
      &reconcile(vec![
        unit("A", &[]),
        unit("B", &["A"]),
        unit("C", &["A"]),
        unit("D", &["B", "C"]),
      ]),
    )
    .expect("valid graph");
    let completed = BTreeSet::from(["A".to_owned()]);

    let ready = graph.ready_frontier(&completed, &BTreeSet::new());

    assert_eq!(
      ready
        .iter()
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>(),
      ["B", "C"]
    );
  }

  #[test]
  fn graph_rejects_dependency_cycles() {
    let error = WorkGraph::from_reconcile(
      &catalog(),
      &reconcile(vec![unit("A", &["B"]), unit("B", &["A"])]),
    )
    .err()
    .expect("cycle rejected");

    assert!(error.to_string().contains("cycle"));
  }

  #[test]
  fn graph_rejects_unknown_dependency() {
    let error = WorkGraph::from_reconcile(&catalog(), &reconcile(vec![unit("A", &["missing"])]))
      .err()
      .expect("unknown dependency rejected");

    assert!(error.to_string().contains("unknown work unit"));
  }

  #[test]
  fn graph_rejects_duplicate_ids() {
    let error =
      WorkGraph::from_reconcile(&catalog(), &reconcile(vec![unit("A", &[]), unit("A", &[])]))
        .err()
        .expect("duplicate rejected");

    assert!(error.to_string().contains("duplicate work unit"));
  }
  #[test]
  fn graph_rejects_unknown_requirement() {
    let mut invalid = unit("A", &[]);
    invalid.requirement_ids = vec!["REQ-404".into()];

    let error = WorkGraph::from_reconcile(&catalog(), &reconcile(vec![invalid]))
      .err()
      .expect("unknown requirement rejected");

    assert!(error.to_string().contains("unknown requirement"));
  }

  #[test]
  fn graph_rejects_self_dependency() {
    let error = WorkGraph::from_reconcile(&catalog(), &reconcile(vec![unit("A", &["A"])]))
      .err()
      .expect("self dependency rejected");

    assert!(error.to_string().contains("itself"));
  }

  #[test]
  fn graph_rejects_structurally_invalid_unit() {
    let mut invalid = unit("A", &[]);
    invalid.scope.paths.clear();

    let error = WorkGraph::from_reconcile(&catalog(), &reconcile(vec![invalid]))
      .err()
      .expect("empty scope rejected");

    assert!(error.to_string().contains("empty declared scope"));
  }
  #[test]
  fn graph_rejects_markdown_check_descriptions() {
    let mut invalid = unit("A", &[]);
    invalid.suggested_checks =
      vec!["Run `cargo run --quiet` and verify stdout is a datetime value.".into()];

    let error = WorkGraph::from_reconcile(&catalog(), &reconcile(vec![invalid]))
      .err()
      .expect("Markdown check description rejected");

    assert!(error.to_string().contains("invalid suggested check"));
  }
}
