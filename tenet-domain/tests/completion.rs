use std::collections::BTreeMap;

use tenet_domain::{
  completion::{
    BlockerCode, DerivationContext, ObligationState, Verdict, derive_completion,
    derive_obligation_state,
  },
  contract::{
    AdmittedContract, AssuranceId, ClaimEvidenceContract, EvidenceContract, ObligationId,
    OracleAssuranceContract, Requirement, RequirementId, VerificationObligation,
  },
  evidence::{
    ArtifactAuthority, ArtifactProvenance, ArtifactValidity, AuthorityId, CandidateId,
    ContentObjectId, DependencySurface, EvidenceArtifact, EvidenceEffect,
    ExecutionEnvironmentIdentity, ExecutionProvenance, OracleIdentity, RunnerIdentity,
    VerifierEvidence, VerifierObservation,
  },
  policy::{EnvironmentMode, ProjectConfig, VerifierAuthority, VerifierSpec},
};

fn content(label: &str) -> ContentObjectId {
  ContentObjectId(format!("sha256:{label:0<64}"))
}

fn authority_id() -> AuthorityId {
  AuthorityId(content("authority"))
}

fn candidate_id() -> CandidateId {
  CandidateId(content("candidate"))
}

fn snapshot_oracle(path: &str, object: &str) -> OracleIdentity {
  OracleIdentity::AuthoritySnapshot {
    verifier_id: path.into(),
    authority_id: authority_id(),
    bundle_path: path.into(),
    bundle_content_id: content(object),
    executable_content_id: content(&format!("executable-{object}")),
    definition_digest: format!("sha256:{path}"),
  }
}

fn project_oracle() -> OracleIdentity {
  OracleIdentity::Project {
    verifier_id: "quality".into(),
    candidate_id: candidate_id(),
    definition_digest: "sha256:quality".into(),
  }
}

fn verifier(id: &str, authority: VerifierAuthority, oracle_path: Option<&str>) -> VerifierSpec {
  VerifierSpec {
    id: id.into(),
    argv: vec!["true".into()],
    cwd: ".".into(),
    timeout_seconds: 1,
    max_output_bytes: 1024,
    env: Default::default(),
    environment_mode: EnvironmentMode::Ambient,
    authority,
    oracle_path: oracle_path.map(str::to_owned),
  }
}

fn execution(mode: EnvironmentMode) -> ExecutionProvenance {
  ExecutionProvenance {
    runner_identity: RunnerIdentity("tenet.local_process_runner.v1".into()),
    tenet_version: "0.1.0".into(),
    os: "test-os".into(),
    architecture: "test-arch".into(),
    environment_mode: mode,
    execution_environment_identity: ExecutionEnvironmentIdentity("sha256:execution".into()),
  }
}

fn verifier_evidence(
  verifier_id: &str,
  authority: ArtifactAuthority,
  oracle_identity: OracleIdentity,
  effect: EvidenceEffect,
) -> VerifierEvidence {
  VerifierEvidence {
    obligation_id: ObligationId("REQ-001/VO-001".into()),
    authority_id: authority_id(),
    candidate_id: candidate_id(),
    verifier_id: verifier_id.into(),
    policy_digest: "policy".into(),
    spec_digest: "spec".into(),
    contract_digest: "contract".into(),
    authority,
    oracle_identity,
    provenance: ArtifactProvenance::TenetLocalVerifier,
    execution: execution(EnvironmentMode::Ambient),
    effect,
    validity: ArtifactValidity::Valid,
    dependency_surface: DependencySurface::CandidateSnapshot,
    observation: VerifierObservation {
      exit_code: Some(0),
      stdout: String::new(),
      stderr: String::new(),
      timed_out: false,
    },
  }
}

