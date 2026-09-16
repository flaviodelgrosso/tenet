//! CompletionPolicy v1 admission and evaluation semantics.

use std::collections::{BTreeMap, BTreeSet};

use tenet_domain::{
  algebra::{
    AlgebraError, AssuranceProfileId, AssuranceRequirementV1, COMPLETION_POLICY_V1,
    CompletionContractV1, CompletionEvaluation, CompletionPolicyId, CompletionState,
    CriterionEvaluation, CriterionState, Evaluation, EvaluationScope, EvidenceControl,
    EvidenceControlRequirementV1, EvidenceDisposition, EvidenceEvaluation, EvidenceResult,
    LOCAL_V1, PROTECTED_V1, RUNNER_SEMANTICS_V1, RequirementEvaluation, Verifier, VerifierMaterial,
    VerifierRun,
  },
  authority::{Admission, Authority, AuthorityProposal, ReconciliationReport, SpecSnapshot},
  completion::Verdict,
  evidence::OracleIdentity,
  policy::{VerificationPolicy, VerifierAuthority, VerifierSpec},
  snapshot::TreeEntry,
};

use crate::{
  authority::{admission_id, authority_id, validate_admission},
  digest::canonical_digest,
  identity::{sealed_executable_content_id, subtree_content_id},
};

pub struct AdmissionChain<'a> {
  pub admission: &'a Admission,
  pub proposal: &'a AuthorityProposal,
  pub report: &'a ReconciliationReport,
  pub authority: &'a Authority,
  pub spec: &'a SpecSnapshot,
  pub policy: &'a VerificationPolicy,
  pub surface_entries: &'a [TreeEntry],
}

pub fn validate_contract(contract: &CompletionContractV1) -> Result<(), AlgebraError> {
  if contract.schema_version != 1 {
    return Err(AlgebraError::UnsupportedSchemaVersion(
      contract.schema_version,
    ));
  }
  require_policy_v1(&contract.policy)?;
  if contract.requirements.is_empty() {
    return Err(AlgebraError::MissingRequirements);
  }

  let mut requirement_ids = BTreeSet::new();
  let mut criterion_ids = BTreeSet::new();
  let mut verifier_ids = BTreeSet::new();
  for requirement in &contract.requirements {
    validate_id("requirement", &requirement.id.0)?;
    validate_statement("requirement", &requirement.id.0, &requirement.statement)?;
    if !requirement_ids.insert(requirement.id.0.as_str()) {
      return Err(AlgebraError::DuplicateId {
        kind: "requirement",
        id: requirement.id.0.clone(),
      });
    }
    if requirement.criteria.is_empty() {
      return Err(AlgebraError::MissingCriteria(requirement.id.0.clone()));
    }
    for criterion in &requirement.criteria {
      validate_id("criterion", &criterion.id.0)?;
      validate_statement("criterion", &criterion.id.0, &criterion.proposition)?;
      if !criterion_ids.insert(criterion.id.0.as_str()) {
        return Err(AlgebraError::DuplicateId {
          kind: "criterion",
          id: criterion.id.0.clone(),
        });
      }
      if criterion.verifiers.is_empty() {
        return Err(AlgebraError::MissingVerifiers(criterion.id.0.clone()));
      }
      for verifier in &criterion.verifiers {
        validate_id("verifier", &verifier.id.0)?;
        if !verifier_ids.insert(verifier.id.0.as_str()) {
          return Err(AlgebraError::DuplicateId {
            kind: "verifier",
            id: verifier.id.0.clone(),
          });
        }
      }
      let authority_bound = criterion
        .verifiers
        .iter()
        .filter(|verifier| verifier.material.control() == EvidenceControl::AuthorityBound)
        .count();
      let possible = match criterion.evidence.control {
        EvidenceControlRequirementV1::AuthorityBoundOnly => {
          authority_bound == criterion.verifiers.len()
        }
        EvidenceControlRequirementV1::AtLeastOneAuthorityBound => authority_bound > 0,
        EvidenceControlRequirementV1::CandidateControlledPermitted => true,
      };
      if !possible {
        return Err(AlgebraError::ImpossibleEvidenceControl(
          criterion.id.0.clone(),
        ));
      }
    }
  }
  Ok(())
}

