use std::{
  collections::{BTreeMap, BTreeSet},
  path::{Path, PathBuf},
  sync::Arc,
};

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tenet_domain::{
  algebra::{
    AssuranceProfileId, CompletionContractV1, CompletionState, Evaluation, EvaluationId,
    EvaluationScope, EvidenceResult, ExecutionContext, ExecutionObservation, LOCAL_V1,
    PROTECTED_V1, PlatformInformation, RUNNER_SEMANTICS_V1, RunnerSemanticsId, VerifierId,
    VerifierMaterial,
  },
  authority::{
    Admission, AdmissionError, AdmissionGrant, AdmissionId, Authority, AuthorityProposal,
    Clarification, ClarificationId, Finding, Issue, ProposalId, ReconciliationReport,
    ReconciliationReportId, SpecSnapshot, SpecSnapshotId,
  },
  completion::Verdict,
  contract::RequirementId,
  evidence::{
    AuthorityId, CandidateId, ContentObjectId, ExecutionEnvironmentIdentity, ExecutionProvenance,
    OracleIdentity, RunnerIdentity,
  },
  paths::{CONTRACT_PATH, SKILL_PATH},
  policy::{
    CommandCwd, PolicyError, VerificationPolicy, VerifierAuthority, VerifierProtection,
    VerifierSpec,
  },
  protocol::{ContextFacts, WorkflowPhase},
};
use tenet_kernel::{
  algebra::{AdmissionChain, evaluate, validate_completion_admission, validate_contract},
  authority::validate_admission,
  digest::{bytes_digest, canonical_digest},
  grant, identity,
  policy::validate_candidate_surface,
  protocol::derive_phase,
};

use crate::{
  ports::{
    ContentStoreError, ExecutedVerifier, ExpectedEntry, InitObservation, PathResolutionError,
    Repository, VerifierRun, VerifierRunner,
  },
  response::{
    ActiveAdmissionInspection, AuthoringReadiness, AuthorityInspectionResult,
    AuthoritySubmissionResult, Blocker, BlockersResult, ContextResult, DoctorCheck, DoctorResult,
    EvidenceReport, InitResult, ProposalInspection, ReceiptVerificationResult,
    ReconciliationInspection, RequirementCheckResult, RequirementStatus, TenetError, VerifyResult,
  },
};

const PROPOSAL_REF: &str = "proposal";
const RECONCILIATION_REF: &str = "reconciliation";
const ACTIVE_ADMISSION_REF: &str = "active-admission";
const FINAL_REF: &str = "final";
const REQUIREMENT_REFS: &str = "requirements";

#[derive(Clone)]
pub struct Tenet {
  cwd: PathBuf,
  repository: Arc<dyn Repository>,
  runner: Arc<dyn VerifierRunner>,
  admission_secret: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InitializeRequest {
  pub spec_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(
  tag = "stage",
  rename_all = "SCREAMING_SNAKE_CASE",
  rename_all_fields = "camelCase",
  deny_unknown_fields
)]
pub enum AuthoritySubmitRequest {
  Proposal {
    contract: CompletionContractV1,
    #[serde(default)]
    issues: Vec<Issue>,
  },
  Reconciliation {
    proposal_id: ProposalId,
    #[serde(default)]
    findings: Vec<Finding>,
  },
  Clarification {
    proposal_id: ProposalId,
    clarification: String,
  },
  Admission {
    proposal_id: ProposalId,
    reconciliation_id: ReconciliationReportId,
    authority_id: AuthorityId,
    #[schemars(
      description = "Trusted admission grant bound to the exact proposal and authority identities. The producer cannot mint this capability without the trusted secret."
    )]
    grant: AdmissionGrant,
  },
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementCheckRequest {
  pub requirement_id: RequirementId,
}

struct LoadedAuthority {
  authority: Authority,
  spec: SpecSnapshot,
  policy: VerificationPolicy,
  contract: CompletionContractV1,
  surface_entries: Vec<tenet_domain::snapshot::TreeEntry>,
}

struct LoadedAdmission {
  id: AdmissionId,
  admission: Admission,
  proposal: AuthorityProposal,
  report: ReconciliationReport,
  loaded: LoadedAuthority,
}

pub type AppResult<T> = std::result::Result<T, TenetError>;

fn public_error(error: anyhow::Error) -> TenetError {
  if let Some(typed) = error
    .chain()
    .find_map(|cause| cause.downcast_ref::<TenetError>().cloned())
  {
    return typed;
  }
  if let Some(path_error) = error
    .chain()
    .find_map(|cause| cause.downcast_ref::<PathResolutionError>())
  {
    return TenetError::new(path_error_code(path_error), path_error.to_string());
  }
  if let Some(content_error) = error
    .chain()
    .find_map(|cause| cause.downcast_ref::<ContentStoreError>())
  {
    return TenetError::new(content_error_code(content_error), content_error.to_string());
  }
  if let Some(policy_error) = error
    .chain()
    .find_map(|cause| cause.downcast_ref::<PolicyError>())
  {
    return TenetError::new("policy_invalid", policy_error.to_string());
  }
  if let Some(admission_error) = error
    .chain()
    .find_map(|cause| cause.downcast_ref::<AdmissionError>())
  {
    return TenetError::new("admission_invalid", admission_error.to_string());
  }
  if let Some(algebra_error) = error
    .chain()
    .find_map(|cause| cause.downcast_ref::<tenet_domain::algebra::AlgebraError>())
  {
    return TenetError::new("semantics_incompatible", algebra_error.to_string());
  }
  TenetError::new("internal_error", error.to_string())
}

fn path_error_code(error: &PathResolutionError) -> &'static str {
  match error {
    PathResolutionError::UnsupportedSymlink { .. } => "unsupported_symlink",
    PathResolutionError::PathEscape { .. } | PathResolutionError::Invalid => "path_escape",
    PathResolutionError::Missing { .. } => "path_missing",
    PathResolutionError::NotDirectory { .. } => "path_not_directory",
    PathResolutionError::NotFile { .. } => "path_not_file",
    PathResolutionError::Special { .. } => "unsupported_filesystem_entry",
    PathResolutionError::Io { .. } => "internal_error",
  }
}

fn content_error_code(error: &ContentStoreError) -> &'static str {
  match error {
    ContentStoreError::Missing { .. } => "content_object_missing",
    ContentStoreError::Integrity { .. } => "content_integrity_failure",
    ContentStoreError::Materialization { .. }
    | ContentStoreError::MaterializationMessage { .. } => "content_materialization_failed",
  }
}

fn app_result<T>(result: Result<T>) -> AppResult<T> {
  result.map_err(public_error)
}

impl Tenet {
  pub fn new(
    cwd: PathBuf,
    repository: Arc<dyn Repository>,
    runner: Arc<dyn VerifierRunner>,
    admission_secret: Option<Vec<u8>>,
  ) -> Self {
    Self {
      cwd,
      repository,
      runner,
      admission_secret,
    }
  }

  /// The trusted admission secret, when the operator process provides one.
  /// Admitting or minting an authority without it fails closed; the secret is
  /// never persisted, logged, or returned.
  pub fn has_admission_secret(&self) -> bool {
    self.admission_secret.is_some()
  }

  fn trusted_admission_secret(&self) -> Result<&[u8]> {
    self.admission_secret.as_deref().ok_or_else(|| {
      anyhow::Error::from(TenetError::new(
        "admission_secret_unavailable",
        "authority admission requires the trusted admission secret in this process",
      ))
    })
  }