fn fixture(
  with_assurance: bool,
) -> (
  AdmittedContract,
  ProjectConfig,
  BTreeMap<String, OracleIdentity>,
  EvidenceArtifact,
  EvidenceArtifact,
) {
  let assurance = OracleAssuranceContract {
    id: AssuranceId("ASSURE-001".into()),
    criterion: "mutation check distinguishes a seeded defect".into(),
    verifier_id: "mutation-assurance".into(),
    authority: VerifierAuthority::AuthoritySnapshot,
  };
  let contract = AdmittedContract {
    schema_version: 1,
    proposal_id: "proposal-1".into(),
    proposal_digest: "proposal".into(),
    spec_digest: "spec".into(),
    policy_digest: "policy".into(),
    requirements: vec![Requirement {
      id: RequirementId("REQ-001".into()),
      statement: "feature exists".into(),
      obligations: vec![VerificationObligation {
        id: ObligationId("REQ-001/VO-001".into()),
        statement: "configured check passes".into(),
        evidence_contract: EvidenceContract {
          claim: ClaimEvidenceContract {
            verifier_id: "quality".into(),
            authority: VerifierAuthority::Project,
          },
          assurances: with_assurance
            .then_some(assurance.clone())
            .into_iter()
            .collect(),
        },
      }],
    }],
  };
  let policy = ProjectConfig {
    version: 1,
    spec_path: "SPEC.md".into(),
    candidate: Default::default(),
    verifiers: vec![
      verifier("quality", VerifierAuthority::Project, None),
      verifier(
        "mutation-assurance",
        VerifierAuthority::AuthoritySnapshot,
        Some("oracles/mutation"),
      ),
    ],
  };
  let assurance_oracle = snapshot_oracle("oracles/mutation", "assurance-object");
  let primary_oracle = project_oracle();
  let oracle_identities = BTreeMap::from([
    ("quality".into(), primary_oracle.clone()),
    ("mutation-assurance".into(), assurance_oracle.clone()),
  ]);
  let claim = EvidenceArtifact::Claim {
    evidence: verifier_evidence(
      "quality",
      ArtifactAuthority::TenetObservedProjectVerifier,
      primary_oracle.clone(),
      EvidenceEffect::Supports,
    ),
  };
  let assurance = EvidenceArtifact::OracleAssurance {
    assurance_id: assurance.id,
    assurance_criterion: assurance.criterion,
    qualified_oracle_identity: primary_oracle,
    evidence: verifier_evidence(
      "mutation-assurance",
      ArtifactAuthority::TenetObservedAuthoritySnapshotVerifier,
      assurance_oracle,
      EvidenceEffect::Supports,
    ),
  };
  (contract, policy, oracle_identities, claim, assurance)
}

fn derive_with(
  contract: &AdmittedContract,
  policy: &ProjectConfig,
  oracle_identities: &BTreeMap<String, OracleIdentity>,
  infrastructure_errors: &BTreeMap<String, String>,
  evidence: &[EvidenceArtifact],
) -> tenet_domain::completion::ObligationResult {
  let authority_id = authority_id();
  let candidate_id = candidate_id();
  derive_obligation_state(
    &DerivationContext {
      authority_id: &authority_id,
      candidate_id: &candidate_id,
      spec_digest: "spec",
      contract_digest: "contract",
      policy_digest: "policy",
      contract,
      policy,
      oracle_identities,
      infrastructure_errors,
    },
    &ObligationId("REQ-001/VO-001".into()),
    evidence,
  )
}

fn evidence_mut(artifact: &mut EvidenceArtifact) -> &mut VerifierEvidence {
  match artifact {
    EvidenceArtifact::Claim { evidence } | EvidenceArtifact::OracleAssurance { evidence, .. } => {
      evidence
    }
  }
}

#[test]
fn primary_support_without_assurance_satisfies_contract() {
  let (contract, policy, oracles, claim, _) = fixture(false);
  let result = derive_with(&contract, &policy, &oracles, &BTreeMap::new(), &[claim]);
  assert_eq!(result.state, ObligationState::ContractSatisfied);
  assert_eq!(derive_completion(&[result]), Verdict::Done);
}

#[test]
fn required_assurance_support_completes_primary_support() {
  let (contract, policy, oracles, claim, assurance) = fixture(true);
  let result = derive_with(
    &contract,
    &policy,
    &oracles,
    &BTreeMap::new(),
    &[claim, assurance],
  );
  assert_eq!(result.state, ObligationState::ContractSatisfied);
}

#[test]
fn assurance_evidence_cannot_substitute_for_claim_evidence() {
  let (contract, policy, oracles, _, assurance) = fixture(true);
  let result = derive_with(&contract, &policy, &oracles, &BTreeMap::new(), &[assurance]);
  assert_eq!(result.state, ObligationState::MissingEvidence);
}

#[test]
fn claim_evidence_cannot_substitute_for_assurance_evidence() {
  let (contract, policy, oracles, claim, _) = fixture(true);
  let result = derive_with(&contract, &policy, &oracles, &BTreeMap::new(), &[claim]);
  assert_eq!(result.state, ObligationState::Unverifiable);
  assert_eq!(result.blockers[0].code, BlockerCode::OracleAssuranceMissing);
}

#[test]
fn primary_contradiction_overrides_assurance_support() {
  let (contract, policy, oracles, mut claim, assurance) = fixture(true);
  evidence_mut(&mut claim).effect = EvidenceEffect::Contradicts;
  let result = derive_with(
    &contract,
    &policy,
    &oracles,
    &BTreeMap::new(),
    &[claim, assurance],
  );
  assert_eq!(result.state, ObligationState::Contradicted);
}