pub fn validate_completion_admission(
  contract: &CompletionContractV1,
  chain: &AdmissionChain<'_>,
) -> Result<(), AlgebraError> {
  validate_contract(contract)?;
  if canonical_digest(contract).ok().as_deref() != Some(&chain.authority.contract.0) {
    return Err(AlgebraError::ContractMismatch);
  }
  validate_admission(
    chain.admission,
    chain.proposal,
    chain.report,
    chain.authority,
    chain.spec,
  )?;
  Ok(())
}

pub fn evidence_result(
  observation: &tenet_domain::algebra::ExecutionObservation,
  definition: &VerifierSpec,
) -> EvidenceResult {
  if observation.infrastructure_error.is_some() {
    return EvidenceResult::InfrastructureError;
  }
  definition
    .command
    .result
    .interpret(observation.exit_code, observation.timed_out, false)
}

pub fn evaluate(
  contract: &CompletionContractV1,
  chain: &AdmissionChain<'_>,
  evaluation: &Evaluation,
) -> Result<CompletionEvaluation, AlgebraError> {
  validate_completion_admission(contract, chain)?;
  if admission_id(chain.admission).ok().as_ref() != Some(&evaluation.admission) {
    return Err(AlgebraError::AdmissionMismatch);
  }
  if authority_id(chain.authority).ok().as_ref() != Some(&evaluation.authority) {
    return Err(AlgebraError::AuthorityMismatch);
  }

  let requirements = scoped_requirements(contract, &evaluation.scope)?;
  let mut expected = BTreeMap::new();
  for requirement in &requirements {
    for criterion in &requirement.criteria {
      for verifier in &criterion.verifiers {
        admitted_definition(chain, verifier)?;
        expected.insert(verifier.id.clone(), verifier);
      }
    }
  }
  let runs = validate_runs(contract, chain, evaluation, &expected)?;
  let mut requirement_results = Vec::with_capacity(requirements.len());
  for requirement in requirements {
    let mut criteria = Vec::with_capacity(requirement.criteria.len());
    for criterion in &requirement.criteria {
      let mut saw_failure = false;
      let mut saw_infrastructure = false;
      let mut saw_missing = false;
      let mut saw_inadmissible = false;
      let mut saw_inconclusive = false;
      let mut evidence = Vec::with_capacity(criterion.verifiers.len());
      for verifier in &criterion.verifiers {
        let definition = admitted_definition(chain, verifier)?;
        let Some(run) = runs.get(&verifier.id) else {
          saw_missing = true;
          evidence.push(EvidenceEvaluation {
            verifier: verifier.id.clone(),
            result: None,
            disposition: EvidenceDisposition::Missing,
          });
          continue;
        };
        let assurance_admissible =
          assurance_satisfies(&run.context.assurance, criterion.evidence.assurance)?;
        let result = evidence_result(&run.observation, definition);
        evidence.push(EvidenceEvaluation {
          verifier: verifier.id.clone(),
          result: Some(result),
          disposition: if matches!(result, EvidenceResult::Pass | EvidenceResult::Inconclusive)
            && !assurance_admissible
          {
            EvidenceDisposition::RejectedAssurance
          } else {
            EvidenceDisposition::Observed
          },
        });
        match result {
          EvidenceResult::Fail => saw_failure = true,
          EvidenceResult::InfrastructureError => saw_infrastructure = true,
          EvidenceResult::Pass if !assurance_admissible => saw_inadmissible = true,
          EvidenceResult::Inconclusive if !assurance_admissible => saw_inadmissible = true,
          EvidenceResult::Pass => {}
          EvidenceResult::Inconclusive => saw_inconclusive = true,
        }
      }
      let state = if saw_failure {
        CriterionState::Contradicted
      } else if saw_infrastructure {
        CriterionState::InfrastructureError
      } else if saw_missing {
        CriterionState::MissingEvidence
      } else if saw_inadmissible {
        CriterionState::InadmissibleEvidence
      } else if saw_inconclusive {
        CriterionState::Inconclusive
      } else {
        CriterionState::Satisfied
      };
      criteria.push(CriterionEvaluation {
        criterion: criterion.id.clone(),
        state,
        evidence,
      });
    }
    requirement_results.push(RequirementEvaluation {
      requirement: requirement.id.clone(),
      state: aggregate(criteria.iter().map(|criterion| criterion.state)),
      criteria,
    });
  }

  let state = aggregate(
    requirement_results
      .iter()
      .map(|requirement| match requirement.state {
        CompletionState::Satisfied => CriterionState::Satisfied,
        CompletionState::Contradicted => CriterionState::Contradicted,
        CompletionState::Inconclusive => CriterionState::Inconclusive,
        CompletionState::InfrastructureError => CriterionState::InfrastructureError,
      }),
  );
  let verdict = matches!(evaluation.scope, EvaluationScope::Final).then(|| match state {
    CompletionState::Satisfied => Verdict::Done,
    CompletionState::Contradicted => Verdict::NotDone,
    CompletionState::Inconclusive => Verdict::Inconclusive,
    CompletionState::InfrastructureError => Verdict::InfrastructureError,
  });
  Ok(CompletionEvaluation {
    state,
    requirements: requirement_results,
    verdict,
  })
}
fn admitted_definition<'a>(
  chain: &'a AdmissionChain<'_>,
  verifier: &Verifier,
) -> Result<&'a VerifierSpec, AlgebraError> {
  let definition = chain
    .policy
    .verifiers
    .iter()
    .find(|definition| definition.id == verifier.id.0)
    .ok_or(AlgebraError::AdmittedVerifierMismatch)?;
  let matches_material = matches!(
    (verifier.material, definition.authority),
    (VerifierMaterial::Candidate, VerifierAuthority::Project)
      | (
        VerifierMaterial::AuthorityBundle,
        VerifierAuthority::AuthoritySnapshot
      )
  );
  if !matches_material {
    return Err(AlgebraError::AdmittedVerifierMismatch);
  }
  Ok(definition)
}

