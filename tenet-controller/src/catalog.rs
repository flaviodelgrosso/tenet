//! Requirement-catalog lifecycle, authority, and specification-coverage policy.

use std::{
  collections::{BTreeMap, BTreeSet},
  fmt::Write as _,
  path::Path,
};

use anyhow::{bail, Context, Result};
use tenet_domain::{
  config::Config,
  model::{ArchitectOutput, RequirementCatalog},
  worker::CatalogCoverage,
};

use tenet_runtime::store;

pub struct Inspection {
  pub specification: String,
  pub specification_hash: String,
  pub authoritative: Option<RequirementCatalog>,
  pub had_cached_catalog: bool,
}

pub async fn inspect(cwd: &Path, config: &Config) -> Result<Inspection> {
  let (specification, specification_hash) = store::spec_text_and_hash(cwd, config).await?;
  let cached = store::read_catalog(cwd).await?;
  if let Some(catalog) = &cached {
    validate(catalog).context("cached requirement catalog is structurally invalid")?;
    if catalog.spec_hash == specification_hash {
      validate_coverage(catalog, &specification)
        .context("cached requirement catalog coverage is invalid")?;
      return Ok(Inspection {
        specification,
        specification_hash,
        authoritative: Some(catalog.clone()),
        had_cached_catalog: true,
      });
    }
  }
  Ok(Inspection {
    specification,
    specification_hash,
    authoritative: None,
    had_cached_catalog: cached.is_some(),
  })
}

pub async fn specification_changed(
  cwd: &Path,
  config: &Config,
  catalog: &RequirementCatalog,
) -> Result<bool> {
  let (_, current_hash) = store::spec_text_and_hash(cwd, config).await?;
  Ok(current_hash != catalog.spec_hash)
}
use crate::evidence;

pub fn annotated_specification(specification: &str) -> String {
  let mut annotated = String::from(
    "Normative fragments below are controller-derived. Every fragmentId must appear in at least one requirement sourceRefs entry with the exact section and textHash.\n\n",
  );
  for fragment in tenet_domain::worker::derive_normative_fragments(specification) {
    let section = fragment.section.as_deref().unwrap_or("<none>");
    let _ = writeln!(
      annotated,
      "[fragmentId={} textHash={} section={section}]\n{}\n",
      fragment.id, fragment.text_hash, fragment.text
    );
  }
  annotated
}

pub fn build(
  specification: &str,
  spec_hash: String,
  output: ArchitectOutput,
) -> Result<RequirementCatalog> {
  let mut requirements = output.requirements;
  for requirement in &mut requirements {
    requirement.required = true;
  }
  let fragments: BTreeMap<_, _> = tenet_domain::worker::derive_normative_fragments(specification)
    .into_iter()
    .map(|fragment| (fragment.id.clone(), fragment.reference()))
    .collect();
  for requirement in &mut requirements {
    for reference in &mut requirement.source_refs {
      if let Some(authoritative) = fragments.get(&reference.fragment_id) {
        reference.clone_from(authoritative);
      }
    }
  }

  let mut acceptance_criteria = output.acceptance_criteria;
  for criterion in &mut acceptance_criteria {
    criterion.mandatory = true;
  }

  let mut verification_obligations = output.verification_obligations;
  for obligation in &mut verification_obligations {
    obligation.required = true;
  }

  let coverage = CatalogCoverage::derive(specification, &requirements);
  Ok(RequirementCatalog {
    spec_hash,
    requirements,
    acceptance_criteria,
    verification_obligations,
    coverage,
  })
}

pub fn validate_coverage(catalog: &RequirementCatalog, specification: &str) -> Result<()> {
  catalog
    .coverage
    .validate_references(&catalog.requirements)
    .map_err(anyhow::Error::msg)?;
  let expected = CatalogCoverage::derive(specification, &catalog.requirements);
  if catalog.coverage != expected {
    bail!("catalog coverage does not match controller-derived specification fragments");
  }
  if !catalog.coverage.is_complete() {
    let uncovered = catalog
      .coverage
      .uncovered_fragment_ids
      .iter()
      .map(ToString::to_string)
      .collect::<Vec<_>>()
      .join(", ");
    bail!("catalog does not cover normative specification fragments: {uncovered}");
  }
  Ok(())
}

pub fn validate(catalog: &RequirementCatalog) -> Result<()> {
  if catalog.requirements.is_empty() {
    bail!("architect produced no requirements");
  }
  let mut requirement_ids = BTreeSet::new();
  for (index, requirement) in catalog.requirements.iter().enumerate() {
    let expected = format!("REQ-{:03}", index + 1);
    if requirement.id.as_str() != expected {
      bail!(
        "requirement id {} is unstable; expected {expected}",
        requirement.id
      );
    }
    if !requirement_ids.insert(requirement.id.clone()) {
      bail!("duplicate requirement id {}", requirement.id);
    }
    if requirement.title.trim().is_empty() || requirement.description.trim().is_empty() {
      bail!("{} is missing title or description", requirement.id);
    }
  }

  let mut criterion_ids = BTreeSet::new();
  for requirement in &catalog.requirements {
    let criteria: Vec<_> = catalog
      .acceptance_criteria
      .iter()
      .filter(|criterion| criterion.requirement_id == requirement.id)
      .collect();
    if requirement.required && !criteria.iter().any(|criterion| criterion.mandatory) {
      bail!("{} has no mandatory acceptance criterion", requirement.id);
    }
    for (index, criterion) in criteria.iter().enumerate() {
      let expected = format!("{}/AC-{:02}", requirement.id, index + 1);
      if criterion.id.as_str() != expected {
        bail!(
          "criterion id {} is unstable; expected {expected}",
          criterion.id
        );
      }
      if !criterion_ids.insert(criterion.id.clone()) {
        bail!("duplicate criterion id {}", criterion.id);
      }
    }
  }
  if catalog
    .acceptance_criteria
    .iter()
    .any(|criterion| !requirement_ids.contains(&criterion.requirement_id))
  {
    bail!("acceptance criterion targets an unknown requirement");
  }

  let mut seen_obligations = BTreeSet::new();
  for criterion in &catalog.acceptance_criteria {
    let obligations: Vec<_> = catalog
      .verification_obligations
      .iter()
      .filter(|obligation| obligation.criterion_id == criterion.id)
      .collect();
    if criterion.mandatory && !obligations.iter().any(|obligation| obligation.required) {
      bail!("{} has no required verification obligation", criterion.id);
    }
    for (index, obligation) in obligations.iter().enumerate() {
      let expected = format!("{}/VO-{:02}", criterion.id, index + 1);
      if obligation.id.as_str() != expected {
        bail!(
          "verification obligation id {} is unstable; expected {expected}",
          obligation.id
        );
      }
      if !seen_obligations.insert(obligation.id.clone()) {
        bail!("duplicate verification obligation id {}", obligation.id);
      }
    }
  }
  if catalog
    .verification_obligations
    .iter()
    .any(|obligation| !criterion_ids.contains(&obligation.criterion_id))
  {
    bail!("verification obligation targets an unknown acceptance criterion");
  }
  evidence::graph_from_catalog(catalog)?;
  Ok(())
}
