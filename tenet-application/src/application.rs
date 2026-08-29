use std::{
  collections::{BTreeMap, BTreeSet},
  fs,
  path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use schemars::{JsonSchema, Schema, schema_for};
use serde::Deserialize;
use tenet_domain::{
  completion::{
    Blocker, BlockerCode, DerivationContext, ObligationState, Verdict, derive_completion,
    derive_obligation_state,
  },
  contract::{
    AdmittedContract, ContractError, ContractProposal, ContractProposalInput, ProposalRecord,
    analyze_verification, canonicalize_proposal, validate_proposal,
  },
  digest::{bytes_digest, canonical_digest},
  evidence::{
    ArtifactAuthority, ArtifactProvenance, ArtifactValidity, AuthorityId, CandidateId,
    ContentObjectId, DependencySurface, EvidenceArtifact, EvidenceEffect, OracleIdentity,
    VerifierEvidence,
  },
  policy::{
    PolicyError, VerificationPolicy, VerifierAuthority, VerifierSpec, validate_candidate_surface,
    validate_policy,
  },
};

use crate::{
  audit::AuditState,
  project::{
    self, CONFIG_PATH, CONTRACT_PATH, ContentStore, ContentStoreError, EntryKind,
    MaterializedSnapshot, SKILL_PATH, TENET_DIR, atomic_write, discover_root, load_policy,
    policy_digest, specification_digest,
  },
  response::{
    ApprovalResult, AuthoritySealResult, CandidateCaptureResult, ContractState, EvidenceResult,
    GateResult, InitResult, ProposalResult, StatusResult, TenetError,
  },
  verifier::{ExecutedVerifier, run_verifier},
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
  pub authority_id: AuthorityId,
  pub candidate_id: CandidateId,
}
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateCaptureRequest {
  pub authority_id: AuthorityId,
}
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceRequest {
  pub authority_id: AuthorityId,
  pub candidate_id: CandidateId,
}
#[expect(
  dead_code,
  reason = "Rust-derived MCP schema types are never instantiated."
)]
#[derive(JsonSchema)]
struct PolicySchema {
  version: u32,
  #[schemars(description = "Safe project-relative authority specification path.")]
  spec_path: String,
  #[schemars(
    description = "Authority-defined positive candidate surface: include selects the namespace and exclude only refines it. Symlinks are unsupported."
  )]
  candidate: tenet_domain::policy::CandidateCapturePolicy,
  verifiers: Vec<PolicyVerifierSchema>,
}
#[expect(
  dead_code,
  reason = "Rust-derived MCP schema variants are never instantiated."
)]
#[derive(JsonSchema)]
#[schemars(
  description = "Tagged verifier configuration. `authority` selects exactly one execution semantics variant."
)]
#[serde(tag = "authority", rename_all = "snake_case")]
enum PolicyVerifierSchema {
  #[schemars(
    description = "Execution root is Candidate Snapshot R. argv[0] is passed directly to the operating system process launcher; relative paths resolve from the verifier cwd and ordinary PATH/absolute-path semantics apply. oracle_path is forbidden."
  )]
  Project {
    id: String,
    argv: Vec<String>,
    cwd: String,
    timeout_seconds: u64,
    max_output_bytes: usize,
    env: BTreeMap<String, String>,
    environment_mode: tenet_domain::policy::EnvironmentMode,
  },
  #[schemars(
    description = "Execution root is the sealed oracle bundle from Authority Capsule A. oracle_path is required. argv[0] directly names a bundled executable; cwd is bundle-relative; R is exposed only through TENET_CANDIDATE_ROOT."
  )]
  AuthoritySnapshot {
    id: String,
    argv: Vec<String>,
    cwd: String,
    timeout_seconds: u64,
    max_output_bytes: usize,
    env: BTreeMap<String, String>,
    environment_mode: tenet_domain::policy::EnvironmentMode,
    oracle_path: String,
  },
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
    .find_map(|cause| cause.downcast_ref::<project::PathResolutionError>())
  {
    return TenetError::new(path_error_code(path_error), path_error.to_string()).with_context(
      None,
      path_error_path(path_error),
      None,
    );
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
    let code = match policy_error {
      PolicyError::CandidateSurfaceUnconfigured => "candidate_surface_unconfigured",
      _ => "policy_invalid",
    };
    return TenetError::new(code, policy_error.to_string());
  }
  if error
    .chain()
    .any(|cause| cause.downcast_ref::<toml::de::Error>().is_some())
  {
    return TenetError::new("policy_invalid", "verification policy is not valid TOML");
  }
  if let Some(contract_error) = error
    .chain()
    .find_map(|cause| cause.downcast_ref::<ContractError>())
  {
    return TenetError::new("contract_invalid", contract_error.to_string());
  }
  TenetError::new("internal_error", error.to_string())
}

fn path_error_code(error: &project::PathResolutionError) -> &'static str {
  match error {
    project::PathResolutionError::UnsupportedSymlink { .. } => "unsupported_symlink",
    project::PathResolutionError::PathEscape { .. } | project::PathResolutionError::Invalid => {
      "path_escape"
    }
    project::PathResolutionError::Missing { .. } => "path_missing",
    project::PathResolutionError::NotDirectory { .. } => "path_not_directory",
    project::PathResolutionError::NotFile { .. } => "path_not_file",
    project::PathResolutionError::Special { .. } => "unsupported_filesystem_entry",
    project::PathResolutionError::Io { .. } => "internal_error",
  }
}