  /// Mint a trusted admission grant for one exact proposal/authority pair.
  /// Only a process possessing the trusted secret can produce a valid grant;
  /// the candidate producer normally cannot.
  pub fn mint_admission_grant(
    &self,
    proposal_id: &ProposalId,
    authority_id: &AuthorityId,
  ) -> AppResult<AdmissionGrant> {
    let secret = self.admission_secret.as_deref().ok_or_else(|| {
      TenetError::new(
        "admission_secret_unavailable",
        "grant minting requires the trusted admission secret in this process",
      )
    })?;
    grant::mint_grant(secret, proposal_id, authority_id)
      .map_err(|error| TenetError::new("admission_grant_invalid", error.to_string()))
  }

  pub fn initialize(&self, request: &InitializeRequest) -> AppResult<InitResult> {
    app_result(self.initialize_inner(request.spec_path.as_deref()))
  }

  pub fn context(&self) -> AppResult<ContextResult> {
    app_result((|| {
      let root = self.initialized_root()?;
      let _lock = self.repository.acquire_lock(&root)?;
      self.context_inner()
    })())
  }

  pub fn authority_submit(
    &self,
    request: AuthoritySubmitRequest,
  ) -> AppResult<AuthoritySubmissionResult> {
    app_result((|| {
      let root = self.initialized_root()?;
      let _lock = self.repository.acquire_lock(&root)?;
      self.authority_submit_inner(request)
    })())
  }

  pub fn requirement_check(
    &self,
    request: &RequirementCheckRequest,
  ) -> AppResult<RequirementCheckResult> {
    app_result((|| {
      let root = self.initialized_root()?;
      let _lock = self.repository.acquire_lock(&root)?;
      self.requirement_check_inner(request)
    })())
  }

  pub fn verify(&self) -> AppResult<VerifyResult> {
    app_result((|| {
      let root = self.initialized_root()?;
      let _lock = self.repository.acquire_lock(&root)?;
      self.verify_inner()
    })())
  }

  pub fn receipt_verify(&self, receipt_id: &EvaluationId) -> AppResult<ReceiptVerificationResult> {
    app_result((|| {
      let root = self.initialized_root()?;
      let _lock = self.repository.acquire_lock(&root)?;
      self.receipt_verify_inner(receipt_id)
    })())
  }

  pub fn doctor(&self) -> AppResult<DoctorResult> {
    app_result((|| {
      let root = self.initialized_root()?;
      let _lock = self.repository.acquire_lock(&root)?;
      self.doctor_inner()
    })())
  }

  /// Inspect the exact authority lifecycle state: proposal, reconciliation,
  /// and the active admitted chain. Identical through every adapter.
  pub fn authority_inspect(&self) -> AppResult<AuthorityInspectionResult> {
    app_result((|| {
      let root = self.initialized_root()?;
      let _lock = self.repository.acquire_lock(&root)?;
      self.authority_inspect_inner(&root)
    })())
  }

  /// Derive the current blocking items from persisted facts and the derived
  /// phase. Informational; cannot establish or deny completion.
  pub fn blockers(&self) -> AppResult<BlockersResult> {
    let context = self.context()?;
    Ok(blockers_from_context(&context))
  }

  /// Read persisted evidence for the final or one requirement-scoped
  /// Evaluation and re-derive its kernel evaluation. Evidence is never a
  /// completion decision.
  pub fn evidence(&self, requirement_id: Option<&RequirementId>) -> AppResult<EvidenceReport> {
    app_result((|| {
      let root = self.initialized_root()?;
      let _lock = self.repository.acquire_lock(&root)?;
      self.evidence_inner(&root, requirement_id)
    })())
  }

  fn authority_inspect_inner(&self, root: &Path) -> Result<AuthorityInspectionResult> {
    let proposal = self
      .repository
      .read_ref(root, PROPOSAL_REF)?
      .map(|id| {
        let (proposal, _loaded) = self.load_proposed(root, &ProposalId(id.clone()))?;
        Ok::<_, anyhow::Error>(ProposalInspection {
          proposal_id: ProposalId(id),
          authority_id: proposal.authority,
          issues: proposal.issues,
        })
      })
      .transpose()?;
    let reconciliation = self
      .repository
      .read_ref(root, RECONCILIATION_REF)?
      .map(|id| {
        let report: ReconciliationReport = self.load_object(root, &id)?;
        Ok::<_, anyhow::Error>(ReconciliationInspection {
          reconciliation_id: ReconciliationReportId(id),
          proposal_id: report.proposal,
          findings: report.findings,
        })
      })
      .transpose()?;
    let active = self
      .repository
      .read_ref(root, ACTIVE_ADMISSION_REF)?
      .map(|id| {
        let chain = self.load_admission(root, &AdmissionId(id.clone()))?;
        Ok::<_, anyhow::Error>(ActiveAdmissionInspection {
          admission_id: AdmissionId(id),
          authority_id: chain.admission.authority,
          spec_path: chain.loaded.spec.path,
          spec_digest: bytes_digest(&chain.loaded.spec.content),
          contract_digest: chain.loaded.authority.contract,
          completion_policy_id: chain.loaded.contract.policy,
          verifiers: chain
            .loaded
            .policy
            .verifiers
            .iter()
            .map(|verifier| verifier.id.clone())
            .collect(),
          grant_proposal: chain.admission.grant.proposal,
          grant_authority: chain.admission.grant.authority,
        })
      })
      .transpose()?;
    Ok(AuthorityInspectionResult {
      schema_version: 1,
      proposal,
      reconciliation,
      active,
    })
  }

  fn evidence_inner(
    &self,
    root: &Path,
    requirement_id: Option<&RequirementId>,
  ) -> Result<EvidenceReport> {
    let chain = self.load_active(root)?;
    let id = match requirement_id {
      Some(requirement) => self
        .repository
        .read_ref(root, &requirement_ref_name(requirement)?)?
        .ok_or_else(|| {
          TenetError::new(
            "evidence_missing",
            format!(
              "no persisted Evaluation for requirement `{}`",
              requirement.0
            ),
          )
        })?,
      None => self
        .repository
        .read_ref(root, FINAL_REF)?
        .ok_or_else(|| TenetError::new("evidence_missing", "no persisted Final Evaluation"))?,
    };
    let evaluation: Evaluation = self.load_object(root, &id)?;
    if evaluation.admission != chain.id
      || evaluation.authority != chain.admission.authority
      || evaluation.candidate.0.0.trim().is_empty()
    {
      return Err(
        TenetError::new(
          "evidence_stale",
          "persisted Evaluation does not belong to the active admission",
        )
        .into(),
      );
    }
    let result = evaluate(
      &chain.loaded.contract,
      &chain.as_kernel_chain(),
      &evaluation,
    )?;
    Ok(EvidenceReport {
      schema_version: 1,
      evaluation_id: EvaluationId(id),
      evaluation,
      result,
    })
  }

  fn initialize_inner(&self, spec: Option<&Path>) -> Result<InitResult> {
    let InitObservation {
      policy,
      spec_digest,
      created,
      ..
    } = self.repository.initialize(&self.cwd, spec)?;
    Ok(InitResult {
      schema_version: 1,
      initialized: true,
      created,
      spec_path: policy.spec_path,
      spec_digest,
      skill_path: SKILL_PATH.into(),
    })
  }