#[test]
fn failed_assurance_makes_supported_claim_unverifiable() {
  let (contract, policy, oracles, claim, mut assurance) = fixture(true);
  evidence_mut(&mut assurance).effect = EvidenceEffect::Contradicts;
  let result = derive_with(
    &contract,
    &policy,
    &oracles,
    &BTreeMap::new(),
    &[claim, assurance],
  );
  assert_eq!(result.state, ObligationState::Unverifiable);
  assert_eq!(derive_completion(&[result]), Verdict::NotDone);
}

#[test]
fn inconclusive_assurance_propagates_inconclusive() {
  let (contract, policy, oracles, claim, mut assurance) = fixture(true);
  evidence_mut(&mut assurance).effect = EvidenceEffect::Inconclusive;
  let result = derive_with(
    &contract,
    &policy,
    &oracles,
    &BTreeMap::new(),
    &[claim, assurance],
  );
  assert_eq!(result.state, ObligationState::Inconclusive);
}

#[test]
fn assurance_infrastructure_failure_propagates_infrastructure_error() {
  let (contract, policy, oracles, claim, _) = fixture(true);
  let errors = BTreeMap::from([("mutation-assurance".into(), "runner failed".into())]);
  let result = derive_with(&contract, &policy, &oracles, &errors, &[claim]);
  assert_eq!(result.state, ObligationState::InfrastructureError);
}

#[test]
fn assurance_infrastructure_failure_overrides_missing_primary_evidence() {
  let (contract, policy, oracles, _, _) = fixture(true);
  let errors = BTreeMap::from([("mutation-assurance".into(), "runner failed".into())]);
  let result = derive_with(&contract, &policy, &oracles, &errors, &[]);
  assert_eq!(result.state, ObligationState::InfrastructureError);
}

#[test]
fn changed_primary_oracle_invalidates_prior_assurance() {
  let (mut contract, mut policy, mut oracles, mut claim, assurance) = fixture(true);
  contract.requirements[0].obligations[0]
    .evidence_contract
    .claim = ClaimEvidenceContract {
    verifier_id: "snapshot-quality".into(),
    authority: VerifierAuthority::AuthoritySnapshot,
  };
  policy.verifiers.push(verifier(
    "snapshot-quality",
    VerifierAuthority::AuthoritySnapshot,
    Some("oracles/quality"),
  ));
  let changed_primary = snapshot_oracle("oracles/quality", "new-primary-object");
  oracles.insert("snapshot-quality".into(), changed_primary.clone());
  if let EvidenceArtifact::Claim { evidence } = &mut claim {
    evidence.verifier_id = "snapshot-quality".into();
    evidence.authority = ArtifactAuthority::TenetObservedAuthoritySnapshotVerifier;
    evidence.oracle_identity = changed_primary;
  }
  let result = derive_with(
    &contract,
    &policy,
    &oracles,
    &BTreeMap::new(),
    &[claim, assurance],
  );
  assert_eq!(result.state, ObligationState::Stale);
  assert_eq!(result.blockers[0].code, BlockerCode::OracleAssuranceStale);
}

#[test]
fn stale_primary_binding_cannot_satisfy() {
  let (contract, policy, oracles, mut claim, _) = fixture(false);
  evidence_mut(&mut claim).authority_id = AuthorityId(content("other"));
  let result = derive_with(&contract, &policy, &oracles, &BTreeMap::new(), &[claim]);
  assert_eq!(result.state, ObligationState::Stale);
}

#[test]
fn untrusted_primary_evidence_never_establishes_completion() {
  let (contract, policy, oracles, mut claim, _) = fixture(false);
  evidence_mut(&mut claim).authority = ArtifactAuthority::AgentAssertion;
  let result = derive_with(&contract, &policy, &oracles, &BTreeMap::new(), &[claim]);
  assert_eq!(result.state, ObligationState::MissingEvidence);
}

#[test]
fn empty_obligation_set_never_authorizes_completion() {
  assert_eq!(derive_completion(&[]), Verdict::NotDone);
}

proptest::proptest! {
  #[test]
  fn adding_untrusted_claim_evidence_never_upgrades_missing_to_satisfied(count in 0usize..20) {
    let (contract, policy, oracles, mut artifact, _) = fixture(false);
    evidence_mut(&mut artifact).authority = ArtifactAuthority::AgentExploratoryExecution;
    let evidence = vec![artifact; count];
    let result = derive_with(&contract, &policy, &oracles, &BTreeMap::new(), &evidence);
    proptest::prop_assert_ne!(result.state, ObligationState::ContractSatisfied);
  }
}
