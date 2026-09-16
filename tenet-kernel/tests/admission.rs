use tenet_domain::{authority::*, evidence::ContentObjectId};
use tenet_kernel::{authority::*, grant};

fn fixture() -> (
  SpecSnapshot,
  Authority,
  AuthorityProposal,
  ReconciliationReport,
  Admission,
) {
  let spec = SpecSnapshot {
    schema_version: 1,
    path: "SPEC.md".into(),
    content: b"S1".to_vec(),
  };
  let authority = Authority {
    schema_version: 1,
    spec: spec_snapshot_id(&spec).unwrap(),
    contract: ContentObjectId(format!("sha256:{}", "c".repeat(64))),
    surface: ContentObjectId(format!("sha256:{}", "a".repeat(64))),
  };
  let proposal = AuthorityProposal {
    schema_version: 1,
    authority: authority_id(&authority).unwrap(),
    issues: vec![],
  };
  let report = ReconciliationReport {
    schema_version: 1,
    proposal: proposal_id(&proposal).unwrap(),
    findings: vec![],
  };
  let admission = admission(&proposal, &report);
  (spec, authority, proposal, report, admission)
}

fn admission(proposal: &AuthorityProposal, report: &ReconciliationReport) -> Admission {
  let proposal_id = proposal_id(proposal).unwrap();
  Admission {
    schema_version: 1,
    proposal: proposal_id.clone(),
    reconciliation: reconciliation_report_id(report).unwrap(),
    authority: proposal.authority.clone(),
    grant: grant::mint_grant(
      b"s".repeat(48).as_slice(),
      &proposal_id,
      &proposal.authority,
    )
    .unwrap(),
  }
}

#[test]
fn exact_chain_is_required_for_admission() {
  let (spec, authority, proposal, report, admission) = fixture();
  assert_eq!(
    validate_admission(&admission, &proposal, &report, &authority, &spec),
    Ok(())
  );
}

#[test]
fn p1_reconciliation_cannot_reconcile_p2_after_authority_mutation() {
  let (spec, mut authority, mut proposal, report, _) = fixture();
  authority.surface = ContentObjectId(format!("sha256:{}", "b".repeat(64)));
  proposal.authority = authority_id(&authority).unwrap();
  let admission = admission(&proposal, &report);
  assert_eq!(
    validate_admission(&admission, &proposal, &report, &authority, &spec),
    Err(AdmissionError::ReconciliationProposalMismatch)
  );
}

#[test]
fn admission_for_a1_cannot_admit_a2() {
  let (spec, mut authority, mut proposal, report, admission) = fixture();
  authority.surface = ContentObjectId(format!("sha256:{}", "b".repeat(64)));
  assert_eq!(
    validate_admission(&admission, &proposal, &report, &authority, &spec),
    Err(AdmissionError::AuthorityMismatch)
  );
  proposal.authority = authority_id(&authority).unwrap();
  assert_eq!(
    validate_admission(&admission, &proposal, &report, &authority, &spec),
    Err(AdmissionError::ProposalMismatch)
  );
}

#[test]
fn admission_cannot_substitute_a_different_reconciliation_report() {
  let (spec, authority, proposal, mut report, admission) = fixture();
  report.findings.push(Finding {
    code: "note".into(),
    message: "reviewed".into(),
    blocking: false,
  });
  assert_eq!(
    validate_admission(&admission, &proposal, &report, &authority, &spec),
    Err(AdmissionError::ReconciliationMismatch)
  );
}

#[test]
fn blocking_findings_and_issues_prevent_admission() {
  let (spec, authority, mut proposal, mut report, _) = fixture();
  report.findings.push(Finding {
    code: "gap".into(),
    message: "missing requirement".into(),
    blocking: true,
  });
  assert_eq!(
    validate_admission(
      &admission(&proposal, &report),
      &proposal,
      &report,
      &authority,
      &spec
    ),
    Err(AdmissionError::BlockingFindings)
  );
  report.findings.clear();
  proposal.issues.push(Issue {
    code: "question".into(),
    message: "unresolved ambiguity".into(),
    blocking: true,
  });
  report.proposal = proposal_id(&proposal).unwrap();
  assert_eq!(
    validate_admission(
      &admission(&proposal, &report),
      &proposal,
      &report,
      &authority,
      &spec
    ),
    Err(AdmissionError::BlockingFindings)
  );
}

#[test]
fn specification_mutation_cannot_reinterpret_admission() {
  let (mut spec, authority, proposal, report, admission) = fixture();
  spec.content = b"S2".to_vec();
  assert_eq!(
    validate_admission(&admission, &proposal, &report, &authority, &spec),
    Err(AdmissionError::SpecificationMismatch)
  );
}

#[test]
fn unknown_lifecycle_version_fails_closed() {
  let (spec, authority, mut proposal, mut report, _) = fixture();
  proposal.schema_version = 2;
  report.proposal = proposal_id(&proposal).unwrap();
  assert_eq!(
    validate_admission(
      &admission(&proposal, &report),
      &proposal,
      &report,
      &authority,
      &spec
    ),
    Err(AdmissionError::UnsupportedVersion)
  );
}
