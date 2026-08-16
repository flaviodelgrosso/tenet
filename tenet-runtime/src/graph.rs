use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};
use petgraph::{
  algo::is_cyclic_directed,
  stable_graph::{NodeIndex, StableDiGraph},
  Direction,
};

use tenet_domain::model::{ReconcileResult, RequirementCatalog, WorkUnit};

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
      unit.validate(&known_requirements)?;
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

#[cfg(test)]
mod tests {
  use super::*;
  use tenet_domain::model::{Requirement, RequirementAssessment, RequirementStatus, WorkScope};

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
