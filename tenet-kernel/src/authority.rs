//! Content identities and exact admission-chain validation, independent of refs and I/O.

use tenet_domain::{
  authority::{
    Admission, AdmissionError, AdmissionId, Authority, AuthorityProposal, Clarification,
    ClarificationId, ProposalId, ReconciliationReport, ReconciliationReportId, SpecSnapshot,
    SpecSnapshotId,
  },
  evidence::{AuthorityId, ContentObjectId},
};

use crate::{digest::canonical_digest, grant::validate_grant_binding};

macro_rules! identity {
  ($function:ident, $object:ty, $id:ident) => {
    pub fn $function(value: &$object) -> Result<$id, serde_json::Error> {
      canonical_digest(value).map(|digest| $id(ContentObjectId(digest)))
    }
  };
}
identity!(spec_snapshot_id, SpecSnapshot, SpecSnapshotId);
identity!(authority_id, Authority, AuthorityId);
identity!(proposal_id, AuthorityProposal, ProposalId);
identity!(
  reconciliation_report_id,
  ReconciliationReport,
  ReconciliationReportId
);
identity!(clarification_id, Clarification, ClarificationId);
identity!(admission_id, Admission, AdmissionId);

pub fn validate_admission(
  admission: &Admission,
  proposal: &AuthorityProposal,
  report: &ReconciliationReport,
  authority: &Authority,
  spec: &SpecSnapshot,
) -> Result<(), AdmissionError> {
  if [
    admission.schema_version,
    proposal.schema_version,
    report.schema_version,
    authority.schema_version,
    spec.schema_version,
  ]
  .iter()
  .any(|version| *version != 1)
  {
    return Err(AdmissionError::UnsupportedVersion);
  }
  if !proposal_id(proposal).is_ok_and(|id| id == admission.proposal) {
    return Err(AdmissionError::ProposalMismatch);
  }
  if report.proposal != admission.proposal {
    return Err(AdmissionError::ReconciliationProposalMismatch);
  }
  if !reconciliation_report_id(report).is_ok_and(|id| id == admission.reconciliation) {
    return Err(AdmissionError::ReconciliationMismatch);
  }
  if proposal.authority != admission.authority
    || !authority_id(authority).is_ok_and(|id| id == admission.authority)
  {
    return Err(AdmissionError::AuthorityMismatch);
  }
  if !spec_snapshot_id(spec).is_ok_and(|id| id == authority.spec) {
    return Err(AdmissionError::SpecificationMismatch);
  }
  if proposal.issues.iter().any(|issue| issue.blocking)
    || report.findings.iter().any(|finding| finding.blocking)
  {
    return Err(AdmissionError::BlockingFindings);
  }
  validate_grant_binding(&admission.grant, &admission.proposal, &admission.authority)
    .map_err(|_| AdmissionError::GrantBindingInvalid)?;
  Ok(())
}