fn require_policy_v1(policy: &CompletionPolicyId) -> Result<(), AlgebraError> {
  if policy.0 == COMPLETION_POLICY_V1 {
    Ok(())
  } else {
    Err(AlgebraError::UnsupportedCompletionPolicy(policy.0.clone()))
  }
}

fn scoped_requirements<'a>(
  contract: &'a CompletionContractV1,
  scope: &EvaluationScope,
) -> Result<Vec<&'a tenet_domain::algebra::Requirement>, AlgebraError> {
  match scope {
    EvaluationScope::Final => Ok(contract.requirements.iter().collect()),
    EvaluationScope::Requirement { requirement } => contract
      .requirements
      .iter()
      .find(|item| item.id == *requirement)
      .map(|item| vec![item])
      .ok_or_else(|| AlgebraError::UnknownRequirement(requirement.0.clone())),
  }
}

fn validate_runs<'a>(
  contract: &CompletionContractV1,
  chain: &AdmissionChain<'_>,
  evaluation: &'a Evaluation,
  expected: &BTreeMap<tenet_domain::algebra::VerifierId, &Verifier>,
) -> Result<BTreeMap<tenet_domain::algebra::VerifierId, &'a VerifierRun>, AlgebraError> {
  let mut runs = BTreeMap::new();
  for run in &evaluation.runs {
    if run.admission != evaluation.admission {
      return Err(AlgebraError::RunAdmissionMismatch);
    }
    if run.authority != evaluation.authority {
      return Err(AlgebraError::RunAuthorityMismatch);
    }
    if run.contract != chain.authority.contract {
      return Err(AlgebraError::RunContractMismatch);
    }
    if run.completion_policy != contract.policy {
      return Err(AlgebraError::RunCompletionPolicyMismatch);
    }
    if run.candidate != evaluation.candidate {
      return Err(AlgebraError::RunCandidateMismatch);
    }
    if run.context.runner_semantics.0 != RUNNER_SEMANTICS_V1 {
      return Err(AlgebraError::UnsupportedRunnerSemantics(
        run.context.runner_semantics.0.clone(),
      ));
    }
    if run.context.assurance != run.provenance.assurance
      || run.context.runner_semantics != run.provenance.runner_semantics
      || run.context.platform != run.provenance.platform
      || run.context.resolved_program != run.provenance.resolved_program
      || run.context.resolved_program_digest != run.provenance.resolved_program_digest
      || run.provenance.runner_identity.0.trim().is_empty()
      || run.provenance.tenet_version.trim().is_empty()
      || run
        .provenance
        .execution_environment_identity
        .0
        .trim()
        .is_empty()
    {
      return Err(AlgebraError::RunProvenanceMismatch);
    }
    let Some(expected_verifier) = expected.get(&run.verifier) else {
      return Err(AlgebraError::VerifierOutsideScope(run.verifier.0.clone()));
    };
    let definition = admitted_definition(chain, expected_verifier)?;
    validate_oracle_identity(run, definition, chain.surface_entries)?;
    if runs.insert(run.verifier.clone(), run).is_some() {
      return Err(AlgebraError::DuplicateRun(run.verifier.0.clone()));
    }
  }
  Ok(runs)
}