  fn context_inner(&self) -> Result<ContextResult> {
    let root = self.initialized_root()?;
    let admission_ref = self.repository.read_ref(&root, ACTIVE_ADMISSION_REF)?;
    let mut compatible = true;
    let chain = if admission_ref.is_some() {
      match self.load_active(&root) {
        Ok(chain) => Some(chain),
        Err(_) => {
          compatible = false;
          None
        }
      }
    } else {
      None
    };

    let live_policy = if chain.is_none() {
      match self.repository.load_policy(&root) {
        Ok(policy) => Some(policy),
        Err(_) => {
          compatible = false;
          None
        }
      }
    } else {
      None
    };
    let spec_path = chain
      .as_ref()
      .map(|chain| chain.loaded.spec.path.as_str())
      .or_else(|| live_policy.as_ref().map(|policy| policy.spec_path.as_str()));
    let spec_exists = spec_path.is_some_and(|path| {
      self
        .repository
        .resolve_relative_path(&root, path, ExpectedEntry::File)
        .is_ok()
    });
    if !spec_exists && compatible {
      return Ok(context_for_phase(
        WorkflowPhase::SpecRequired,
        None,
        None,
        None,
        None,
        vec![],
        None,
      ));
    }

    let proposal_id = self.repository.read_ref(&root, PROPOSAL_REF)?;
    let reconciliation_id = self.repository.read_ref(&root, RECONCILIATION_REF)?;
    let proposal = if let Some(chain) = &chain {
      Some(chain.proposal.clone())
    } else {
      match proposal_id
        .as_ref()
        .map(|id| {
          self
            .load_proposed(&root, &ProposalId(id.clone()))
            .map(|(proposal, _)| proposal)
        })
        .transpose()
      {
        Ok(value) => value,
        Err(_) => {
          compatible = false;
          None
        }
      }
    };
    let report = if let Some(chain) = &chain {
      Some(chain.report.clone())
    } else {
      match reconciliation_id
        .as_ref()
        .map(|id| self.load_object::<ReconciliationReport>(&root, id))
        .transpose()
      {
        Ok(Some(report)) if report.schema_version == 1 => Some(report),
        Ok(None) => None,
        Ok(Some(_)) | Err(_) => {
          compatible = false;
          None
        }
      }
    };
    if let (Some(proposal_id), Some(report)) = (&proposal_id, &report)
      && report.proposal.0 != *proposal_id
      && chain.is_none()
    {
      compatible = false;
    }

    let authority_current = chain
      .as_ref()
      .map(|chain| self.spec_is_current(&root, &chain.loaded.spec))
      .transpose()?
      .unwrap_or(false);
    let current_candidate = if authority_current {
      chain
        .as_ref()
        .map(|chain| self.capture_candidate(&root, &chain.loaded.policy))
        .transpose()?
    } else {
      None
    };

    let mut requirement_checks = Vec::new();
    if let Some(chain) = &chain {
      for (_, id) in self.repository.list_refs(&root, REQUIREMENT_REFS)? {
        let evaluation: Evaluation = match self.load_object(&root, &id) {
          Ok(value) => value,
          Err(_) => {
            compatible = false;
            continue;
          }
        };
        let EvaluationScope::Requirement { requirement } = &evaluation.scope else {
          compatible = false;
          continue;
        };
        if evaluation.admission != chain.id || evaluation.authority != chain.admission.authority {
          continue;
        }
        match evaluate(
          &chain.loaded.contract,
          &chain.as_kernel_chain(),
          &evaluation,
        ) {
          Ok(result) => requirement_checks.push(RequirementStatus {
            requirement_id: requirement.clone(),
            evaluation_id: EvaluationId(id),
            candidate_id: evaluation.candidate,
            state: result.state,
          }),
          Err(_) => compatible = false,
        }
      }
    }
    requirement_checks.sort_by(|left, right| left.requirement_id.0.cmp(&right.requirement_id.0));

    let mut final_satisfied = false;
    let mut current_matches_final = false;
    if let (Some(chain), Some(final_id)) =
      (chain.as_ref(), self.repository.read_ref(&root, FINAL_REF)?)
    {
      match self.load_object::<Evaluation>(&root, &final_id) {
        Ok(evaluation)
          if evaluation.admission == chain.id
            && evaluation.authority == chain.admission.authority
            && matches!(evaluation.scope, EvaluationScope::Final) =>
        {
          match evaluate(
            &chain.loaded.contract,
            &chain.as_kernel_chain(),
            &evaluation,
          ) {
            Ok(result) => {
              final_satisfied = result.state == CompletionState::Satisfied;
              current_matches_final = current_candidate.as_ref() == Some(&evaluation.candidate);
            }
            Err(_) => compatible = false,
          }
        }
        Ok(_) => {}
        Err(_) => compatible = false,
      }
    }

    let facts = ContextFacts {
      spec_exists,
      compatible,
      proposal_exists: proposal.is_some(),
      reconciliation_exists: report.is_some(),
      reconciliation_blocked: proposal
        .as_ref()
        .is_some_and(|proposal| proposal.issues.iter().any(|issue| issue.blocking))
        || report
          .as_ref()
          .is_some_and(|report| report.findings.iter().any(|finding| finding.blocking)),
      admission_exists: chain.is_some(),
      authority_current,
      final_satisfied,
      current_matches_final,
    };
    let phase = derive_phase(facts);
    let authoring = (phase == WorkflowPhase::AuthorityRequired)
      .then_some(live_policy.as_ref())
      .flatten()
      .map(authoring_readiness);
    Ok(context_for_phase(
      phase,
      chain.as_ref().map(|chain| chain.id.clone()),
      chain
        .as_ref()
        .map(|chain| chain.admission.authority.clone()),
      chain
        .as_ref()
        .map(|chain| chain.loaded.contract.policy.clone()),
      current_candidate,
      requirement_checks,
      authoring,
    ))
  }

  fn authority_submit_inner(
    &self,
    request: AuthoritySubmitRequest,
  ) -> Result<AuthoritySubmissionResult> {
    let root = self.initialized_root()?;
    match request {
      AuthoritySubmitRequest::Proposal { contract, issues } => {
        let policy = self.repository.load_policy(&root)?;
        validate_candidate_surface(&policy.candidate)?;
        validate_contract(&contract)?;
        self.validate_contract_policy(&contract, &policy)?;
        self.validate_authority_sources(&root, &policy)?;
        let spec_digest = self.repository.specification_digest(&root, &policy)?;
        let stage = self
          .repository
          .stage_authority_surface(&root, &policy, &contract)?;
        let staged_policy = self.repository.load_policy(stage.path())?;
        let spec_path = self.repository.resolve_relative_path(
          stage.path(),
          &policy.spec_path,
          ExpectedEntry::File,
        )?;
        let spec = SpecSnapshot {
          schema_version: 1,
          path: policy.spec_path.clone(),
          content: self.repository.read_file(&spec_path)?,
        };
        if bytes_digest(&spec.content) != spec_digest || staged_policy != policy {
          return Err(
            TenetError::new(
              "authority_surface_changed",
              "authority changed during proposal capture",
            )
            .into(),
          );
        }
        self.validate_authority_sources(stage.path(), &staged_policy)?;
        let spec_id = SpecSnapshotId(self.store_value(&root, &spec)?);
        let contract_id = self.store_value(&root, &contract)?;
        let authority = Authority {
          schema_version: 1,
          spec: spec_id,
          contract: contract_id,
          surface: self.repository.capture(&root, stage.path())?,
        };
        let authority_id = AuthorityId(self.store_value(&root, &authority)?);
        let proposal = AuthorityProposal {
          schema_version: 1,
          authority: authority_id.clone(),
          issues,
        };
        let proposal_id = ProposalId(self.store_value(&root, &proposal)?);
        self
          .repository
          .write_ref(&root, PROPOSAL_REF, &proposal_id.0)?;
        self.repository.remove_ref(&root, RECONCILIATION_REF)?;
        Ok(AuthoritySubmissionResult::Proposal {
          schema_version: 1,
          proposal_id,
          authority_id,
          proposal,
          contract,
        })
      }
      AuthoritySubmitRequest::Reconciliation {
        proposal_id,
        findings,
      } => {
        self.load_proposed(&root, &proposal_id)?;
        let report = ReconciliationReport {
          schema_version: 1,
          proposal: proposal_id,
          findings,
        };
        let reconciliation_id = ReconciliationReportId(self.store_value(&root, &report)?);
        self
          .repository
          .write_ref(&root, RECONCILIATION_REF, &reconciliation_id.0)?;
        Ok(AuthoritySubmissionResult::Reconciliation {
          schema_version: 1,
          reconciliation_id,
          report,
        })
      }
      AuthoritySubmitRequest::Clarification {
        proposal_id,
        clarification,
      } => {
        self.load_proposed(&root, &proposal_id)?;
        if clarification.trim().is_empty() {
          return Err(
            TenetError::new("clarification_invalid", "clarification must not be blank").into(),
          );
        }
        let clarification = Clarification {
          schema_version: 1,
          proposal: proposal_id,
          clarification,
        };
        let clarification_id = ClarificationId(self.store_value(&root, &clarification)?);
        Ok(AuthoritySubmissionResult::Clarification {
          schema_version: 1,
          clarification_id,
          clarification,
        })
      }
      AuthoritySubmitRequest::Admission {
        proposal_id,
        reconciliation_id,
        authority_id,
        grant,
      } => {
        let secret = self.trusted_admission_secret()?;
        grant::verify_grant(secret, &grant, &proposal_id, &authority_id)
          .map_err(|error| TenetError::new("admission_grant_invalid", error.to_string()))?;
        let (proposal, loaded) = self.load_proposed(&root, &proposal_id)?;
        let report: ReconciliationReport = self.load_object(&root, &reconciliation_id.0)?;
        let admission = Admission {
          schema_version: 1,
          proposal: proposal_id,
          reconciliation: reconciliation_id,
          authority: authority_id.clone(),
          grant,
        };
        validate_admission(
          &admission,
          &proposal,
          &report,
          &loaded.authority,
          &loaded.spec,
        )?;
        self.require_current_spec(&root, &loaded.spec)?;
        validate_completion_admission(
          &loaded.contract,
          &AdmissionChain {
            admission: &admission,
            proposal: &proposal,
            report: &report,
            authority: &loaded.authority,
            spec: &loaded.spec,
            policy: &loaded.policy,
            surface_entries: &loaded.surface_entries,
          },
        )?;
        let admission_id = AdmissionId(self.store_value(&root, &admission)?);
        self
          .repository
          .write_ref(&root, ACTIVE_ADMISSION_REF, &admission_id.0)?;
        Ok(AuthoritySubmissionResult::Admission {
          schema_version: 1,
          admission_id,
          authority_id,
          admission,
        })
      }
    }
  }