fn path_error_path(error: &project::PathResolutionError) -> Option<String> {
  match error {
    project::PathResolutionError::Invalid => None,
    project::PathResolutionError::PathEscape { path }
    | project::PathResolutionError::UnsupportedSymlink { path }
    | project::PathResolutionError::Missing { path }
    | project::PathResolutionError::NotDirectory { path }
    | project::PathResolutionError::NotFile { path }
    | project::PathResolutionError::Special { path }
    | project::PathResolutionError::Io { path, .. } => Some(path.clone()),
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
  pub fn new(cwd: PathBuf) -> Self {
    Self { cwd }
  }
  pub fn initialize(&self, request: &InitializeRequest) -> AppResult<InitResult> {
    app_result(initialize(&self.cwd, request.spec_path.as_deref()))
  }
  pub fn status(&self) -> AppResult<StatusResult> {
    app_result(status(&self.cwd))
  }
  pub fn contract_schema(&self) -> Schema {
    schema_for!(ContractProposalInput)
  }
  pub fn policy_schema(&self) -> Schema {
    schema_for!(PolicySchema)
  }
  pub fn propose(&self, input: ContractProposalInput) -> AppResult<ProposalResult> {
    app_result(propose(&self.cwd, input))
  }
  pub fn approve(&self, request: &ApproveRequest) -> AppResult<ApprovalResult> {
    app_result(approve(
      &self.cwd,
      &request.proposal_id,
      &request.proposal_digest,
    ))
  }
  pub fn authority_seal(&self) -> AppResult<AuthoritySealResult> {
    app_result(authority_seal(&self.cwd))
  }
  pub fn candidate_capture(
    &self,
    request: &CandidateCaptureRequest,
  ) -> AppResult<CandidateCaptureResult> {
    app_result(candidate_capture(&self.cwd, request.authority_id.clone()))
  }
  pub fn gate(&self, request: &GateRequest) -> AppResult<GateResult> {
    app_result(gate(
      &self.cwd,
      request.authority_id.clone(),
      request.candidate_id.clone(),
    ))
  }
  pub fn exact_evidence(&self, request: &EvidenceRequest) -> AppResult<EvidenceResult> {
    app_result(evidence(
      &self.cwd,
      &request.authority_id,
      &request.candidate_id,
    ))
  }
}

fn initialize(cwd: &Path, spec: Option<&Path>) -> Result<InitResult> {
  let root = cwd
    .canonicalize()
    .context("canonicalize project directory")?;
  let spec = match spec {
    Some(path) if path.is_absolute() => path.to_path_buf(),
    Some(path) => root.join(path),
    None => root.join("SPEC.md"),
  };
  let (policy, spec_digest, created) = project::initialize(&root, &spec)?;
  let contract_state =
    match project::resolve_relative_path(&root, CONTRACT_PATH, project::ExpectedEntry::File) {
      Ok(_) => ContractState::Admitted,
      Err(project::PathResolutionError::Missing { .. }) => ContractState::Missing,
      Err(error) => return Err(anyhow::Error::new(error)),
    };
  Ok(InitResult {
    schema_version: 1,
    initialized: true,
    created,
    spec_path: policy.spec_path,
    spec_digest,
    contract_state,
    skill_path: SKILL_PATH.into(),
  })
}

fn status(cwd: &Path) -> Result<StatusResult> {
  let root = discover_root(cwd)?;
  let policy = load_policy(&root)?;
  let spec_digest = specification_digest(&root, &policy)?;
  let policy_digest = policy_digest(&policy)?;
  let contract = load_contract_optional(&root)?;
  let contract_digest = contract.as_ref().map(canonical_digest).transpose()?;
  let contract_state = match &contract {
    Some(contract)
      if contract.spec_digest != spec_digest || contract.policy_digest != policy_digest =>
    {
      ContractState::Stale
    }
    Some(_) => ContractState::Admitted,
    None if has_pending_proposal(&root)? => ContractState::PendingApproval,
    None => ContractState::Missing,
  };
  let audit = AuditState::load(&root)?;
  let last = audit.gates.last();
  Ok(StatusResult {
    schema_version: 1,
    initialized: true,
    spec_path: Some(policy.spec_path),
    spec_digest: Some(spec_digest),
    policy_digest: Some(policy_digest),
    contract_state,
    contract_digest,
    last_gated_authority_id: last.map(|gate| gate.authority_id.clone()),
    candidate_surface_configured: !policy.candidate.include.is_empty(),
    last_gated_candidate_id: last.map(|gate| gate.candidate_id.clone()),
    last_verdict: last.map(|gate| gate.verdict.clone()),
    unresolved_obligations: last
      .map(|gate| {
        gate
          .obligations
          .iter()
          .filter(|item| item.state != ObligationState::ContractSatisfied)
          .cloned()
          .collect()
      })
      .unwrap_or_default(),
  })
}

fn propose(cwd: &Path, input: ContractProposalInput) -> Result<ProposalResult> {
  let root = initialized_root(cwd)?;
  let policy = load_policy(&root)?;
  validate_authority_sources(&root, &policy)?;
  validate_candidate_surface(&policy.candidate)?;
  let spec_digest = specification_digest(&root, &policy)?;
  let policy_digest = policy_digest(&policy)?;
  let proposal = canonicalize_proposal(input, &policy).map_err(|error| {
    anyhow::Error::new(TenetError::new(
      "proposal_invalid",
      format!("contract proposal is invalid: {error}"),
    ))
  })?;
  if proposal.spec_digest != spec_digest {
    return Err(anyhow::Error::new(TenetError::new(
      "specification_stale",
      format!(
        "proposal specification digest `{}` does not match current `{spec_digest}`",
        proposal.spec_digest
      ),
    )));
  }
  if proposal.policy_digest != policy_digest {
    return Err(anyhow::Error::new(TenetError::new(
      "policy_stale",
      format!(
        "proposal policy digest `{}` does not match current `{policy_digest}`",
        proposal.policy_digest
      ),
    )));
  }
  let proposal_digest = canonical_digest(&proposal)?;
  let proposal_id = format!(
    "proposal-{}",
    digest_key(&proposal_digest)
      .chars()
      .take(16)
      .collect::<String>()
  );
  let record = ProposalRecord {
    proposal_id: proposal_id.clone(),
    proposal_digest: proposal_digest.clone(),
    proposal,
  };
  let mut encoded = serde_json::to_vec_pretty(&record)?;
  encoded.push(b'\n');
  let proposals = project::prepare_relative_directory(&root, ".tenet/proposals")?;
  fs::create_dir_all(proposals)?;
  atomic_write(&proposal_path(&root, &proposal_digest), &encoded)?;
  let (verification_profile, warnings) = analyze_verification(&record.proposal, &policy);
  Ok(ProposalResult {
    schema_version: 1,
    proposal_id,
    proposal_digest,
    approval_required: true,
    proposal: record.proposal,
    verification_profile,
    warnings,
  })
}

fn approve(cwd: &Path, proposal_id: &str, digest: &str) -> Result<ApprovalResult> {
  let root = initialized_root(cwd)?;
  let relative = proposal_relative_path(digest)?;
  let proposal =
    match project::resolve_relative_path(&root, &relative, project::ExpectedEntry::File) {
      Ok(path) => path,
      Err(project::PathResolutionError::Missing { .. }) => {
        return Err(anyhow::Error::new(TenetError::new(
          "proposal_missing",
          format!("pending proposal `{digest}` not found; submit a contract proposal first"),
        )));
      }
      Err(error) => return Err(anyhow::Error::new(error)),
    };
  let record: ProposalRecord = serde_json::from_slice(&fs::read(proposal)?).map_err(|error| {
    anyhow::Error::new(TenetError::new(
      "proposal_invalid",
      format!("pending proposal is invalid: {error}"),
    ))
  })?;
  if record.proposal_id != proposal_id
    || record.proposal_digest != digest
    || canonical_digest(&record.proposal)? != digest
  {
    return Err(anyhow::Error::new(TenetError::new(
      "approval_identity_mismatch",
      "approval identity does not match the stored proposal",
    )));
  }
  let policy = load_policy(&root)?;
  validate_authority_sources(&root, &policy)?;
  validate_proposal(&record.proposal, &policy)?;
  if record.proposal.spec_digest != specification_digest(&root, &policy)?
    || record.proposal.policy_digest != policy_digest(&policy)?
  {
    return Err(anyhow::Error::new(TenetError::new(
      "contract_stale",
      "proposal is stale because the specification or verification policy changed",
    )));
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

fn authority_seal(cwd: &Path) -> Result<AuthoritySealResult> {
  let root = initialized_root(cwd)?;
  let policy = load_policy(&root)?;
  validate_candidate_surface(&policy.candidate)?;
  let Some(contract) = load_contract_optional(&root)? else {
    return Err(anyhow::Error::new(TenetError::new(
      "contract_stale",
      "no admitted completion contract",
    )));
  };
  let spec_digest = specification_digest(&root, &policy)?;
  let policy_digest_value = policy_digest(&policy)?;
  if contract.spec_digest != spec_digest {
    return Err(anyhow::Error::new(TenetError::new(
      "specification_stale",
      "admitted contract does not match current specification",
    )));
  }
  if contract.policy_digest != policy_digest_value {
    return Err(anyhow::Error::new(TenetError::new(
      "policy_stale",
      "admitted contract does not match current policy",
    )));
  }
  validate_admitted_contract(&contract, &policy)?;
  validate_authority_sources(&root, &policy)?;
  let contract_digest = canonical_digest(&contract)?;
  let store = ContentStore::open(&root)?;
  let store_root =
    project::resolve_relative_path(&root, ".tenet/store", project::ExpectedEntry::Directory)
      .map_err(anyhow::Error::new)?;
  let stage = tempfile::Builder::new()
    .prefix("authority-")
    .tempdir_in(store_root)?;
  copy_authority_surface(&root, stage.path(), &policy)?;
  let sealed_policy = load_policy(stage.path())?;
  let sealed_spec_digest = specification_digest(stage.path(), &sealed_policy)?;
  let sealed_contract = load_contract_optional(stage.path())?.ok_or_else(|| {
    anyhow::Error::new(TenetError::new(
      "authority_surface_changed",
      "admitted contract disappeared while sealing authority",
    ))
  })?;
  let sealed_contract_digest = canonical_digest(&sealed_contract)?;
  if policy_digest(&sealed_policy)? != policy_digest_value
    || sealed_spec_digest != spec_digest
    || sealed_contract_digest != contract_digest
  {
    return Err(anyhow::Error::new(TenetError::new(
      "authority_surface_changed",
      "authority policy, specification, or contract changed while sealing",
    )));
  }
  validate_authority_sources(stage.path(), &sealed_policy)?;
  let snapshot_id = store.capture(stage.path(), |_| false)?;
  let authority_id = AuthorityId(snapshot_id);
  Ok(AuthoritySealResult {
    schema_version: 1,
    authority_id,
    specification_digest: spec_digest,
    policy_digest: policy_digest_value,
    contract_digest,
    oracle_bundle_paths: sealed_policy
      .verifiers
      .iter()
      .filter_map(|verifier| verifier.oracle_path.clone())
      .collect(),
  })
}

fn candidate_capture(cwd: &Path, authority_id: AuthorityId) -> Result<CandidateCaptureResult> {
  let root = initialized_root(cwd)?;
  let store = ContentStore::open(&root)?;
  let authority = store
    .materialize(&authority_id.0)
    .map_err(anyhow::Error::new)?;
  let policy_path =
    project::resolve_relative_path(authority.path(), CONFIG_PATH, project::ExpectedEntry::File)
      .map_err(anyhow::Error::new)?;
  let policy_text = fs::read_to_string(policy_path)?;
  let policy: VerificationPolicy = toml::from_str(&policy_text).map_err(|error| {
    anyhow::Error::new(TenetError::new(
      "policy_invalid",
      format!("sealed authority policy is invalid: {error}"),
    ))
  })?;
  validate_policy(&policy).map_err(|error| {
    anyhow::Error::new(TenetError::new(
      "policy_invalid",
      format!("sealed authority policy is invalid: {error}"),
    ))
  })?;
  validate_candidate_surface(&policy.candidate)?;
  let candidate_root = project::resolve_relative_path(
    &root,
    &policy.candidate.root,
    project::ExpectedEntry::Directory,
  )
  .map_err(anyhow::Error::new)?;
  let candidate_id = store.capture_selected(
    &candidate_root,
    &policy.candidate.include,
    &policy.candidate.exclude,
  )?;
  Ok(CandidateCaptureResult {
    schema_version: 1,
    candidate_id: CandidateId(candidate_id),
  })
}

fn gate(cwd: &Path, authority_id: AuthorityId, candidate_id: CandidateId) -> Result<GateResult> {
  let root = initialized_root(cwd)?;
  let store = ContentStore::open(&root)?;
  let authority = match store.materialize(&authority_id.0) {
    Ok(snapshot) => snapshot,
    Err(error) => {
      let (code, message) = content_failure(&error);
      return finish_gate(
        &root,
        infrastructure_gate(authority_id, candidate_id, code, &message),
        Vec::new(),
      );
    }
  };
  let candidate = match store.materialize(&candidate_id.0) {
    Ok(snapshot) => snapshot,
    Err(error) => {
      let (code, message) = content_failure(&error);
      return finish_gate(
        &root,
        infrastructure_gate(authority_id, candidate_id, code, &message),
        Vec::new(),
      );
    }
  };
  gate_materialized(
    &root,
    &store,
    authority_id,
    candidate_id,
    authority,
    candidate,
  )
}

fn gate_materialized(
  root: &Path,
  store: &ContentStore,
  authority_id: AuthorityId,
  candidate_id: CandidateId,
  authority: MaterializedSnapshot,
  candidate: MaterializedSnapshot,
) -> Result<GateResult> {
  let authority_root = authority.path();
  let policy: VerificationPolicy = match fs::read_to_string(authority_root.join(CONFIG_PATH))
    .context("read sealed authority policy")
    .and_then(|text| toml::from_str(&text).context("parse sealed authority policy"))
  {
    Ok(policy) => policy,
    Err(error) => {
      return finish_gate(
        root,
        control_plane_gate(
          authority_id,
          candidate_id,
          BlockerCode::ProjectNotInitialized,
          &format!("sealed authority is malformed: {error:#}"),
        ),
        Vec::new(),
      );
    }
  };
  if let Err(error) = validate_policy(&policy) {
    return finish_gate(
      root,
      control_plane_gate(
        authority_id,
        candidate_id,
        BlockerCode::PolicyStale,
        &format!("sealed authority policy invalid: {error}"),
      ),
      Vec::new(),
    );
  }
  let spec_bytes = match fs::read(authority_root.join(&policy.spec_path)) {
    Ok(bytes) => bytes,
    Err(error) => {
      return finish_gate(
        root,
        control_plane_gate(
          authority_id,
          candidate_id,
          BlockerCode::SpecificationStale,
          &format!("sealed authority specification missing: {error}"),
        ),
        Vec::new(),
      );
    }
  };
  let spec_digest = bytes_digest(&spec_bytes);
  let contract: AdmittedContract = match fs::read(authority_root.join(CONTRACT_PATH))
    .context("read sealed contract")
    .and_then(|bytes| serde_json::from_slice(&bytes).context("parse sealed contract"))
  {
    Ok(contract) => contract,
    Err(error) => {
      return finish_gate(
        root,
        control_plane_gate(
          authority_id,
          candidate_id,
          BlockerCode::ContractMissing,
          &format!("sealed authority contract missing: {error:#}"),
        ),
        Vec::new(),
      );
    }
  };
  let contract_digest = canonical_digest(&contract)?;
  let policy_digest = policy_digest(&policy)?;
  if contract.spec_digest != spec_digest {
    return finish_gate(
      root,
      mismatch_gate(
        authority_id,
        candidate_id,
        spec_digest,
        contract_digest,
        policy_digest,
        BlockerCode::SpecificationStale,
        "sealed specification differs from admitted contract",
      ),
      Vec::new(),
    );
  }
  if contract.policy_digest != policy_digest {
    return finish_gate(
      root,
      mismatch_gate(
        authority_id,
        candidate_id,
        spec_digest,
        contract_digest,
        policy_digest,
        BlockerCode::PolicyStale,
        "sealed policy differs from admitted contract",
      ),
      Vec::new(),
    );
  }
  validate_admitted_contract(&contract, &policy)?;
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
  let mut observations = BTreeMap::new();
  let mut infrastructure_errors = BTreeMap::new();
  let mut oracle_identities = BTreeMap::new();
  for verifier_id in verifier_ids {
    let verifier = policy
      .verifiers
      .iter()
      .find(|item| item.id == verifier_id)
      .context("validated verifier disappeared")?;
    match execute_configured_verifier(
      store,
      authority_root,
      candidate.path(),
      &authority_id,
      &candidate_id,
      verifier,
    ) {
      Ok((identity, executed))
        if !executed.observation.timed_out && executed.observation.exit_code.is_some() =>
      {
        oracle_identities.insert(verifier_id.into(), identity);
        observations.insert(verifier_id.into(), executed);
      }
      Ok((_, executed)) => {
        infrastructure_errors.insert(
          verifier_id.into(),
          if executed.observation.timed_out {
            "verifier timed out".into()
          } else {
            "verifier terminated without exit code".into()
          },
        );
      }
      Err(error) => {
        infrastructure_errors.insert(verifier_id.into(), format!("{error:#}"));
      }
    }
  }
  let mut artifacts = Vec::new();
  for obligation in contract.obligations() {
    record_evidence(
      &mut artifacts,
      obligation.id.clone(),
      &obligation.evidence_contract.claim.verifier_id,
      None,
      &policy,
      &observations,
      &oracle_identities,
      &authority_id,
      &candidate_id,
      &spec_digest,
      &contract_digest,
      &policy_digest,
    )?;
    let qualified = oracle_identities
      .get(&obligation.evidence_contract.claim.verifier_id)
      .cloned();
    for assurance in &obligation.evidence_contract.assurances {
      if let Some(qualified) = qualified.clone() {
        record_evidence(
          &mut artifacts,
          obligation.id.clone(),
          &assurance.verifier_id,
          Some((assurance.id.clone(), assurance.criterion.clone(), qualified)),
          &policy,
          &observations,
          &oracle_identities,
          &authority_id,
          &candidate_id,
          &spec_digest,
          &contract_digest,
          &policy_digest,
        )?;
      }
    }
  }
  let context = DerivationContext {
    authority_id: &authority_id,
    candidate_id: &candidate_id,
    spec_digest: &spec_digest,
    contract_digest: &contract_digest,
    policy_digest: &policy_digest,
    contract: &contract,
    policy: &policy,
    oracle_identities: &oracle_identities,
    infrastructure_errors: &infrastructure_errors,
  };
  let obligations = contract
    .obligations()
    .map(|obligation| {
      let mut result = derive_obligation_state(&context, &obligation.id, &artifacts);
      if result.state == ObligationState::Contradicted {
        result.blockers.push(Blocker {
          code: BlockerCode::VerifierFailed,
          obligation_id: Some(obligation.id.clone()),
          verifier_id: Some(obligation.evidence_contract.claim.verifier_id.clone()),
          message: "configured primary verifier exited unsuccessfully".into(),
        });
      }
      result
    })
    .collect::<Vec<_>>();
  let result = GateResult {
    schema_version: 1,
    authority_id,
    candidate_id,
    spec_digest,
    contract_digest,
    policy_digest,
    verdict: derive_completion(&obligations),
    blockers: obligations
      .iter()
      .flat_map(|obligation| obligation.blockers.clone())
      .collect(),
    obligations,
  };
  finish_gate(root, result, artifacts)
}

#[allow(clippy::too_many_arguments)]
fn record_evidence(
  artifacts: &mut Vec<EvidenceArtifact>,
  obligation_id: tenet_domain::contract::ObligationId,
  verifier_id: &str,
  assurance: Option<(tenet_domain::contract::AssuranceId, String, OracleIdentity)>,
  policy: &VerificationPolicy,
  observations: &BTreeMap<String, ExecutedVerifier>,
  identities: &BTreeMap<String, OracleIdentity>,
  authority_id: &AuthorityId,
  candidate_id: &CandidateId,
  spec_digest: &str,
  contract_digest: &str,
  policy_digest: &str,
) -> Result<()> {
  let Some(executed) = observations.get(verifier_id) else {
    return Ok(());
  };
  if executed.observation.exit_code == Some(125) {
    return Ok(());
  }
  let verifier = policy
    .verifiers
    .iter()
    .find(|item| item.id == verifier_id)
    .context("validated verifier disappeared")?;
  let evidence = VerifierEvidence {
    obligation_id,
    authority_id: authority_id.clone(),
    candidate_id: candidate_id.clone(),
    verifier_id: verifier_id.into(),
    policy_digest: policy_digest.into(),
    spec_digest: spec_digest.into(),
    contract_digest: contract_digest.into(),
    authority: artifact_authority(verifier.authority),
    oracle_identity: identities
      .get(verifier_id)
      .context("executed verifier lacks oracle identity")?
      .clone(),
    provenance: ArtifactProvenance::TenetLocalVerifier,
    execution: executed.execution.clone(),
    effect: evidence_effect(executed.observation.exit_code),
    validity: ArtifactValidity::Valid,
    dependency_surface: DependencySurface::CandidateSnapshot,
    observation: executed.observation.clone(),
  };
  match assurance {
    Some((assurance_id, criterion, qualified_oracle_identity)) => {
      artifacts.push(EvidenceArtifact::OracleAssurance {
        assurance_id,
        assurance_criterion: criterion,
        qualified_oracle_identity,
        evidence,
      })
    }
    None => artifacts.push(EvidenceArtifact::Claim { evidence }),
  }
  Ok(())
}

fn execute_configured_verifier(
  store: &ContentStore,
  authority_root: &Path,
  candidate_root: &Path,
  authority_id: &AuthorityId,
  candidate_id: &CandidateId,
  verifier: &VerifierSpec,
) -> Result<(OracleIdentity, ExecutedVerifier)> {
  let definition_digest = canonical_digest(verifier)?;
  let (identity, oracle) = match verifier.authority {
    VerifierAuthority::Project => (
      OracleIdentity::Project {
        verifier_id: verifier.id.clone(),
        candidate_id: candidate_id.clone(),
        definition_digest,
      },
      None,
    ),
    VerifierAuthority::AuthoritySnapshot => {
      let bundle_path = verifier
        .oracle_path
        .as_deref()
        .context("validated authority verifier lost oracle path")?;
      let manifest = store.manifest(&authority_id.0)?;
      let bundle_content_id = subtree_content_id(&manifest.entries, bundle_path)?;
      let executable_path = verifier
        .oracle_executable_path()
        .context("validated authority verifier lost executable path")?;
      let executable_path = executable_path
        .to_str()
        .context("oracle executable path must be UTF-8")?;
      let executable_content_id = manifest
        .entries
        .iter()
        .find(|entry| entry.path == executable_path && entry.kind == EntryKind::File)
        .and_then(|entry| entry.content_id.clone())
        .context("sealed authority executable missing")?;
      (
        OracleIdentity::AuthoritySnapshot {
          verifier_id: verifier.id.clone(),
          authority_id: authority_id.clone(),
          bundle_path: bundle_path.into(),
          bundle_content_id,
          executable_content_id,
          definition_digest,
        },
        Some(authority_root.join(bundle_path)),
      )
    }
  };
  run_verifier(
    candidate_root,
    oracle.as_deref(),
    verifier,
    authority_id,
    candidate_id,
    &identity,
  )
  .map(|executed| (identity, executed))
}

fn subtree_content_id(entries: &[project::TreeEntry], path: &str) -> Result<ContentObjectId> {
  let prefix = format!("{path}/");
  let entries = entries
    .iter()
    .filter(|entry| entry.path == path || entry.path.starts_with(&prefix))
    .collect::<Vec<_>>();
  if entries
    .first()
    .is_none_or(|entry| entry.kind != EntryKind::Directory)
  {
    bail!("sealed authority oracle bundle is not a directory");
  }
  ContentObjectId::new(canonical_digest(&entries)?).map_err(anyhow::Error::msg)
}
fn evidence_effect(code: Option<i32>) -> EvidenceEffect {
  match code {
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

fn validate_authority_sources(root: &Path, policy: &VerificationPolicy) -> Result<()> {
  for verifier in policy
    .verifiers
    .iter()
    .filter(|verifier| verifier.authority == VerifierAuthority::AuthoritySnapshot)
  {
    let bundle_path = verifier
      .oracle_path
      .as_deref()
      .context("authority_snapshot verifier has no oracle_path")?;
    let bundle =
      project::resolve_relative_path(root, bundle_path, project::ExpectedEntry::Directory)
        .map_err(|error| authority_path_error(verifier, bundle_path, "oracle_bundle", error))?;
    let executable =
      project::resolve_relative_path(&bundle, &verifier.argv[0], project::ExpectedEntry::File)
        .map_err(|error| {
          authority_path_error(verifier, &verifier.argv[0], "oracle_executable", error)
        })?;
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      if fs::symlink_metadata(&executable)?.permissions().mode() & 0o111 == 0 {
        return Err(anyhow::Error::new(
          TenetError::new(
            "oracle_executable_not_executable",
            format!("oracle executable `{}` is not executable", verifier.argv[0]),
          )
          .with_context(
            Some(verifier.id.clone()),
            Some(verifier.argv[0].clone()),
            None,
          ),
        ));
      }
    }
    let _cwd =
      project::resolve_relative_path(&bundle, &verifier.cwd, project::ExpectedEntry::Directory)
        .map_err(|error| authority_path_error(verifier, &verifier.cwd, "oracle_cwd", error))?;
  }
  Ok(())
}

fn authority_path_error(
  verifier: &VerifierSpec,
  path: &str,
  role: &str,
  error: project::PathResolutionError,
) -> anyhow::Error {
  let code = match (&error, role) {
    (project::PathResolutionError::Missing { .. }, "oracle_bundle") => "oracle_bundle_missing",
    (project::PathResolutionError::NotDirectory { .. }, "oracle_bundle") => {
      "oracle_bundle_not_directory"
    }
    (project::PathResolutionError::Missing { .. }, "oracle_executable") => {
      "oracle_executable_missing"
    }
    (project::PathResolutionError::NotFile { .. }, "oracle_executable")
    | (project::PathResolutionError::Special { .. }, "oracle_executable") => {
      "oracle_executable_not_file"
    }
    (project::PathResolutionError::Missing { .. }, "oracle_cwd") => "oracle_cwd_missing",
    (project::PathResolutionError::NotDirectory { .. }, "oracle_cwd")
    | (project::PathResolutionError::Special { .. }, "oracle_cwd") => "oracle_cwd_not_directory",
    (project::PathResolutionError::UnsupportedSymlink { .. }, _) => "unsupported_symlink",
    (project::PathResolutionError::PathEscape { .. }, _)
    | (project::PathResolutionError::Invalid, _) => "path_escape",
    _ => "internal_error",
  };
  anyhow::Error::new(
    TenetError::new(code, format!("{role} path `{path}` is invalid: {error}")).with_context(
      Some(verifier.id.clone()),
      Some(path.into()),
      None,
    ),
  )
}

fn copy_authority_surface(root: &Path, stage: &Path, policy: &VerificationPolicy) -> Result<()> {
  for path in [CONFIG_PATH, CONTRACT_PATH, policy.spec_path.as_str()] {
    copy_authority_path(root, stage, path)?;
  }
  for path in policy
    .verifiers
    .iter()
    .filter_map(|verifier| verifier.oracle_path.as_deref())
  {
    copy_authority_path(root, stage, path)?;
  }
  Ok(())
}

fn copy_authority_path(root: &Path, stage: &Path, path: &str) -> Result<()> {
  let source = project::resolve_relative_path(root, path, project::ExpectedEntry::Any)
    .map_err(|error| authority_copy_path_error(path, error))?;
  let destination = stage.join(path);
  let metadata = fs::symlink_metadata(&source)?;
  if metadata.is_file() {
    fs::create_dir_all(
      destination
        .parent()
        .context("authority path has no parent")?,
    )?;
    fs::copy(source, destination)?;
  } else if metadata.is_dir() {
    copy_directory(root, path, &destination)?;
  } else {
    return Err(authority_copy_path_error(
      path,
      project::PathResolutionError::Special { path: path.into() },
    ));
  }
  Ok(())
}

fn copy_directory(root: &Path, relative: &str, destination: &Path) -> Result<()> {
  let source = project::resolve_relative_path(root, relative, project::ExpectedEntry::Directory)
    .map_err(|error| authority_copy_path_error(relative, error))?;
  fs::create_dir_all(destination)?;
  let mut items = fs::read_dir(&source)?.collect::<std::result::Result<Vec<_>, _>>()?;
  items.sort_by_key(|item| item.file_name());
  for item in items {
    let name = item
      .file_name()
      .to_str()
      .context("authority paths must be UTF-8")?
      .to_owned();
    let child = if relative == "." {
      name.clone()
    } else {
      format!("{relative}/{name}")
    };
    let child_source = project::resolve_relative_path(root, &child, project::ExpectedEntry::Any)
      .map_err(|error| authority_copy_path_error(&child, error))?;
    let child_destination = destination.join(&name);
    let metadata = fs::symlink_metadata(&child_source)?;
    if metadata.is_dir() {
      copy_directory(root, &child, &child_destination)?;
    } else if metadata.is_file() {
      fs::copy(child_source, child_destination)?;
    } else {
      return Err(authority_copy_path_error(
        &child,
        project::PathResolutionError::Special {
          path: child.clone(),
        },
      ));
    }
  }
  Ok(())
}

fn authority_copy_path_error(path: &str, error: project::PathResolutionError) -> anyhow::Error {
  let code = match error {
    project::PathResolutionError::UnsupportedSymlink { .. } => "unsupported_symlink",
    project::PathResolutionError::PathEscape { .. } | project::PathResolutionError::Invalid => {
      "path_escape"
    }
    project::PathResolutionError::Missing { .. } => "content_object_missing",
    project::PathResolutionError::Special { .. } => "unsupported_filesystem_entry",
    _ => "internal_error",
  };
  anyhow::Error::new(
    TenetError::new(
      code,
      format!("authority path `{path}` cannot be sealed: {error}"),
    )
    .with_context(None, Some(path.into()), None),
  )
}

fn evidence(
  cwd: &Path,
  authority_id: &AuthorityId,
  candidate_id: &CandidateId,
) -> Result<EvidenceResult> {
  let root = initialized_root(cwd)?;
  let audit = AuditState::load(&root)?;
  Ok(EvidenceResult {
    schema_version: 1,
    authority_id: authority_id.clone(),
    candidate_id: candidate_id.clone(),
    artifacts: audit
      .evidence
      .into_iter()
      .filter(|item| {
        let evidence = item.evidence();
        evidence.authority_id == *authority_id && evidence.candidate_id == *candidate_id
      })
      .collect(),
    gates: audit
      .gates
      .into_iter()
      .filter(|item| item.authority_id == *authority_id && item.candidate_id == *candidate_id)
      .collect(),
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
  authority_id: AuthorityId,
  candidate_id: CandidateId,
  code: BlockerCode,
  message: &str,
) -> GateResult {
  mismatch_gate(
    authority_id,
    candidate_id,
    String::new(),
    String::new(),
    String::new(),
    code,
    message,
  )
}
fn content_failure(error: &ContentStoreError) -> (BlockerCode, String) {
  let code = match error {
    ContentStoreError::Missing { .. } => BlockerCode::ContentObjectMissing,
    ContentStoreError::Integrity { .. } => BlockerCode::ContentIntegrityFailure,
    ContentStoreError::Materialization { .. }
    | ContentStoreError::MaterializationMessage { .. } => BlockerCode::ContentMaterializationFailed,
  };
  (code, error.to_string())
}

fn infrastructure_gate(
  authority_id: AuthorityId,
  candidate_id: CandidateId,
  code: BlockerCode,
  message: &str,
) -> GateResult {
  mismatch_gate_with_verdict(
    authority_id,
    candidate_id,
    String::new(),
    String::new(),
    String::new(),
    code,
    message,
    Verdict::InfrastructureError,
  )
}
fn mismatch_gate(
  authority_id: AuthorityId,
  candidate_id: CandidateId,
  spec_digest: String,
  contract_digest: String,
  policy_digest: String,
  code: BlockerCode,
  message: &str,
) -> GateResult {
  mismatch_gate_with_verdict(
    authority_id,
    candidate_id,
    spec_digest,
    contract_digest,
    policy_digest,
    code,
    message,
    Verdict::NotDone,
  )
}

#[expect(
  clippy::too_many_arguments,
  reason = "Gate metadata and blocker fields are kept explicit at this boundary."
)]
fn mismatch_gate_with_verdict(
  authority_id: AuthorityId,
  candidate_id: CandidateId,
  spec_digest: String,
  contract_digest: String,
  policy_digest: String,
  code: BlockerCode,
  message: &str,
  verdict: Verdict,
) -> GateResult {
  GateResult {
    schema_version: 1,
    authority_id,
    candidate_id,
    spec_digest,
    contract_digest,
    policy_digest,
    verdict,
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
  validate_proposal(
    &ContractProposal {
      schema_version: contract.schema_version,
      spec_digest: contract.spec_digest.clone(),
      policy_digest: contract.policy_digest.clone(),
      requirements: contract.requirements.clone(),
    },
    policy,
  )
  .context("validate admitted contract")
}
fn load_contract_optional(root: &Path) -> Result<Option<AdmittedContract>> {
  let path = match project::resolve_relative_path(root, CONTRACT_PATH, project::ExpectedEntry::File)
  {
    Ok(path) => path,
    Err(project::PathResolutionError::Missing { .. }) => return Ok(None),
    Err(error) => return Err(anyhow::Error::new(error)),
  };
  serde_json::from_slice(&fs::read(path)?)
    .context("parse admitted completion contract")
    .map(Some)
}
fn initialized_root(cwd: &Path) -> Result<PathBuf> {
  discover_root(cwd)
}
fn proposal_relative_path(digest: &str) -> Result<String> {
  let normalized = ContentObjectId::new(digest.to_owned())
    .map_err(|message| anyhow::Error::new(TenetError::new("proposal_digest_invalid", message)))?;
  if normalized.0 != digest {
    return Err(anyhow::Error::new(TenetError::new(
      "proposal_digest_invalid",
      "proposal digest must use lowercase canonical sha256 form",
    )));
  }
  Ok(format!("{TENET_DIR}/proposals/{}.json", digest_key(digest)))
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
  let directory = match project::resolve_relative_path(
    root,
    ".tenet/proposals",
    project::ExpectedEntry::Directory,
  ) {
    Ok(directory) => directory,
    Err(project::PathResolutionError::Missing { .. }) => return Ok(false),
    Err(error) => return Err(anyhow::Error::new(error)),
  };
  Ok(fs::read_dir(directory)?.next().transpose()?.is_some())
}
