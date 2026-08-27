use tenet_domain::{
  completion::{
    derive_completion, derive_obligation_state, DerivationContext, ObligationState, Verdict,
  },
  contract::{
    AdmittedContract, EvidenceContract, ObligationId, Requirement, RequirementId,
    VerificationObligation,
  },
  evidence::{
    ArtifactAuthority, ArtifactProvenance, ArtifactValidity, DependencySurface, EvidenceArtifact,
    EvidenceEffect, VerifierObservation,
  },
  policy::{RepositoryConfig, VerifierAuthority, VerifierSpec},
};

fn fixture() -> (AdmittedContract, RepositoryConfig, EvidenceArtifact) {
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
          verifier_id: "quality".into(),
        },
      }],
    }],
  };
  let policy = RepositoryConfig {
    version: 1,
    spec_path: "SPEC.md".into(),
    verifiers: vec![VerifierSpec {
      id: "quality".into(),
      argv: vec!["true".into()],
      cwd: ".".into(),
      timeout_seconds: 1,
      max_output_bytes: 1024,
      env: Default::default(),
      authority: VerifierAuthority::Project,
    }],
  };
  let evidence = EvidenceArtifact {
    obligation_id: ObligationId("REQ-001/VO-001".into()),
    revision: "revision".into(),
    verifier_id: "quality".into(),
    policy_digest: "policy".into(),
    spec_digest: "spec".into(),
    contract_digest: "contract".into(),
    authority: ArtifactAuthority::TenetObservedProjectVerifier,
    provenance: ArtifactProvenance::TenetLocalVerifier,
    effect: EvidenceEffect::Supports,
    validity: ArtifactValidity::Valid,
    dependency_surface: DependencySurface::RepositoryWide,
    observation: VerifierObservation {
      exit_code: Some(0),
      stdout: String::new(),
      stderr: String::new(),
      timed_out: false,
    },
  };
  (contract, policy, evidence)
}

fn derive(evidence: &[EvidenceArtifact]) -> tenet_domain::completion::ObligationResult {
  let (contract, policy, _) = fixture();
  derive_obligation_state(
    &DerivationContext {
      revision: "revision",
      spec_digest: "spec",
      contract_digest: "contract",
      policy_digest: "policy",
      contract: &contract,
      policy: &policy,
    },
    &ObligationId("REQ-001/VO-001".into()),
    evidence,
  )
}

#[test]
fn supporting_authoritative_evidence_satisfies_contract() {
  let (_, _, evidence) = fixture();
  let result = derive(&[evidence]);
  assert_eq!(result.state, ObligationState::ContractSatisfied);
  assert_eq!(derive_completion(&[result]), Verdict::Done);
}

#[test]
fn missing_evidence_fails_closed() {
  let result = derive(&[]);
  assert_eq!(result.state, ObligationState::MissingEvidence);
  assert_eq!(derive_completion(&[result]), Verdict::NotDone);
}

#[test]
fn empty_obligation_set_never_authorizes_completion() {
  assert_eq!(derive_completion(&[]), Verdict::NotDone);
}
#[test]

fn contradiction_overrides_support() {
  let (_, _, support) = fixture();
  let mut contradiction = support.clone();
  contradiction.effect = EvidenceEffect::Contradicts;
  let result = derive(&[support, contradiction]);
  assert_eq!(result.state, ObligationState::Contradicted);
  assert_eq!(derive_completion(&[result]), Verdict::NotDone);
}

#[test]
fn stale_and_mismatched_bindings_cannot_satisfy() {
  let (_, _, mut evidence) = fixture();
  evidence.revision = "other".into();
  let revision = derive(&[evidence.clone()]);
  evidence.revision = "revision".into();
  evidence.spec_digest = "other".into();
  let specification = derive(&[evidence.clone()]);
  evidence.spec_digest = "spec".into();
  evidence.policy_digest = "other".into();
  let policy = derive(&[evidence]);
  assert_eq!(revision.state, ObligationState::Stale);
  assert_eq!(specification.state, ObligationState::Stale);
  assert_eq!(policy.state, ObligationState::Stale);
}

#[test]
fn agent_assertion_never_establishes_completion() {
  let (_, _, mut evidence) = fixture();
  evidence.authority = ArtifactAuthority::AgentAssertion;
  assert_eq!(derive(&[evidence]).state, ObligationState::MissingEvidence);
}

#[test]
fn agent_reported_provenance_never_establishes_completion() {
  let (_, _, mut evidence) = fixture();
  evidence.provenance = ArtifactProvenance::AgentReported;
  assert_eq!(derive(&[evidence]).state, ObligationState::MissingEvidence);
}

proptest::proptest! {
  #[test]
  fn adding_untrusted_evidence_never_upgrades_missing_to_satisfied(count in 0usize..20) {
    let (_, _, mut artifact) = fixture();
    artifact.authority = ArtifactAuthority::AgentExploratoryExecution;
    let evidence = vec![artifact; count];
    proptest::prop_assert_ne!(derive(&evidence).state, ObligationState::ContractSatisfied);
  }
}