  fn requirement_check_inner(
    &self,
    request: &RequirementCheckRequest,
  ) -> Result<RequirementCheckResult> {
    let root = self.initialized_root()?;
    // Authoritative verification loads the active Admission through the
    // trusted path: the grant mac is re-verified under the trusted secret, so
    // a process without it fails closed instead of running verifiers under a
    // possibly forged persisted authority.
    self.trusted_admission_secret()?;
    let chain = self.load_active(&root)?;
    self.require_current_spec(&root, &chain.loaded.spec)?;
    let candidate_id = self.capture_candidate(&root, &chain.loaded.policy)?;
    let scope = EvaluationScope::Requirement {
      requirement: request.requirement_id.clone(),
    };
    let evaluation = self.execute_evaluation(&root, &chain, candidate_id.clone(), scope)?;
    let evaluation_id = EvaluationId(self.store_value(&root, &evaluation)?);
    let result = evaluate(
      &chain.loaded.contract,
      &chain.as_kernel_chain(),
      &evaluation,
    )?;
    self.repository.write_ref(
      &root,
      &requirement_ref_name(&request.requirement_id)?,
      &evaluation_id.0,
    )?;
    Ok(RequirementCheckResult {
      schema_version: 1,
      admission_id: chain.id,
      authority_id: chain.admission.authority,
      candidate_id,
      evaluation_id,
      evaluation,
      result,
    })
  }

  fn verify_inner(&self) -> Result<VerifyResult> {
    let root = self.initialized_root()?;
    // `DONE` may only be derived from an Admission whose grant mac verifies
    // under the trusted secret; a process without the secret fails closed.
    self.trusted_admission_secret()?;
    let chain = self.load_active(&root)?;
    self.require_current_spec(&root, &chain.loaded.spec)?;
    let candidate_id = self.capture_candidate(&root, &chain.loaded.policy)?;
    let evaluation =
      self.execute_evaluation(&root, &chain, candidate_id.clone(), EvaluationScope::Final)?;
    let evaluation_id = EvaluationId(self.store_value(&root, &evaluation)?);
    let result = evaluate(
      &chain.loaded.contract,
      &chain.as_kernel_chain(),
      &evaluation,
    )?;
    self
      .repository
      .write_ref(&root, FINAL_REF, &evaluation_id.0)?;
    let mut verdict = result
      .verdict
      .ok_or_else(|| anyhow::anyhow!("final evaluation omitted a verdict"))?;
    let mut reason = None;
    let mut current_candidate_id = None;
    if result.state == CompletionState::Satisfied {
      let current = self.capture_candidate(&root, &chain.loaded.policy)?;
      if current != candidate_id {
        verdict = Verdict::Inconclusive;
        reason = Some("CANDIDATE_CHANGED_DURING_VERIFICATION".into());
        current_candidate_id = Some(current);
      }
    }
    Ok(VerifyResult {
      schema_version: 1,
      admission_id: chain.id,
      authority_id: chain.admission.authority,
      candidate_id,
      evaluation_id,
      verdict,
      result,
      reason,
      evaluation,
      current_candidate_id,
    })
  }

  fn receipt_verify_inner(&self, receipt_id: &EvaluationId) -> Result<ReceiptVerificationResult> {
    let root = self.initialized_root()?;
    // Receipt `DONE` re-derives completion from the historical Admission, so
    // it loads through the trusted mac-revalidating path.
    self.trusted_admission_secret()?;
    let evaluation: Evaluation = self.load_object(&root, &receipt_id.0)?;
    if !matches!(evaluation.scope, EvaluationScope::Final) {
      return Err(
        TenetError::new(
          "receipt_not_final",
          "receipt must identify a Final Evaluation",
        )
        .into(),
      );
    }
    let chain = self.load_admission(&root, &evaluation.admission)?;
    self.repository.manifest(&root, &evaluation.candidate.0)?;
    let result = evaluate(
      &chain.loaded.contract,
      &chain.as_kernel_chain(),
      &evaluation,
    )?;
    if result.verdict != Some(Verdict::Done) {
      return Err(
        TenetError::new(
          "receipt_not_complete",
          "Final Evaluation did not derive DONE",
        )
        .into(),
      );
    }
    let evidence_set_digest = ContentObjectId(canonical_digest(&evaluation.runs)?);
    let mut verification_environment_ids = evaluation
      .runs
      .iter()
      .map(|run| run.provenance.execution_environment_identity.clone())
      .collect::<Vec<_>>();
    verification_environment_ids.sort_by(|left, right| left.0.cmp(&right.0));
    verification_environment_ids.dedup();
    Ok(ReceiptVerificationResult {
      schema_version: 1,
      receipt_id: receipt_id.clone(),
      authority_id: evaluation.authority,
      candidate_id: evaluation.candidate,
      contract_digest: chain.loaded.authority.contract,
      completion_policy_id: chain.loaded.contract.policy,
      evidence_set_digest,
      verification_environment_ids,
      verdict: Verdict::Done,
    })
  }

