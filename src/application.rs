use std::{
  collections::{BTreeMap, BTreeSet},
  fs,
  path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use schemars::{schema_for, JsonSchema, Schema};
use serde::Deserialize;
use tenet_domain::{
  completion::{
    derive_completion, derive_obligation_state, Blocker, BlockerCode, DerivationContext,
    ObligationState, Verdict,
  },
  contract::{validate_proposal, AdmittedContract, ContractProposal, ProposalRecord},
  digest::{bytes_digest, canonical_digest},
  evidence::{
    ArtifactAuthority, ArtifactProvenance, ArtifactValidity, DependencySurface, EvidenceArtifact,
    EvidenceEffect, GitObjectId, OracleIdentity, VerifierEvidence,
  },
  policy::{
    validate_policy, RepositoryConfig, VerificationPolicy, VerifierAuthority, VerifierSpec,
  },
};

use crate::{
  audit::AuditState,
  repository::{
    self, atomic_write, discover_root, load_policy, policy_digest, specification_digest,
    MaterializedRevision, CONFIG_PATH, CONTRACT_PATH, SKILL_PATH, TENET_DIR,
  },
  response::{
    ApprovalResult, ContractState, EvidenceResult, GateResult, InitResult, ProposalResult,
    StatusResult,
  },
  verifier::{run_verifier, ExecutedVerifier},
};

#[derive(Clone, Debug)]
pub struct Tenet {
  cwd: PathBuf,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InitializeRequest {
  pub spec_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApproveRequest {
  pub proposal_id: String,
  pub proposal_digest: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GateRequest {
  pub authority_revision: String,
  pub revision: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceRequest {
  pub revision: String,
}

impl Tenet {
  pub fn new(cwd: PathBuf) -> Self {
    Self { cwd }
  }

  pub fn initialize(&self, request: &InitializeRequest) -> Result<InitResult> {
    initialize(&self.cwd, request.spec_path.as_deref())
  }

  pub fn status(&self) -> Result<StatusResult> {
    status(&self.cwd)
  }

  pub fn contract_schema(&self) -> Schema {
    schema_for!(ContractProposal)
  }

  pub fn policy_schema(&self) -> Schema {
    schema_for!(RepositoryConfig)
  }

  pub fn propose(&self, proposal: ContractProposal) -> Result<ProposalResult> {
    propose(&self.cwd, proposal)
  }

  pub fn approve(&self, request: &ApproveRequest) -> Result<ApprovalResult> {
    approve(&self.cwd, &request.proposal_id, &request.proposal_digest)
  }

  pub fn gate(&self, request: &GateRequest) -> Result<GateResult> {
    gate(&self.cwd, &request.authority_revision, &request.revision)
  }

  pub fn exact_evidence(&self, request: &EvidenceRequest) -> Result<EvidenceResult> {
    let root = initialized_root(&self.cwd)?;
    let revision = repository::resolve_revision(&root, &request.revision)
      .context("resolve evidence candidate revision")?;
    evidence(&self.cwd, Some(&revision))
  }
}

fn initialize(cwd: &Path, spec: Option<&Path>) -> Result<InitResult> {
  let root = discover_root(cwd)?;
  let spec = match spec {
    Some(spec) if spec.is_absolute() => spec.to_path_buf(),
    Some(spec) => cwd
      .canonicalize()
      .context("canonicalize working directory")?
      .join(spec),
    None => root.join("SPEC.md"),
  };
  let (policy, spec_digest, created) = repository::initialize(&root, &spec)?;
  Ok(InitResult {
    schema_version: 1,
    initialized: true,
    created,
    spec_path: policy.spec_path,
    spec_digest,
    contract_state: if root.join(CONTRACT_PATH).exists() {
      ContractState::Admitted
    } else {
      ContractState::Missing
    },
    skill_path: SKILL_PATH.into(),
  })
}

fn status(cwd: &Path) -> Result<StatusResult> {
  let root = discover_root(cwd)?;
  if !root.join(CONFIG_PATH).exists() {
    return Ok(StatusResult {
      schema_version: 1,
      initialized: false,
      spec_path: None,
      spec_digest: None,
      policy_digest: None,
      contract_state: ContractState::Missing,
      contract_digest: None,
      last_gated_authority_revision: None,
      last_gated_revision: None,
      last_verdict: None,
      unresolved_obligations: Vec::new(),
    });
  }
  let policy = load_policy(&root)?;
  let current_spec_digest = specification_digest(&root, &policy)?;
  let current_policy_digest = policy_digest(&policy)?;
  let contract = load_contract_optional(&root)?;
  let contract_digest = contract
    .as_ref()
    .map(canonical_digest)
    .transpose()
    .context("hash admitted contract")?;
  let pending = has_pending_proposal(&root)?;
  let contract_state = match &contract {
    Some(contract)
      if contract.spec_digest != current_spec_digest
        || contract.policy_digest != current_policy_digest =>
    {
      ContractState::Stale
    }
    Some(_) => ContractState::Admitted,
    None if pending => ContractState::PendingApproval,
    None => ContractState::Missing,
  };
  let audit = AuditState::load(&root)?;
  let last = audit.gates.last();
  let unresolved_obligations = last
    .map(|gate| {
      gate
        .obligations
        .iter()
        .filter(|item| item.state != ObligationState::ContractSatisfied)
        .cloned()
        .collect()
    })
    .unwrap_or_default();
  Ok(StatusResult {
    schema_version: 1,
    initialized: true,
    spec_path: Some(policy.spec_path),
    spec_digest: Some(current_spec_digest),
    policy_digest: Some(current_policy_digest),
    contract_state,
    contract_digest,
    last_gated_authority_revision: last.map(|gate| gate.authority_revision.clone()),
    last_gated_revision: last.map(|gate| gate.revision.clone()),
    last_verdict: last.map(|gate| gate.verdict.clone()),
    unresolved_obligations,
  })
}

fn propose(cwd: &Path, proposal: ContractProposal) -> Result<ProposalResult> {
  let root = initialized_root(cwd)?;
  let policy = load_policy(&root)?;
  let current_spec_digest = specification_digest(&root, &policy)?;
  let current_policy_digest = policy_digest(&policy)?;
  validate_proposal(&proposal, &policy).context("validate contract proposal")?;
  if proposal.spec_digest != current_spec_digest {
    bail!(
      "proposal specification digest `{}` does not match current `{current_spec_digest}`",
      proposal.spec_digest
    );
  }
  if proposal.policy_digest != current_policy_digest {
    bail!(
      "proposal policy digest `{}` does not match current `{current_policy_digest}`",
      proposal.policy_digest
    );
  }
  let digest = canonical_digest(&proposal).context("hash contract proposal")?;
  let proposal_id = format!(
    "proposal-{}",
    digest_key(&digest).chars().take(16).collect::<String>()
  );
  let record = ProposalRecord {
    proposal_id: proposal_id.clone(),
    proposal_digest: digest.clone(),
    proposal,
  };
  let mut encoded = serde_json::to_vec_pretty(&record)?;
  encoded.push(b'\n');
  atomic_write(&proposal_path(&root, &digest), &encoded)?;
  Ok(ProposalResult {
    schema_version: 1,
    proposal_id,
    proposal_digest: digest,
    approval_required: true,
  })
}

fn approve(cwd: &Path, proposal_id: &str, digest: &str) -> Result<ApprovalResult> {
  let root = initialized_root(cwd)?;
  let path = proposal_path(&root, digest);
  let bytes = fs::read(&path).with_context(|| {
    format!("pending proposal `{digest}` not found; submit a contract proposal first")
  })?;
  let record: ProposalRecord = serde_json::from_slice(&bytes).context("parse pending proposal")?;
  if record.proposal_id != proposal_id || record.proposal_digest != digest {
    bail!("approval identity does not match the stored proposal");
  }
  let actual_digest = canonical_digest(&record.proposal)?;
  if actual_digest != digest {
    bail!("stored proposal content does not match approval digest");
  }
  let policy = load_policy(&root)?;
  validate_proposal(&record.proposal, &policy).context("revalidate contract proposal")?;
  let spec_digest = specification_digest(&root, &policy)?;
  let current_policy_digest = policy_digest(&policy)?;
  if record.proposal.spec_digest != spec_digest
    || record.proposal.policy_digest != current_policy_digest
  {
    bail!("proposal is stale because the specification or verification policy changed");
  }
  let admitted = AdmittedContract::from(record);
  let contract_digest = canonical_digest(&admitted)?;
  let mut encoded = serde_json::to_vec_pretty(&admitted)?;
  encoded.push(b'\n');
  atomic_write(&root.join(CONTRACT_PATH), &encoded)?;
  Ok(ApprovalResult {
    schema_version: 1,
    proposal_id: admitted.proposal_id,
    proposal_digest: admitted.proposal_digest,
    contract_digest,
    contract_path: CONTRACT_PATH.into(),
  })
}

fn gate(cwd: &Path, authority_revision: &str, revision: &str) -> Result<GateResult> {
  let root = discover_root(cwd)?;
  let authority_revision = repository::resolve_revision(&root, authority_revision)
    .context("resolve authority revision")?;
  let revision =
    repository::resolve_revision(&root, revision).context("resolve candidate revision")?;

  if !repository::is_ancestor(&root, &authority_revision, &revision)? {
    let result = control_plane_gate(
      authority_revision,
      revision,
      BlockerCode::AuthorityRevisionNotAncestor,
      "authority revision is not an ancestor of the candidate revision",
    );
    return finish_gate(&root, result, Vec::new());
  }

  let Some(policy_bytes) = repository::read_revision_file(&root, &authority_revision, CONFIG_PATH)?
  else {
    let result = control_plane_gate(
      authority_revision,
      revision,
      BlockerCode::RepositoryNotInitialized,
      "authority revision is not Tenet-enabled",
    );
    return finish_gate(&root, result, Vec::new());
  };
  let policy_text = String::from_utf8(policy_bytes).context("authority policy is not UTF-8")?;
  let policy: VerificationPolicy =
    toml::from_str(&policy_text).context("parse authority revision policy")?;
  validate_policy(&policy).context("validate authority revision policy")?;
  let policy_digest = policy_digest(&policy)?;

  let spec_bytes = repository::read_revision_file(&root, &authority_revision, &policy.spec_path)?
    .with_context(|| {
    format!(
      "authority revision specification `{}` does not exist",
      policy.spec_path
    )
  })?;
  let spec_digest = bytes_digest(&spec_bytes);

  let Some(contract_bytes) =
    repository::read_revision_file(&root, &authority_revision, CONTRACT_PATH)?
  else {
    let mut result = control_plane_gate(
      authority_revision,
      revision,
      BlockerCode::ContractMissing,
      "authority revision has no admitted completion contract",
    );
    result.spec_digest = spec_digest;
    result.policy_digest = policy_digest;
    return finish_gate(&root, result, Vec::new());
  };
  let contract: AdmittedContract =
    serde_json::from_slice(&contract_bytes).context("parse authority revision contract")?;
  let contract_digest = canonical_digest(&contract)?;

  if contract.spec_digest != spec_digest {
    let result = mismatch_gate(
      authority_revision,
      revision,
      spec_digest,
      contract_digest,
      policy_digest,
      BlockerCode::SpecificationChanged,
      "authority specification digest differs from the admitted contract",
    );
    return finish_gate(&root, result, Vec::new());
  }
  if contract.policy_digest != policy_digest {
    let result = mismatch_gate(
      authority_revision,
      revision,
      spec_digest,
      contract_digest,
      policy_digest,
      BlockerCode::PolicyChanged,
      "authority policy digest differs from the admitted contract",
    );
    return finish_gate(&root, result, Vec::new());
  }
  validate_admitted_contract(&contract, &policy)?;

  let mut authority_paths = BTreeSet::from([CONFIG_PATH, CONTRACT_PATH, policy.spec_path.as_str()]);
  authority_paths.extend(
    policy
      .verifiers
      .iter()
      .filter_map(|verifier| verifier.oracle_path.as_deref()),
  );
  let authority_paths = authority_paths.into_iter().collect::<Vec<_>>();
  let changed_paths =
    repository::changed_paths(&root, &authority_revision, &revision, &authority_paths)?;
  if !changed_paths.is_empty() {
    let message = format!(
      "candidate changed authority-owned paths: {}; admit and select a new authority revision",
      changed_paths.join(", ")
    );
    let result = mismatch_gate(
      authority_revision,
      revision,
      spec_digest,
      contract_digest,
      policy_digest,
      BlockerCode::AuthoritySurfaceChanged,
      &message,
    );
    return finish_gate(&root, result, Vec::new());
  }

  let verifier_ids = contract
    .obligations()
    .flat_map(|obligation| {
      std::iter::once(obligation.evidence_contract.claim.verifier_id.as_str()).chain(
        obligation
          .evidence_contract
          .assurances
          .iter()
          .map(|assurance| assurance.verifier_id.as_str()),
      )
    })
    .collect::<BTreeSet<_>>();
  let mut observations: BTreeMap<String, ExecutedVerifier> = BTreeMap::new();
  let mut infrastructure_errors: BTreeMap<String, String> = BTreeMap::new();
  let mut oracle_identities: BTreeMap<String, OracleIdentity> = BTreeMap::new();
  for verifier_id in verifier_ids {
    let verifier = policy
      .verifiers
      .iter()
      .find(|item| item.id == verifier_id)
      .context("validated verifier disappeared")?;
    match execute_configured_verifier(&root, &authority_revision, &revision, verifier) {
      Ok((oracle_identity, executed)) => {
        oracle_identities.insert(verifier_id.into(), oracle_identity);
        if executed.observation.timed_out {
          infrastructure_errors.insert(verifier_id.into(), "verifier timed out".to_owned());
        } else if executed.observation.exit_code.is_none() {
          infrastructure_errors.insert(
            verifier_id.into(),
            "verifier terminated without an exit code".to_owned(),
          );
        } else {
          observations.insert(verifier_id.into(), executed);
        }
      }
      Err(error) => {
        infrastructure_errors.insert(verifier_id.into(), format!("{error:#}"));
      }
    }
  }

  let mut artifacts = Vec::new();
  for obligation in contract.obligations() {
    let claim = &obligation.evidence_contract.claim;
    if let Some(executed) = observations.get(&claim.verifier_id) {
      if executed.observation.exit_code != Some(125) {
        let verifier = policy
          .verifiers
          .iter()
          .find(|item| item.id == claim.verifier_id)
          .context("validated verifier disappeared")?;
        let oracle_identity = oracle_identities
          .get(&claim.verifier_id)
          .context("executed verifier has no oracle identity")?
          .clone();
        artifacts.push(EvidenceArtifact::Claim {
          evidence: VerifierEvidence {
            obligation_id: obligation.id.clone(),
            authority_revision: authority_revision.clone(),
            revision: revision.clone(),
            verifier_id: claim.verifier_id.clone(),
            policy_digest: policy_digest.clone(),
            spec_digest: spec_digest.clone(),
            contract_digest: contract_digest.clone(),
            authority: artifact_authority(verifier.authority),
            oracle_identity,
            provenance: ArtifactProvenance::TenetLocalVerifier,
            execution: executed.execution.clone(),
            effect: evidence_effect(executed.observation.exit_code),
            validity: ArtifactValidity::Valid,
            dependency_surface: DependencySurface::RepositoryWide,
            observation: executed.observation.clone(),
          },
        });
      }
    }
    let Some(qualified_oracle_identity) = oracle_identities.get(&claim.verifier_id).cloned() else {
      continue;
    };
    for assurance in &obligation.evidence_contract.assurances {
      let Some(executed) = observations.get(&assurance.verifier_id) else {
        continue;
      };
      if executed.observation.exit_code == Some(125) {
        continue;
      }
      let verifier = policy
        .verifiers
        .iter()
        .find(|item| item.id == assurance.verifier_id)
        .context("validated verifier disappeared")?;
      let oracle_identity = oracle_identities
        .get(&assurance.verifier_id)
        .context("executed assurance verifier has no oracle identity")?
        .clone();
      artifacts.push(EvidenceArtifact::OracleAssurance {
        assurance_id: assurance.id.clone(),
        assurance_criterion: assurance.criterion.clone(),
        qualified_oracle_identity: qualified_oracle_identity.clone(),
        evidence: VerifierEvidence {
          obligation_id: obligation.id.clone(),
          authority_revision: authority_revision.clone(),
          revision: revision.clone(),
          verifier_id: assurance.verifier_id.clone(),
          policy_digest: policy_digest.clone(),
          spec_digest: spec_digest.clone(),
          contract_digest: contract_digest.clone(),
          authority: artifact_authority(verifier.authority),
          oracle_identity,
          provenance: ArtifactProvenance::TenetLocalVerifier,
          execution: executed.execution.clone(),
          effect: evidence_effect(executed.observation.exit_code),
          validity: ArtifactValidity::Valid,
          dependency_surface: DependencySurface::RepositoryWide,
          observation: executed.observation.clone(),
        },
      });
    }
  }

  let context = DerivationContext {
    authority_revision: &authority_revision,
    revision: &revision,
    spec_digest: &spec_digest,
    contract_digest: &contract_digest,
    policy_digest: &policy_digest,
    contract: &contract,
    policy: &policy,
    oracle_identities: &oracle_identities,
    infrastructure_errors: &infrastructure_errors,
  };
  let mut obligations = Vec::new();
  for obligation in contract.obligations() {
    let mut derived = derive_obligation_state(&context, &obligation.id, &artifacts);
    if derived.state == ObligationState::Contradicted {
      derived.blockers.push(Blocker {
        code: BlockerCode::VerifierFailed,
        obligation_id: Some(obligation.id.clone()),
        verifier_id: Some(obligation.evidence_contract.claim.verifier_id.clone()),
        message: "configured primary verifier exited unsuccessfully".into(),
      });
    }
    obligations.push(derived);
  }
  let verdict = derive_completion(&obligations);
  let blockers = obligations
    .iter()
    .flat_map(|item| item.blockers.clone())
    .collect();
  let result = GateResult {
    schema_version: 1,
    authority_revision,
    revision,
    spec_digest,
    contract_digest,
    policy_digest,
    verdict,
    obligations,
    blockers,
  };
  finish_gate(&root, result, artifacts)
}
fn execute_configured_verifier(
  root: &Path,
  authority_revision: &str,
  revision: &str,
  verifier: &VerifierSpec,
) -> Result<(OracleIdentity, ExecutedVerifier)> {
  let definition_digest = canonical_digest(verifier)?;
  let oracle_identity = match verifier.authority {
    VerifierAuthority::Project => OracleIdentity::Project {
      verifier_id: verifier.id.clone(),
      candidate_revision: revision.into(),
      definition_digest,
    },
    VerifierAuthority::AuthoritySnapshot => {
      let bundle_path = verifier
        .oracle_path
        .as_deref()
        .context("validated authority_snapshot verifier lost oracle_path")?;
      let bundle_object =
        repository::revision_directory_object(root, authority_revision, bundle_path)?;
      let executable_path = verifier
        .oracle_executable_path()
        .context("validated authority_snapshot verifier lost executable path")?;
      let executable_path = executable_path
        .to_str()
        .context("authority oracle executable path is not UTF-8")?;
      let executable_object =
        repository::revision_executable_object(root, authority_revision, executable_path)?;
      OracleIdentity::AuthoritySnapshot {
        verifier_id: verifier.id.clone(),
        authority_revision: authority_revision.into(),
        bundle_path: bundle_path.into(),
        bundle_object_id: GitObjectId(bundle_object),
        executable_object_id: GitObjectId(executable_object),
        definition_digest,
      }
    }
  };
  let candidate = MaterializedRevision::create(root, revision)?;
  let authority = if verifier.authority == VerifierAuthority::AuthoritySnapshot {
    Some(MaterializedRevision::create(root, authority_revision)?)
  } else {
    None
  };
  let oracle_bundle = match authority.as_ref() {
    Some(materialized) => Some(
      materialized.path().join(
        verifier
          .oracle_path
          .as_deref()
          .context("validated authority_snapshot verifier lost oracle_path")?,
      ),
    ),
    None => None,
  };
  let executed = run_verifier(
    candidate.path(),
    oracle_bundle.as_deref(),
    verifier,
    authority_revision,
    revision,
    &oracle_identity,
  )?;
  Ok((oracle_identity, executed))
}

fn evidence_effect(exit_code: Option<i32>) -> EvidenceEffect {
  match exit_code {
    Some(0) => EvidenceEffect::Supports,
    Some(126) => EvidenceEffect::Inconclusive,
    _ => EvidenceEffect::Contradicts,
  }
}

fn artifact_authority(authority: VerifierAuthority) -> ArtifactAuthority {
  match authority {
    VerifierAuthority::Project => ArtifactAuthority::TenetObservedProjectVerifier,
    VerifierAuthority::AuthoritySnapshot => {
      ArtifactAuthority::TenetObservedAuthoritySnapshotVerifier
    }
  }
}

fn evidence(cwd: &Path, revision: Option<&str>) -> Result<EvidenceResult> {
  let root = initialized_root(cwd)?;
  let audit = AuditState::load(&root)?;
  let artifacts = audit
    .evidence
    .into_iter()
    .filter(|item| revision.is_none_or(|revision| item.evidence().revision == revision))
    .collect();
  let gates = audit
    .gates
    .into_iter()
    .filter(|item| revision.is_none_or(|revision| item.revision == revision))
    .collect();
  Ok(EvidenceResult {
    schema_version: 1,
    revision: revision.map(str::to_owned),
    artifacts,
    gates,
  })
}

fn finish_gate(
  root: &Path,
  result: GateResult,
  artifacts: Vec<EvidenceArtifact>,
) -> Result<GateResult> {
  let mut audit = AuditState::load(root)?;
  audit.evidence.extend(artifacts);
  audit.gates.push(result.clone());
  audit.save(root)?;
  Ok(result)
}

fn control_plane_gate(
  authority_revision: String,
  revision: String,
  code: BlockerCode,
  message: &str,
) -> GateResult {
  mismatch_gate(
    authority_revision,
    revision,
    String::new(),
    String::new(),
    String::new(),
    code,
    message,
  )
}

fn mismatch_gate(
  authority_revision: String,
  revision: String,
  spec_digest: String,
  contract_digest: String,
  policy_digest: String,
  code: BlockerCode,
  message: &str,
) -> GateResult {
  GateResult {
    schema_version: 1,
    authority_revision,
    revision,
    spec_digest,
    contract_digest,
    policy_digest,
    verdict: Verdict::NotDone,
    obligations: Vec::new(),
    blockers: vec![Blocker {
      code,
      obligation_id: None,
      verifier_id: None,
      message: message.into(),
    }],
  }
}

fn validate_admitted_contract(
  contract: &AdmittedContract,
  policy: &VerificationPolicy,
) -> Result<()> {
  let proposal = ContractProposal {
    schema_version: contract.schema_version,
    spec_digest: contract.spec_digest.clone(),
    policy_digest: contract.policy_digest.clone(),
    requirements: contract.requirements.clone(),
  };
  validate_proposal(&proposal, policy).context("validate admitted contract")
}

fn load_contract_optional(root: &Path) -> Result<Option<AdmittedContract>> {
  let path = root.join(CONTRACT_PATH);
  if !path.exists() {
    return Ok(None);
  }
  let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
  serde_json::from_slice(&bytes)
    .context("parse admitted completion contract")
    .map(Some)
}

fn initialized_root(cwd: &Path) -> Result<PathBuf> {
  let root = discover_root(cwd)?;
  if !root.join(CONFIG_PATH).exists() {
    bail!("repository is not initialized")
  }
  Ok(root)
}

fn proposal_path(root: &Path, digest: &str) -> PathBuf {
  root
    .join(TENET_DIR)
    .join("proposals")
    .join(format!("{}.json", digest_key(digest)))
}

fn digest_key(digest: &str) -> &str {
  digest.strip_prefix("sha256:").unwrap_or(digest)
}

fn has_pending_proposal(root: &Path) -> Result<bool> {
  let directory = root.join(TENET_DIR).join("proposals");
  if !directory.exists() {
    return Ok(false);
  }
  Ok(fs::read_dir(directory)?.next().transpose()?.is_some())
}
