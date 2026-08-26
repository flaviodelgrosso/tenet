//! Requirement-catalog lifecycle, authority, and specification-coverage policy.

use std::{
  collections::{BTreeMap, BTreeSet},
  fmt::Write as _,
  path::Path,
};

use anyhow::{bail, Context, Result};
use tenet_domain::{
  config::Config,
  evidence::{AcceptanceCriterion, VerificationObligation},
  ids::{ArchitectSourceRef, CriterionId, ObligationId, RequirementId, SpecFragmentId},
  model::{ArchitectOutput, Requirement, RequirementCatalog},
  proof::{EvidenceContract, EvidencePredicate},
  worker::{derive_normative_fragments, CatalogCoverage, SpecFragment},
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
      if validate_evidence_contracts(catalog, config).is_err() {
        return Ok(Inspection {
          specification,
          specification_hash,
          authoritative: None,
          had_cached_catalog: true,
        });
      }
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

pub const ARCHITECT_FRAGMENT_BATCH_SIZE: usize = 64;

pub struct ArchitectBatch {
  pub fragment_ids: Vec<SpecFragmentId>,
  pub source_fragments: BTreeMap<ArchitectSourceRef, SpecFragmentId>,
  pub annotated_specification: String,
}

pub struct CatalogAuthority {
  fragments: Vec<SpecFragment>,
  fragment_indexes: BTreeMap<SpecFragmentId, usize>,
}

impl CatalogAuthority {
  pub fn derive(specification: &str) -> Self {
    let fragments = derive_normative_fragments(specification);
    let fragment_indexes = fragments
      .iter()
      .enumerate()
      .map(|(index, fragment)| (fragment.id.clone(), index))
      .collect();
    Self {
      fragments,
      fragment_indexes,
    }
  }

  pub fn architect_batches(&self) -> Vec<ArchitectBatch> {
    let batch_count = self.fragments.len().div_ceil(ARCHITECT_FRAGMENT_BATCH_SIZE);
    self
      .fragments
      .chunks(ARCHITECT_FRAGMENT_BATCH_SIZE)
      .enumerate()
      .map(|(index, fragments)| {
        let batch_number = index + 1;
        ArchitectBatch {
          fragment_ids: fragments
            .iter()
            .map(|fragment| fragment.id.clone())
            .collect(),
          source_fragments: fragments
            .iter()
            .enumerate()
            .map(|(index, fragment)| {
              (
                architect_source_ref(batch_number, index),
                fragment.id.clone(),
              )
            })
            .collect(),
          annotated_specification: annotate_batch(fragments, batch_number, batch_count),
        }
      })
      .collect()
  }

  pub fn build_batch(
    &self,
    batch: &ArchitectBatch,
    spec_hash: String,
    output: ArchitectOutput,
  ) -> Result<RequirementCatalog> {
    let requirements = output
      .requirements
      .into_iter()
      .map(|requirement| {
        let source_refs = requirement
          .source_refs
          .into_iter()
          .map(|source_ref| {
            let fragment_id = batch.source_fragments.get(&source_ref).ok_or_else(|| {
              anyhow::anyhow!(
                "{} references unknown architect source token {source_ref}",
                requirement.id
              )
            })?;
            let index = self
              .fragment_indexes
              .get(fragment_id)
              .with_context(|| format!("unknown assigned specification fragment {fragment_id}"))?;
            Ok(self.fragments[*index].reference())
          })
          .collect::<Result<Vec<_>>>()?;
        Ok(Requirement {
          id: requirement.id,
          title: requirement.title,
          description: requirement.description,
          required: true,
          source_refs,
        })
      })
      .collect::<Result<Vec<_>>>()?;

    let mut acceptance_criteria = output.acceptance_criteria;
    for criterion in &mut acceptance_criteria {
      criterion.mandatory = true;
    }
    let mut verification_obligations = output.verification_obligations;
    for obligation in &mut verification_obligations {
      obligation.required = true;
    }
    let batch_fragments = batch
      .fragment_ids
      .iter()
      .map(|fragment_id| {
        self
          .fragment_indexes
          .get(fragment_id)
          .map(|index| self.fragments[*index].clone())
          .with_context(|| format!("unknown assigned specification fragment {fragment_id}"))
      })
      .collect::<Result<Vec<_>>>()?;
    let coverage = coverage_for_fragments(batch_fragments, &requirements);
    Ok(RequirementCatalog {
      spec_hash,
      requirements,
      acceptance_criteria,
      verification_obligations,
      coverage,
    })
  }

  pub fn merge_batches(
    self,
    spec_hash: String,
    batches: Vec<RequirementCatalog>,
  ) -> Result<RequirementCatalog> {
    merge_batches(self.fragments, spec_hash, batches)
  }
}

fn architect_source_ref(batch_number: usize, fragment_index: usize) -> ArchitectSourceRef {
  ArchitectSourceRef::from(format!("B{batch_number:04}-F{:02}", fragment_index + 1))
}

fn annotate_batch(fragments: &[SpecFragment], batch_number: usize, batch_count: usize) -> String {
  let mut annotated = format!(
    "Controller-derived normative fragment batch {batch_number} of {batch_count}. Every short sourceRef token in this batch must appear in at least one requirement sourceRefs entry. Copy only sourceRef tokens; fragment IDs, hashes, sections, and reference metadata remain controller-owned.\n\n"
  );
  for (index, fragment) in fragments.iter().enumerate() {
    let section = fragment.section.as_deref().unwrap_or("<none>");
    let source_ref = architect_source_ref(batch_number, index);
    let _ = writeln!(
      annotated,
      "[sourceRef={source_ref} section={section}]\n{}\n",
      fragment.text
    );
  }
  annotated
}

pub fn build(
  specification: &str,
  spec_hash: String,
  output: ArchitectOutput,
) -> Result<RequirementCatalog> {
  let authority = CatalogAuthority::derive(specification);
  let batch = authority
    .architect_batches()
    .into_iter()
    .next()
    .context("specification contains no normative fragments")?;
  authority.build_batch(&batch, spec_hash, output)
}

fn coverage_for_fragments(
  normative_fragments: Vec<SpecFragment>,
  requirements: &[Requirement],
) -> CatalogCoverage {
  let covered: BTreeSet<_> = requirements
    .iter()
    .flat_map(|requirement| requirement.source_refs.iter())
    .map(|reference| reference.fragment_id.clone())
    .collect();
  let uncovered_fragment_ids = normative_fragments
    .iter()
    .filter(|fragment| !covered.contains(&fragment.id))
    .map(|fragment| fragment.id.clone())
    .collect();
  CatalogCoverage {
    normative_fragments,
    uncovered_fragment_ids,
  }
}

pub fn validate_derived_coverage(catalog: &RequirementCatalog) -> Result<()> {
  catalog
    .coverage
    .validate_references(&catalog.requirements)
    .map_err(anyhow::Error::msg)?;
  ensure_complete_coverage(catalog)
}

fn ensure_complete_coverage(catalog: &RequirementCatalog) -> Result<()> {
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

pub fn validate_coverage(catalog: &RequirementCatalog, specification: &str) -> Result<()> {
  catalog
    .coverage
    .validate_references(&catalog.requirements)
    .map_err(anyhow::Error::msg)?;
  let expected = CatalogCoverage::derive(specification, &catalog.requirements);
  if catalog.coverage != expected {
    bail!("catalog coverage does not match controller-derived specification fragments");
  }
  ensure_complete_coverage(catalog)
}
pub fn validate_batch_coverage(
  catalog: &RequirementCatalog,
  expected_fragment_ids: &[SpecFragmentId],
) -> Result<()> {
  catalog
    .coverage
    .validate_references(&catalog.requirements)
    .map_err(anyhow::Error::msg)?;
  let expected: BTreeSet<_> = expected_fragment_ids.iter().cloned().collect();
  let actual: BTreeSet<_> = catalog
    .requirements
    .iter()
    .flat_map(|requirement| requirement.source_refs.iter())
    .map(|reference| reference.fragment_id.clone())
    .collect();
  let uncovered: Vec<_> = expected.difference(&actual).cloned().collect();
  if !uncovered.is_empty() {
    bail!(
      "catalog does not cover normative specification fragments: {}",
      uncovered
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
    );
  }
  let outside_batch: Vec<_> = actual.difference(&expected).cloned().collect();
  if !outside_batch.is_empty() {
    bail!(
      "catalog references fragments outside assigned architect batch: {}",
      outside_batch
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
    );
  }
  Ok(())
}

fn merge_batches(
  normative_fragments: Vec<SpecFragment>,
  spec_hash: String,
  batches: Vec<RequirementCatalog>,
) -> Result<RequirementCatalog> {
  let mut requirements = Vec::new();
  let mut acceptance_criteria = Vec::new();
  let mut verification_obligations = Vec::new();

  for batch in batches {
    validate(&batch)?;
    let mut requirement_ids = BTreeMap::new();
    for requirement in batch.requirements {
      let new_id = RequirementId::from(format!("REQ-{:03}", requirements.len() + 1));
      requirement_ids.insert(requirement.id.clone(), new_id.clone());
      requirements.push(Requirement {
        id: new_id,
        title: requirement.title,
        description: requirement.description,
        required: true,
        source_refs: requirement.source_refs,
      });
    }

    let mut criterion_ids = BTreeMap::new();
    let mut criterion_counts = BTreeMap::<RequirementId, usize>::new();
    for criterion in batch.acceptance_criteria {
      let requirement_id = requirement_ids
        .get(&criterion.requirement_id)
        .cloned()
        .context("architect batch criterion targets an unknown requirement")?;
      let count = criterion_counts.entry(requirement_id.clone()).or_default();
      *count += 1;
      let id = CriterionId::from(format!("{requirement_id}/AC-{count:02}"));
      criterion_ids.insert(criterion.id, id.clone());
      acceptance_criteria.push(AcceptanceCriterion {
        id,
        requirement_id,
        description: criterion.description,
        mandatory: true,
      });
    }

    let mut obligation_counts = BTreeMap::<CriterionId, usize>::new();
    for obligation in batch.verification_obligations {
      let criterion_id = criterion_ids
        .get(&obligation.criterion_id)
        .cloned()
        .context("architect batch obligation targets an unknown criterion")?;
      let count = obligation_counts.entry(criterion_id.clone()).or_default();
      *count += 1;
      verification_obligations.push(VerificationObligation {
        id: ObligationId::from(format!("{criterion_id}/VO-{count:02}")),
        criterion_id,
        description: obligation.description,
        required: true,
        evidence_contract: obligation.evidence_contract,
      });
    }
  }

  let coverage = coverage_for_fragments(normative_fragments, &requirements);
  Ok(RequirementCatalog {
    spec_hash,
    requirements,
    acceptance_criteria,
    verification_obligations,
    coverage,
  })
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

  let mut criteria_by_requirement = BTreeMap::<RequirementId, Vec<&AcceptanceCriterion>>::new();
  for criterion in &catalog.acceptance_criteria {
    criteria_by_requirement
      .entry(criterion.requirement_id.clone())
      .or_default()
      .push(criterion);
  }
  let mut criterion_ids = BTreeSet::new();
  for requirement in &catalog.requirements {
    let criteria = criteria_by_requirement
      .get(&requirement.id)
      .map(Vec::as_slice)
      .unwrap_or_default();
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
  if criteria_by_requirement
    .keys()
    .any(|requirement_id| !requirement_ids.contains(requirement_id))
  {
    bail!("acceptance criterion targets an unknown requirement");
  }

  let mut obligations_by_criterion = BTreeMap::<CriterionId, Vec<&VerificationObligation>>::new();
  for obligation in &catalog.verification_obligations {
    obligations_by_criterion
      .entry(obligation.criterion_id.clone())
      .or_default()
      .push(obligation);
  }
  let mut seen_obligations = BTreeSet::new();
  for criterion in &catalog.acceptance_criteria {
    let obligations = obligations_by_criterion
      .get(&criterion.id)
      .map(Vec::as_slice)
      .unwrap_or_default();
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
  if obligations_by_criterion
    .keys()
    .any(|criterion_id| !criterion_ids.contains(criterion_id))
  {
    bail!("verification obligation targets an unknown acceptance criterion");
  }
  evidence::graph_from_catalog(catalog)?;
  Ok(())
}

pub fn validate_evidence_contracts(catalog: &RequirementCatalog, config: &Config) -> Result<()> {
  let checks: BTreeSet<_> = config
    .verification
    .checks
    .iter()
    .map(|check| check.name.as_str())
    .collect();
  for obligation in &catalog.verification_obligations {
    validate_contract(&obligation.evidence_contract, &checks)
      .with_context(|| format!("invalid evidence contract for {}", obligation.id))?;
  }
  Ok(())
}

fn validate_contract(contract: &EvidenceContract, checks: &BTreeSet<&str>) -> Result<()> {
  match contract {
    EvidenceContract::Artifact {
      predicate: EvidencePredicate::NamedProjectCheck { name },
    } => {
      if !checks.contains(name.as_str()) {
        bail!("named project check {name:?} is not controller-configured");
      }
    }
    EvidenceContract::Artifact {
      predicate: EvidencePredicate::ProjectVerification,
    } => {
      bail!(
        "generic project verification is a global completion gate, not an obligation evidence contract"
      );
    }
    EvidenceContract::Artifact {
      predicate: EvidencePredicate::ExecutableEvidence,
    } => {
      bail!("generic executable evidence requires a trusted verifier that is not configured");
    }
    EvidenceContract::Artifact {
      predicate: EvidencePredicate::SourceInspection,
    } => {}
    EvidenceContract::All { requirements } | EvidenceContract::Any { requirements } => {
      if requirements.is_empty() {
        bail!("composite evidence contract must not be empty");
      }
      for requirement in requirements {
        validate_contract(requirement, checks)?;
      }
    }
    EvidenceContract::HumanAttestation { .. } => {
      bail!("human attestation requires a configured controller issuer");
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use tenet_domain::{
    evidence::{AcceptanceCriterion, VerificationObligation},
    ids::{CriterionId, ObligationId, RequirementId},
    model::{ArchitectOutput, ArchitectRequirement},
  };

  use super::*;
  fn large_specification(fragment_count: usize) -> String {
    (1..=fragment_count)
      .map(|index| format!("Normative requirement {index}."))
      .collect::<Vec<_>>()
      .join("\n\n")
  }

  fn batch_output(source_refs: Vec<ArchitectSourceRef>) -> ArchitectOutput {
    let requirements = source_refs
      .into_iter()
      .enumerate()
      .map(|(index, source_ref)| ArchitectRequirement {
        id: RequirementId::from(format!("REQ-{:03}", index + 1)),
        title: format!("Batch requirement {}", index + 1),
        description: "Covers one assigned normative fragment".into(),
        required: true,
        source_refs: vec![source_ref],
      })
      .collect::<Vec<_>>();
    let acceptance_criteria = requirements
      .iter()
      .map(|requirement| AcceptanceCriterion {
        id: CriterionId::from(format!("{}/AC-01", requirement.id)),
        requirement_id: requirement.id.clone(),
        description: "The assigned behavior is observable".into(),
        mandatory: true,
      })
      .collect::<Vec<_>>();
    let verification_obligations = acceptance_criteria
      .iter()
      .map(|criterion| VerificationObligation {
        id: ObligationId::from(format!("{}/VO-01", criterion.id)),
        criterion_id: criterion.id.clone(),
        description: "Establish the assigned behavior".into(),
        required: true,
        evidence_contract: Default::default(),
      })
      .collect();
    ArchitectOutput {
      requirements,
      acceptance_criteria,
      verification_obligations,
    }
  }

  fn build_large_catalog(specification: &str) -> RequirementCatalog {
    let authority = CatalogAuthority::derive(specification);
    let batches = authority.architect_batches();
    let catalogs = batches
      .into_iter()
      .map(|batch| {
        let expected = batch.fragment_ids.clone();
        let catalog = authority
          .build_batch(
            &batch,
            "spec-hash".into(),
            batch_output(batch.source_fragments.keys().cloned().collect()),
          )
          .expect("expand architect fragment identifiers");
        validate(&catalog).expect("validate batch structure");
        validate_batch_coverage(&catalog, &expected).expect("validate assigned batch coverage");
        catalog
      })
      .collect();
    authority
      .merge_batches("spec-hash".into(), catalogs)
      .expect("merge architect batches")
  }

  #[test]
  fn two_hundred_fragments_are_partitioned_without_controller_metadata_in_output_contract() {
    let specification = large_specification(200);
    let batches = CatalogAuthority::derive(&specification).architect_batches();

    assert_eq!(batches.len(), 4);
    assert_eq!(
      batches
        .iter()
        .map(|batch| batch.fragment_ids.len())
        .sum::<usize>(),
      200
    );
    assert!(batches.iter().all(|batch| {
      batch.annotated_specification.contains("[sourceRef=")
        && !batch.annotated_specification.contains("fragmentId=")
        && !batch.annotated_specification.contains("textHash=")
    }));
    assert!(batches[0]
      .source_fragments
      .contains_key(&ArchitectSourceRef::from("B0001-F01")));
    assert!(batches[1]
      .source_fragments
      .contains_key(&ArchitectSourceRef::from("B0002-F01")));
  }

  #[test]
  fn batch_catalog_retains_only_its_assigned_fragments() {
    let specification = large_specification(200);
    let authority = CatalogAuthority::derive(&specification);
    let batch = authority
      .architect_batches()
      .into_iter()
      .next()
      .expect("first architect batch");
    let catalog = authority
      .build_batch(
        &batch,
        "spec-hash".into(),
        batch_output(batch.source_fragments.keys().cloned().collect()),
      )
      .expect("build bounded batch catalog");

    assert_eq!(catalog.coverage.normative_fragments.len(), 64);
  }

  #[test]
  fn stale_token_from_another_batch_is_rejected() {
    let specification = large_specification(65);
    let authority = CatalogAuthority::derive(&specification);
    let batches = authority.architect_batches();
    let mut output = batch_output(batches[0].source_fragments.keys().cloned().collect());
    output.requirements[0]
      .source_refs
      .push(batches[1].source_fragments.keys().next().unwrap().clone());

    let error = authority
      .build_batch(&batches[0], "spec-hash".into(), output)
      .expect_err("stale source token must fail closed");

    assert!(error
      .to_string()
      .contains("references unknown architect source token B0002-F01"));
  }
  #[test]
  fn small_specification_uses_one_batch_and_preserves_catalog_ids() {
    let specification = "The product must preserve the original small-spec workflow.";
    let catalog = build_large_catalog(specification);

    assert_eq!(
      CatalogAuthority::derive(specification)
        .architect_batches()
        .len(),
      1
    );
    assert_eq!(catalog.requirements[0].id.as_str(), "REQ-001");
    assert_eq!(catalog.acceptance_criteria[0].id.as_str(), "REQ-001/AC-01");
    assert_eq!(
      catalog.verification_obligations[0].id.as_str(),
      "REQ-001/AC-01/VO-01"
    );
    validate_coverage(&catalog, specification).expect("validate small catalog coverage");
  }

  #[test]
  fn merged_large_catalog_covers_every_controller_derived_fragment() {
    let specification = large_specification(200);
    let catalog = build_large_catalog(&specification);

    assert!(catalog.coverage.is_complete());
    assert_eq!(catalog.coverage.normative_fragments.len(), 200);
    validate_derived_coverage(&catalog).expect("validate global coverage");
  }

  #[test]
  fn source_inspection_contract_is_admitted_only_for_fail_closed_adjudication() {
    let contract = EvidenceContract::Artifact {
      predicate: EvidencePredicate::SourceInspection,
    };
    validate_contract(&contract, &BTreeSet::new())
      .expect("source inspection can collect supporting evidence but cannot prove");
  }

  #[test]
  fn generic_project_verification_cannot_prove_an_obligation() {
    let contract = EvidenceContract::Artifact {
      predicate: EvidencePredicate::ProjectVerification,
    };
    let error = validate_contract(&contract, &BTreeSet::new())
      .expect_err("project verification remains an independent completion gate");
    assert!(error
      .to_string()
      .contains("global completion gate, not an obligation evidence contract"));
  }

  #[test]
  fn human_attestation_requires_a_controller_issuer() {
    let contract = EvidenceContract::HumanAttestation {
      statement: "Release approved".into(),
    };
    let error = validate_contract(&contract, &BTreeSet::new())
      .expect_err("human attestation has no configured issuer");
    assert!(error
      .to_string()
      .contains("human attestation requires a configured controller issuer"));
  }

  #[test]
  fn generic_executable_evidence_requires_a_trusted_verifier() {
    let contract = EvidenceContract::Artifact {
      predicate: EvidencePredicate::ExecutableEvidence,
    };
    let error = validate_contract(&contract, &BTreeSet::new())
      .expect_err("trusted executable issuer is unavailable");
    assert!(error
      .to_string()
      .contains("requires a trusted verifier that is not configured"));
  }

  #[test]
  fn merged_batch_catalog_ids_are_deterministic_and_globally_sequential() {
    let specification = large_specification(200);
    let first = build_large_catalog(&specification);
    let second = build_large_catalog(&specification);

    assert_eq!(first, second);
    assert_eq!(first.requirements.len(), 200);
    assert!(first
      .requirements
      .iter()
      .enumerate()
      .all(|(index, requirement)| requirement.id.as_str() == format!("REQ-{:03}", index + 1)));
    validate(&first).expect("validate deterministic merged identifiers");
  }
}