  fn doctor_inner(&self) -> Result<DoctorResult> {
    let root = self.initialized_root()?;
    let mut checks = vec![DoctorCheck {
      name: "repository_root".into(),
      passed: true,
      detail: root.display().to_string(),
    }];

    let policy = self.repository.load_policy(&root);
    checks.push(match &policy {
      Ok(policy) => {
        match self
          .repository
          .resolve_relative_path(&root, &policy.spec_path, ExpectedEntry::File)
        {
          Ok(_) => doctor_check("specification", true, policy.spec_path.clone()),
          Err(error) => doctor_check("specification", false, error.to_string()),
        }
      }
      Err(error) => doctor_check("specification", false, error.to_string()),
    });

    let integrity = self.repository.inspect_integrity(&root);
    checks.push(match &integrity {
      Ok(observation) => doctor_check(
        "object_blob_ref_integrity",
        true,
        format!(
          "{} objects, {} blobs, {} refs",
          observation.object_count, observation.blob_count, observation.ref_count
        ),
      ),
      Err(error) => doctor_check("object_blob_ref_integrity", false, error.to_string()),
    });

    let active_ref = self.repository.read_ref(&root, ACTIVE_ADMISSION_REF);
    let active_chain = match &active_ref {
      Ok(Some(_)) => self.load_active(&root).map(Some),
      Ok(None) => Ok(None),
      Err(error) => Err(anyhow::anyhow!(error.to_string())),
    };
    checks.push(match &active_chain {
      Ok(Some(_)) => doctor_check("active_admission_chain", true, "validated"),
      Ok(None) => doctor_check("active_admission_chain", true, "not admitted"),
      Err(error) => doctor_check("active_admission_chain", false, error.to_string()),
    });

    let semantics = match (&policy, &active_chain) {
      (Ok(policy), Ok(chain)) if policy.version == 1 => chain
        .as_ref()
        .map(|chain| self.validate_persisted_evaluations(&root, chain))
        .transpose()
        .map(|count| {
          format!(
            "project format 1; {} persisted evaluations validated",
            count.unwrap_or(0)
          )
        }),
      (Ok(policy), _) => Err(anyhow::anyhow!(
        "unsupported project format {}",
        policy.version
      )),
      (Err(error), _) => Err(anyhow::anyhow!(error.to_string())),
    };
    checks.push(match semantics {
      Ok(detail) => doctor_check("supported_semantic_versions", true, detail),
      Err(error) => doctor_check("supported_semantic_versions", false, error.to_string()),
    });

    checks.push(doctor_check(
      "repository_write_scope",
      integrity.is_ok(),
      if integrity.is_ok() {
        "repository-contained paths validated"
      } else {
        "repository integrity prevents validating write scope"
      },
    ));

    let integration = self.validate_integrations(&root);
    checks.push(match integration {
      Ok(()) => doctor_check(
        "integration_consistency",
        true,
        "MCP, Skill, policy, and immutable authority state agree",
      ),
      Err(error) => doctor_check("integration_consistency", false, error.to_string()),
    });

    Ok(DoctorResult {
      schema_version: 1,
      healthy: checks.iter().all(|check| check.passed),
      checks,
    })
  }

  fn validate_persisted_evaluations(&self, root: &Path, chain: &LoadedAdmission) -> Result<usize> {
    let mut ids = self
      .repository
      .list_refs(root, REQUIREMENT_REFS)?
      .into_iter()
      .map(|(_, id)| id)
      .collect::<Vec<_>>();
    if let Some(id) = self.repository.read_ref(root, FINAL_REF)? {
      ids.push(id);
    }
    for id in &ids {
      let evaluation: Evaluation = self.load_object(root, id)?;
      let manifest = self.repository.manifest(root, &evaluation.candidate.0)?;
      identity::validate_candidate_manifest(&manifest)?;
      evaluate(
        &chain.loaded.contract,
        &chain.as_kernel_chain(),
        &evaluation,
      )?;
    }
    Ok(ids.len())
  }

  fn validate_integrations(&self, root: &Path) -> Result<()> {
    if let Some(id) = self.repository.read_ref(root, PROPOSAL_REF)? {
      self.load_proposed(root, &ProposalId(id))?;
    }
    if let Some(id) = self.repository.read_ref(root, RECONCILIATION_REF)? {
      let report: ReconciliationReport = self.load_object(root, &id)?;
      let proposal = self
        .repository
        .read_ref(root, PROPOSAL_REF)?
        .context("reconciliation ref exists without proposal ref")?;
      if report.proposal.0 != proposal {
        anyhow::bail!("reconciliation ref targets another proposal");
      }
    }
    let mcp_path = self
      .repository
      .resolve_relative_path(root, ".mcp.json", ExpectedEntry::File)?;
    let mcp: serde_json::Value = serde_json::from_slice(&self.repository.read_file(&mcp_path)?)?;
    if mcp
      .pointer("/mcpServers/tenet/command")
      .and_then(serde_json::Value::as_str)
      != Some("tenet")
      || mcp
        .pointer("/mcpServers/tenet/args/0")
        .and_then(serde_json::Value::as_str)
        != Some("mcp")
    {
      anyhow::bail!("Tenet MCP integration is inconsistent");
    }
    let skill_path =
      self
        .repository
        .resolve_relative_path(root, SKILL_PATH, ExpectedEntry::File)?;
    let skill = String::from_utf8(self.repository.read_file(&skill_path)?)?;
    for operation in [
      "tenet_context",
      "tenet_authority_submit",
      "tenet_requirement_check",
      "tenet_verify",
    ] {
      if !skill.contains(operation) {
        anyhow::bail!("Tenet Skill omits `{operation}`");
      }
    }
    Ok(())
  }

  fn execute_evaluation(
    &self,
    root: &Path,
    chain: &LoadedAdmission,
    candidate_id: CandidateId,
    scope: EvaluationScope,
  ) -> Result<Evaluation> {
    let verifier_ids = scoped_verifier_ids(&chain.loaded.contract, &scope)?;
    let mut runs = Vec::with_capacity(verifier_ids.len());
    for verifier_id in verifier_ids {
      let verifier = chain
        .loaded
        .policy
        .verifiers
        .iter()
        .find(|verifier| verifier.id == verifier_id)
        .with_context(|| format!("configured verifier `{verifier_id}` disappeared"))?;
      let run = match self.execute_configured_verifier(root, chain, &candidate_id, verifier) {
        Ok(run) => run,
        Err(error) => infrastructure_run(
          chain.id.clone(),
          chain.admission.authority.clone(),
          chain.loaded.authority.contract.clone(),
          chain.loaded.contract.policy.clone(),
          candidate_id.clone(),
          verifier,
          error.to_string(),
        )?,
      };
      runs.push(run);
    }
    Ok(Evaluation {
      admission: chain.id.clone(),
      authority: chain.admission.authority.clone(),
      candidate: candidate_id,
      scope,
      runs,
    })
  }