fn validate_oracle_identity(
  run: &VerifierRun,
  definition: &VerifierSpec,
  surface_entries: &[TreeEntry],
) -> Result<(), AlgebraError> {
  let expected_definition =
    canonical_digest(definition).map_err(|_| AlgebraError::RunOracleMismatch)?;
  let derived_result = evidence_result(&run.observation, definition);
  if run.observation.result != derived_result {
    return Err(AlgebraError::RunProvenanceMismatch);
  }
  let valid = match (&run.provenance.oracle_identity, definition.authority) {
    (
      OracleIdentity::Project {
        verifier_id,
        candidate_id,
        definition_digest,
      },
      VerifierAuthority::Project,
    ) => {
      verifier_id == &run.verifier.0
        && candidate_id == &run.candidate
        && definition_digest == &expected_definition
    }
    (
      OracleIdentity::AuthoritySnapshot {
        verifier_id,
        authority_id,
        bundle_path,
        bundle_content_id,
        executable_content_id,
        definition_digest,
      },
      VerifierAuthority::AuthoritySnapshot,
    ) => {
      let Some(expected_bundle_path) = definition.oracle_path.as_deref() else {
        return Err(AlgebraError::RunOracleMismatch);
      };
      let Some(expected_executable_path) = definition
        .oracle_executable_path()
        .and_then(|path| path.to_str().map(str::to_owned))
      else {
        return Err(AlgebraError::RunOracleMismatch);
      };
      let expected_bundle_content_id = subtree_content_id(surface_entries, expected_bundle_path)
        .map_err(|_| AlgebraError::RunOracleMismatch)?;
      let Some(expected_executable_content_id) =
        sealed_executable_content_id(surface_entries, &expected_executable_path)
      else {
        return Err(AlgebraError::RunOracleMismatch);
      };
      verifier_id == &run.verifier.0
        && authority_id == &run.authority
        && bundle_path == expected_bundle_path
        && bundle_content_id == &expected_bundle_content_id
        && executable_content_id == &expected_executable_content_id
        && definition_digest == &expected_definition
    }
    (
      OracleIdentity::Unavailable {
        verifier_id,
        definition_digest,
      },
      _,
    ) if derived_result == EvidenceResult::InfrastructureError => {
      verifier_id == &run.verifier.0 && definition_digest == &expected_definition
    }
    _ => false,
  };
  if valid {
    Ok(())
  } else {
    Err(AlgebraError::RunOracleMismatch)
  }
}

fn assurance_satisfies(
  profile: &AssuranceProfileId,
  requirement: AssuranceRequirementV1,
) -> Result<bool, AlgebraError> {
  match profile.0.as_str() {
    LOCAL_V1 => Ok(requirement == AssuranceRequirementV1::LocalOrStronger),
    PROTECTED_V1 => Ok(true),
    _ => Err(AlgebraError::UnsupportedAssuranceProfile(profile.0.clone())),
  }
}

fn validate_id(kind: &'static str, id: &str) -> Result<(), AlgebraError> {
  if id.trim().is_empty() {
    Err(AlgebraError::BlankId { kind })
  } else {
    Ok(())
  }
}

fn validate_statement(kind: &'static str, id: &str, value: &str) -> Result<(), AlgebraError> {
  if value.trim().is_empty() {
    Err(AlgebraError::BlankStatement {
      kind,
      id: id.to_owned(),
    })
  } else {
    Ok(())
  }
}

fn aggregate(states: impl Iterator<Item = CriterionState>) -> CompletionState {
  let mut saw_infrastructure = false;
  let mut saw_inconclusive = false;
  for state in states {
    match state {
      CriterionState::Contradicted => return CompletionState::Contradicted,
      CriterionState::InfrastructureError => saw_infrastructure = true,
      CriterionState::Satisfied => {}
      CriterionState::MissingEvidence
      | CriterionState::InadmissibleEvidence
      | CriterionState::Inconclusive => saw_inconclusive = true,
    }
  }
  if saw_infrastructure {
    CompletionState::InfrastructureError
  } else if saw_inconclusive {
    CompletionState::Inconclusive
  } else {
    CompletionState::Satisfied
  }
}