  fn execute_configured_verifier(
    &self,
    root: &Path,
    chain: &LoadedAdmission,
    candidate_id: &CandidateId,
    verifier: &VerifierSpec,
  ) -> Result<tenet_domain::algebra::VerifierRun> {
    let definition_digest = canonical_digest(verifier)?;
    // Protected execution reads through a privately staged view whose
    // immutability the operating system enforces against other processes;
    // local execution keeps the ordinary materialized snapshots.
    let protected_view = if verifier.protection == VerifierProtection::Protected {
      Some(
        self
          .repository
          .stage_protected_view(root, &candidate_id.0, &chain.loaded.authority.surface)
          .context("stage protected verifier view")?,
      )
    } else {
      None
    };
    let local_candidate = if protected_view.is_none() {
      Some(self.repository.materialize(root, &candidate_id.0)?)
    } else {
      None
    };
    let local_authority = if protected_view.is_none() {
      Some(
        self
          .repository
          .materialize(root, &chain.loaded.authority.surface)?,
      )
    } else {
      None
    };
    let (candidate_root, authority_root) = match &protected_view {
      Some(view) => (view.candidate_root(), view.authority_root()),
      None => (
        local_candidate
          .as_ref()
          .expect("local candidate view")
          .path(),
        local_authority
          .as_ref()
          .expect("local authority view")
          .path(),
      ),
    };
    let scratch = self.repository.fresh_scratch(root)?;
    let output = self.repository.fresh_output(root)?;
    let identity = match verifier.authority {
      VerifierAuthority::Project => OracleIdentity::Project {
        verifier_id: verifier.id.clone(),
        candidate_id: candidate_id.clone(),
        definition_digest,
      },
      VerifierAuthority::AuthoritySnapshot => {
        let bundle_path = verifier
          .oracle_path
          .as_deref()
          .context("validated authority verifier lost oracle path")?;
        let manifest = self
          .repository
          .manifest(root, &chain.loaded.authority.surface)?;
        let bundle_content_id = identity::subtree_content_id(&manifest.entries, bundle_path)?;
        let executable_path = verifier
          .oracle_executable_path()
          .and_then(|path| path.to_str().map(str::to_owned))
          .context("validated authority verifier lost executable path")?;
        let executable_content_id =
          identity::sealed_executable_content_id(&manifest.entries, &executable_path)
            .context("sealed authority executable missing")?;
        OracleIdentity::AuthoritySnapshot {
          verifier_id: verifier.id.clone(),
          authority_id: chain.admission.authority.clone(),
          bundle_path: bundle_path.into(),
          bundle_content_id,
          executable_content_id,
          definition_digest,
        }
      }
    };
    let view_digests = protected_view
      .as_ref()
      .map_or(&[][..], |view| view.view_digests());
    let view_directories = protected_view
      .as_ref()
      .map_or(&[][..], |view| view.view_directories());
    if let Some(view) = &protected_view {
      // The verifier must only ever observe bytes already proven equal to
      // the admitted identities at this instant. On macOS this closes the
      // image-create-to-mount window: a hostiles write to the backing image
      // either precedes this check (rejected here as infrastructure) or
      // finds an unlinked file. Namespace backends verify inside the
      // private copy before exec.
      if !view.verify_intact(&candidate_id.0, &chain.loaded.authority.surface)? {
        return infrastructure_run(
          chain.id.clone(),
          chain.admission.authority.clone(),
          chain.loaded.authority.contract.clone(),
          chain.loaded.contract.policy.clone(),
          candidate_id.clone(),
          verifier,
          "protected view did not match the admitted identities before execution".into(),
        );
      }
    }
    let executed: ExecutedVerifier = self.runner.run(&VerifierRun {
      candidate_root,
      authority_root,
      scratch_root: scratch.path(),
      output_root: output.path(),
      verifier,
      authority_id: &chain.admission.authority,
      candidate_id,
      oracle_identity: &identity,
      view_digests,
      view_directories,
    })?;
    validate_executed_verifier(verifier, &identity, &executed)?;
    if let Some(view) = &protected_view {
      // The enforcing boundary belongs to the protected view; the staging
      // directory is not the verifier's read surface on namespace backends,
      // so integrity is proven by the view itself, not by a generic
      // recapture of a mutable directory.
      if !view.verify_intact(&candidate_id.0, &chain.loaded.authority.surface)? {
        return infrastructure_run(
          chain.id.clone(),
          chain.admission.authority.clone(),
          chain.loaded.authority.contract.clone(),
          chain.loaded.contract.policy.clone(),
          candidate_id.clone(),
          verifier,
          "protected view boundary changed during verification".into(),
        );
      }
    } else {
      let candidate_include = ["**".to_owned()];
      let observed_candidate =
        self
          .repository
          .capture_selected(root, candidate_root, &candidate_include, &[])?;
      let observed_authority = self.repository.capture(root, authority_root)?;
      if observed_candidate != candidate_id.0
        || observed_authority != chain.loaded.authority.surface
      {
        return infrastructure_run(
          chain.id.clone(),
          chain.admission.authority.clone(),
          chain.loaded.authority.contract.clone(),
          chain.loaded.contract.policy.clone(),
          candidate_id.clone(),
          verifier,
          "verifier mutated its immutable Candidate or Authority view".into(),
        );
      }
    }
    Ok(executed.domain_run(
      chain.id.clone(),
      chain.admission.authority.clone(),
      chain.loaded.authority.contract.clone(),
      chain.loaded.contract.policy.clone(),
      candidate_id.clone(),
      verifier.id.clone(),
    ))
  }

  fn capture_candidate(&self, root: &Path, policy: &VerificationPolicy) -> Result<CandidateId> {
    let candidate_root = self.repository.resolve_relative_path(
      root,
      &policy.candidate.root,
      ExpectedEntry::Directory,
    )?;
    let candidate_id = CandidateId(self.repository.capture_selected(
      root,
      &candidate_root,
      &policy.candidate.include,
      &policy.candidate.exclude,
    )?);
    let manifest = self.repository.manifest(root, &candidate_id.0)?;
    identity::validate_candidate_manifest(&manifest)?;
    Ok(candidate_id)
  }

  fn validate_contract_policy(
    &self,
    contract: &CompletionContractV1,
    policy: &VerificationPolicy,
  ) -> Result<()> {
    let configured = policy
      .verifiers
      .iter()
      .map(|verifier| (verifier.id.as_str(), verifier))
      .collect::<BTreeMap<_, _>>();
    for requirement in &contract.requirements {
      for criterion in &requirement.criteria {
        for verifier in &criterion.verifiers {
          let definition = configured.get(verifier.id.0.as_str()).ok_or_else(|| {
            TenetError::new(
              "verifier_not_configured",
              format!("verifier `{}` is not configured", verifier.id.0),
            )
          })?;
          let material = match definition.authority {
            VerifierAuthority::Project => VerifierMaterial::Candidate,
            VerifierAuthority::AuthoritySnapshot => VerifierMaterial::AuthorityBundle,
          };
          if material != verifier.material {
            return Err(
              TenetError::new(
                "verifier_material_mismatch",
                format!(
                  "verifier `{}` material does not match policy",
                  verifier.id.0
                ),
              )
              .into(),
            );
          }
        }
      }
    }
    Ok(())
  }

  fn validate_authority_sources(&self, root: &Path, policy: &VerificationPolicy) -> Result<()> {
    for verifier in policy
      .verifiers
      .iter()
      .filter(|verifier| verifier.authority == VerifierAuthority::AuthoritySnapshot)
    {
      let bundle_path = verifier
        .oracle_path
        .as_deref()
        .context("authority_snapshot verifier has no oracle_path")?;
      self
        .repository
        .resolve_relative_path(root, bundle_path, ExpectedEntry::Directory)?;
      let executable_path = verifier
        .oracle_executable_path()
        .and_then(|path| path.to_str().map(str::to_owned))
        .context("authority verifier executable must be an AuthorityPath")?;
      let executable =
        self
          .repository
          .resolve_relative_path(root, &executable_path, ExpectedEntry::File)?;
      if !self.repository.is_executable(&executable)? {
        return Err(
          TenetError::new(
            "oracle_executable_not_executable",
            format!("oracle executable `{executable_path}` is not executable"),
          )
          .into(),
        );
      }
      if let CommandCwd::Authority(cwd) = &verifier.command.cwd {
        self
          .repository
          .resolve_relative_path(root, cwd, ExpectedEntry::Directory)?;
      }
    }
    Ok(())
  }

  fn load_active(&self, root: &Path) -> Result<LoadedAdmission> {
    let id = AdmissionId(
      self
        .repository
        .read_ref(root, ACTIVE_ADMISSION_REF)?
        .ok_or_else(|| TenetError::new("admission_missing", "no active admission"))?,
    );
    self.load_admission(root, &id)
  }

  fn load_admission(&self, root: &Path, id: &AdmissionId) -> Result<LoadedAdmission> {
    let admission: Admission = self.load_object(root, &id.0)?;
    let (proposal, loaded) = self.load_proposed(root, &admission.proposal)?;
    let report: ReconciliationReport = self.load_object(root, &admission.reconciliation.0)?;
    validate_admission(
      &admission,
      &proposal,
      &report,
      &loaded.authority,
      &loaded.spec,
    )?;
    // Structural binding alone never authenticates a persisted Admission:
    // any process with repository write access can hand-construct a
    // content-addressed chain. A process holding the trusted secret re-verifies
    // the grant mac on every load, so a forged or tampered persisted chain can
    // never participate in verification or completion. Processes without the
    // secret load structurally for informational reads only; every path that
    // can influence verification or `DONE` requires the secret first.
    if let Some(secret) = self.admission_secret.as_deref() {
      grant::verify_grant(
        secret,
        &admission.grant,
        &admission.proposal,
        &admission.authority,
      )
      .map_err(|error| TenetError::new("admission_grant_invalid", error.to_string()))?;
    }
    validate_completion_admission(
      &loaded.contract,
      &AdmissionChain {
        admission: &admission,
        proposal: &proposal,
        report: &report,
        authority: &loaded.authority,
        spec: &loaded.spec,
        policy: &loaded.policy,
        surface_entries: &loaded.surface_entries,
      },
    )?;
    Ok(LoadedAdmission {
      id: id.clone(),
      admission,
      proposal,
      report,
      loaded,
    })
  }

  fn load_proposed(
    &self,
    root: &Path,
    id: &ProposalId,
  ) -> Result<(AuthorityProposal, LoadedAuthority)> {
    let proposal: AuthorityProposal = self.load_object(root, &id.0)?;
    if proposal.schema_version != 1 {
      return Err(AdmissionError::UnsupportedVersion.into());
    }
    let authority: Authority = self.load_object(root, &proposal.authority.0)?;
    let spec: SpecSnapshot = self.load_object(root, &authority.spec.0)?;
    if authority.schema_version != 1 || spec.schema_version != 1 {
      return Err(AdmissionError::UnsupportedVersion.into());
    }
    let surface = self.repository.materialize(root, &authority.surface)?;
    let policy = self.repository.load_policy(surface.path())?;
    validate_candidate_surface(&policy.candidate)?;
    let surface_entries = self.repository.manifest(root, &authority.surface)?.entries;
    self.validate_authority_sources(surface.path(), &policy)?;
    let path =
      self
        .repository
        .resolve_relative_path(surface.path(), &spec.path, ExpectedEntry::File)?;
    if spec.path != policy.spec_path || self.repository.read_file(&path)? != spec.content {
      return Err(AdmissionError::SpecificationMismatch.into());
    }
    let contract: CompletionContractV1 = self.load_object(root, &authority.contract)?;
    validate_contract(&contract)?;
    self.validate_contract_policy(&contract, &policy)?;
    let surface_contract =
      self
        .repository
        .resolve_relative_path(surface.path(), CONTRACT_PATH, ExpectedEntry::File)?;
    let surface_contract: CompletionContractV1 =
      serde_json::from_slice(&self.repository.read_file(&surface_contract)?)?;
    if surface_contract != contract {
      return Err(
        TenetError::new(
          "authority_invalid",
          "authority contract does not match its immutable surface",
        )
        .into(),
      );
    }
    Ok((
      proposal,
      LoadedAuthority {
        authority,
        spec,
        policy,
        contract,
        surface_entries,
      },
    ))
  }

  fn store_value<T: Serialize>(&self, root: &Path, value: &T) -> Result<ContentObjectId> {
    self
      .repository
      .store_object(root, &serde_json::to_vec(value)?)
  }

  fn load_object<T: DeserializeOwned + Serialize>(
    &self,
    root: &Path,
    id: &ContentObjectId,
  ) -> Result<T> {
    let bytes = self.repository.load_object(root, id)?;
    let value: T = serde_json::from_slice(&bytes).map_err(|error| {
      TenetError::new(
        "object_invalid",
        format!("invalid immutable object: {error}"),
      )
    })?;
    if canonical_digest(&value)? != id.0 {
      return Err(ContentStoreError::integrity(id, "noncanonical immutable object").into());
    }
    Ok(value)
  }

  fn spec_is_current(&self, root: &Path, spec: &SpecSnapshot) -> Result<bool> {
    let path = match self
      .repository
      .resolve_relative_path(root, &spec.path, ExpectedEntry::File)
    {
      Ok(path) => path,
      Err(PathResolutionError::Missing { .. }) => return Ok(false),
      Err(error) => return Err(error.into()),
    };
    Ok(self.repository.read_file(&path)? == spec.content)
  }

  fn require_current_spec(&self, root: &Path, spec: &SpecSnapshot) -> Result<()> {
    if !self.spec_is_current(root, spec)? {
      return Err(
        TenetError::new(
          "authority_stale",
          "admitted specification differs from current specification",
        )
        .into(),
      );
    }
    Ok(())
  }

  fn initialized_root(&self) -> Result<PathBuf> {
    self.repository.discover_root(&self.cwd)
  }
}

impl LoadedAdmission {
  fn as_kernel_chain(&self) -> AdmissionChain<'_> {
    AdmissionChain {
      admission: &self.admission,
      proposal: &self.proposal,
      report: &self.report,
      authority: &self.loaded.authority,
      spec: &self.loaded.spec,
      policy: &self.loaded.policy,
      surface_entries: &self.loaded.surface_entries,
    }
  }
}

fn scoped_verifier_ids(
  contract: &CompletionContractV1,
  scope: &EvaluationScope,
) -> Result<BTreeSet<String>> {
  let requirements: Vec<_> = match scope {
    EvaluationScope::Final => contract.requirements.iter().collect(),
    EvaluationScope::Requirement { requirement } => vec![
      contract
        .requirements
        .iter()
        .find(|item| item.id == *requirement)
        .ok_or_else(|| {
          tenet_domain::algebra::AlgebraError::UnknownRequirement(requirement.0.clone())
        })?,
    ],
  };
  Ok(
    requirements
      .into_iter()
      .flat_map(|requirement| &requirement.criteria)
      .flat_map(|criterion| &criterion.verifiers)
      .map(|verifier| verifier.id.0.clone())
      .collect(),
  )
}

fn validate_executed_verifier(
  verifier: &VerifierSpec,
  oracle_identity: &OracleIdentity,
  executed: &ExecutedVerifier,
) -> Result<()> {
  let expected = verifier.command.result.interpret(
    executed.observation.exit_code,
    executed.observation.timed_out,
    executed.infrastructure_error.is_some(),
  );
  if executed.result != expected {
    anyhow::bail!("runner result disagrees with the admitted exit-code policy");
  }
  if !assurance_matches_protection(verifier, executed)
    || (executed.context.assurance.0 != LOCAL_V1 && executed.context.assurance.0 != PROTECTED_V1)
    || executed.context.runner_semantics.0 != RUNNER_SEMANTICS_V1
    || executed.execution.assurance != executed.context.assurance
    || executed.execution.runner_semantics != executed.context.runner_semantics
    || executed.execution.platform != executed.context.platform
    || executed.execution.resolved_program != executed.context.resolved_program
    || executed.execution.resolved_program_digest != executed.context.resolved_program_digest
    || executed.execution.oracle_identity != *oracle_identity
    || executed.execution.runner_identity.0.trim().is_empty()
    || executed
      .execution
      .execution_environment_identity
      .0
      .trim()
      .is_empty()
  {
    anyhow::bail!("runner context and provenance are inconsistent or unsupported");
  }
  Ok(())
}

/// A protected verifier must report `PROTECTED_V1`; an infrastructure failure
/// may report `LOCAL_V1` because no protected execution occurred. A local
/// verifier must never over-claim `PROTECTED_V1`.
fn assurance_matches_protection(verifier: &VerifierSpec, executed: &ExecutedVerifier) -> bool {
  match verifier.protection {
    VerifierProtection::Local => executed.context.assurance.0 == LOCAL_V1,
    VerifierProtection::Protected => {
      executed.context.assurance.0 == PROTECTED_V1 || executed.infrastructure_error.is_some()
    }
  }
}

fn infrastructure_run(
  admission: AdmissionId,
  authority: AuthorityId,
  contract: ContentObjectId,
  completion_policy: tenet_domain::algebra::CompletionPolicyId,
  candidate: CandidateId,
  verifier: &VerifierSpec,
  message: String,
) -> Result<tenet_domain::algebra::VerifierRun> {
  let platform = PlatformInformation {
    os: std::env::consts::OS.into(),
    architecture: std::env::consts::ARCH.into(),
  };
  let execution_environment_identity = ExecutionEnvironmentIdentity(bytes_digest(
    format!("{}:{message}", verifier.id).as_bytes(),
  ));
  let oracle_identity = OracleIdentity::Unavailable {
    verifier_id: verifier.id.clone(),
    definition_digest: canonical_digest(verifier)?,
  };
  Ok(tenet_domain::algebra::VerifierRun {
    admission,
    authority,
    contract,
    completion_policy,
    candidate,
    verifier: VerifierId(verifier.id.clone()),
    observation: ExecutionObservation {
      result: EvidenceResult::InfrastructureError,
      exit_code: None,
      timed_out: false,
      infrastructure_error: Some(message),
    },
    context: ExecutionContext {
      assurance: AssuranceProfileId(LOCAL_V1.into()),
      runner_semantics: RunnerSemanticsId(RUNNER_SEMANTICS_V1.into()),
      platform: platform.clone(),
      resolved_program: None,
      resolved_program_digest: None,
    },
    provenance: ExecutionProvenance {
      runner_identity: RunnerIdentity("tenet.application.infrastructure.v1".into()),
      runner_semantics: RunnerSemanticsId(RUNNER_SEMANTICS_V1.into()),
      assurance: AssuranceProfileId(LOCAL_V1.into()),
      tenet_version: env!("CARGO_PKG_VERSION").into(),
      platform,
      resolved_program: None,
      resolved_program_digest: None,
      oracle_identity,
      execution_environment_identity,
    },
  })
}

fn requirement_ref_name(requirement: &RequirementId) -> Result<String> {
  let digest = canonical_digest(requirement)?;
  Ok(format!("{REQUIREMENT_REFS}/{}", &digest[7..]))
}

fn doctor_check(name: impl Into<String>, passed: bool, detail: impl Into<String>) -> DoctorCheck {
  DoctorCheck {
    name: name.into(),
    passed,
    detail: detail.into(),
  }
}

fn authoring_readiness(policy: &VerificationPolicy) -> AuthoringReadiness {
  let candidate_configured = validate_candidate_surface(&policy.candidate).is_ok();
  let configured_verifier_ids = policy
    .verifiers
    .iter()
    .map(|verifier| verifier.id.clone())
    .collect::<Vec<_>>();
  let mut missing_prerequisites = Vec::new();
  if !candidate_configured {
    missing_prerequisites.push("candidate_surface_not_configured".into());
  }
  if configured_verifier_ids.is_empty() {
    missing_prerequisites.push("no_verifiers_configured".into());
  }
  AuthoringReadiness {
    config_path: ".tenet/tenet.toml".into(),
    candidate_configured,
    configured_verifier_ids,
    missing_prerequisites,
  }
}

fn context_for_phase(
  phase: WorkflowPhase,
  active_admission_id: Option<AdmissionId>,
  authority_id: Option<AuthorityId>,
  completion_policy_id: Option<tenet_domain::algebra::CompletionPolicyId>,
  current_candidate_id: Option<CandidateId>,
  requirement_checks: Vec<RequirementStatus>,
  authoring: Option<AuthoringReadiness>,
) -> ContextResult {
  let next_action = match phase {
    WorkflowPhase::SpecRequired => "Create SPEC.md, then run tenet init.",
    WorkflowPhase::AuthorityRequired => {
      "Configure the Candidate and verifier prerequisites, then submit an authority PROPOSAL."
    }
    WorkflowPhase::AuthorityReconciliation => "Submit RECONCILIATION for the exact proposal.",
    WorkflowPhase::AuthorityClarification => "Submit CLARIFICATION or a revised PROPOSAL.",
    WorkflowPhase::AuthorityAdmission => {
      "Ask the trusted operator to mint a grant and submit ADMISSION for the exact proposal, reconciliation, and authority; never run tenet authority grant yourself."
    }
    WorkflowPhase::AuthorityStale => {
      "Submit and admit a new authority for the current specification."
    }
    WorkflowPhase::Incompatible => "Run tenet doctor and replace unsupported or corrupt state.",
    WorkflowPhase::Implementation => "Implement requirements, check them, then call tenet_verify.",
    WorkflowPhase::Completed => "No action; current Candidate is verified.",
  };
  ContextResult {
    schema_version: 1,
    phase,
    active_admission_id,
    authority_id,
    completion_policy_id,
    current_candidate_id,
    requirement_checks,
    authoring,
    next_action: next_action.into(),
  }
}

fn blocker(code: &str, message: impl Into<String>) -> Blocker {
  Blocker {
    code: code.into(),
    message: message.into(),
  }
}

fn blockers_from_context(context: &ContextResult) -> BlockersResult {
  let mut blockers = Vec::new();
  match context.phase {
    WorkflowPhase::SpecRequired => blockers.push(blocker(
      "spec_missing",
      "The admitted specification file is missing; create it, then run tenet init.",
    )),
    WorkflowPhase::Incompatible => blockers.push(blocker(
      "state_incompatible",
      "Persisted Tenet state is unsupported or corrupt; run tenet doctor.",
    )),
    WorkflowPhase::AuthorityRequired => blockers.push(blocker(
      "authority_proposal_missing",
      "No authority proposal exists for the current specification; submit a PROPOSAL.",
    )),
    WorkflowPhase::AuthorityReconciliation => blockers.push(blocker(
      "reconciliation_missing",
      "The active proposal has no reconciliation report; submit RECONCILIATION.",
    )),
    WorkflowPhase::AuthorityClarification => blockers.push(blocker(
      "blocking_findings",
      "Blocking issues or findings require CLARIFICATION or a revised PROPOSAL.",
    )),
    WorkflowPhase::AuthorityAdmission => blockers.push(blocker(
      "admission_missing",
      "Admission requires a trusted grant bound to the exact proposal and authority; ask the trusted operator to mint a grant and admit.",
    )),
    WorkflowPhase::AuthorityStale => blockers.push(blocker(
      "authority_stale",
      "The admitted authority no longer matches the current specification; admit a new authority.",
    )),
    WorkflowPhase::Implementation => {
      if context.requirement_checks.is_empty() {
        blockers.push(blocker(
          "verification_missing",
          "No requirement checks are recorded for the active admission.",
        ));
      }
      for check in &context.requirement_checks {
        if check.state != CompletionState::Satisfied {
          blockers.push(blocker(
            "requirement_not_satisfied",
            format!(
              "requirement `{}` derived {}",
              check.requirement_id.0,
              serde_json::to_value(check.state)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "unsatisfied".into())
            ),
          ));
        }
      }
    }
    WorkflowPhase::Completed => {}
  }
  BlockersResult {
    schema_version: 1,
    phase: context.phase,
    blockers,
  }
}
