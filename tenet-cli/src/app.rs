use std::{
  collections::BTreeMap,
  fs,
  path::{Path, PathBuf},
  process::ExitCode,
};

use anyhow::{bail, Context, Result};
use schemars::schema_for;
use tenet_domain::{
  completion::{
    derive_completion, derive_obligation_state, Blocker, BlockerCode, DerivationContext,
    ObligationResult, ObligationState, Verdict,
  },
  contract::{
    validate_proposal, AdmittedContract, ContractProposal, ProposalRecord, VerificationObligation,
  },
  digest::canonical_digest,
  evidence::{
    ArtifactAuthority, ArtifactProvenance, ArtifactValidity, DependencySurface, EvidenceArtifact,
    EvidenceEffect,
  },
  policy::{VerificationPolicy, VerifierAuthority},
};

use crate::{
  audit::AuditState,
  cli::{Cli, Command, ContractCommand},
  repository::{
    self, atomic_write, discover_root, load_policy, policy_digest, specification_digest,
    MaterializedRevision, CONFIG_PATH, CONTRACT_PATH, SKILL_PATH, TENET_DIR,
  },
  response::{
    ApprovalResult, ContractState, EvidenceResult, GateResult, InitResult, ProposalResult,
    StatusResult,
  },
  verifier::run_verifier,
};

pub fn run(cli: Cli) -> Result<ExitCode> {
  let cwd = cli.cwd.unwrap_or(std::env::current_dir()?);
  match cli.command {
    Command::Init { spec, json } => initialize(&cwd, &spec, json),
    Command::Status { json } => status(&cwd, json),
    Command::Contract { command } => contract(&cwd, command),
    Command::Gate { revision, json } => gate(&cwd, &revision, json),
    Command::Evidence { revision, json } => evidence(&cwd, revision.as_deref(), json),
  }
}

fn initialize(cwd: &Path, spec: &Path, json: bool) -> Result<ExitCode> {
  let root = discover_root(cwd)?;
  let spec = if spec.is_absolute() {
    spec.to_path_buf()
  } else {
    cwd.join(spec)
  };
  let (policy, spec_digest, created) = repository::initialize(&root, &spec)?;
  let result = InitResult {
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
  };
  print_value(&result, json, |value| {
    println!("initialized: {}", value.initialized);
    println!("specification: {} ({})", value.spec_path, value.spec_digest);
    println!("contract: {:?}", value.contract_state);
    println!("skill: {}", value.skill_path);
  })?;
  Ok(ExitCode::SUCCESS)
}

fn status(cwd: &Path, json: bool) -> Result<ExitCode> {
  let root = discover_root(cwd)?;
  if !root.join(CONFIG_PATH).exists() {
    let result = StatusResult {
      schema_version: 1,
      initialized: false,
      spec_path: None,
      spec_digest: None,
      policy_digest: None,
      contract_state: ContractState::Missing,
      contract_digest: None,
      last_gated_revision: None,
      last_verdict: None,
      unresolved_obligations: Vec::new(),
    };
    print_status(&result, json)?;
    return Ok(ExitCode::from(2));
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
  let result = StatusResult {
    schema_version: 1,
    initialized: true,
    spec_path: Some(policy.spec_path),
    spec_digest: Some(current_spec_digest),
    policy_digest: Some(current_policy_digest),
    contract_state,
    contract_digest,
    last_gated_revision: last.map(|gate| gate.revision.clone()),
    last_verdict: last.map(|gate| gate.verdict.clone()),
    unresolved_obligations,
  };
  print_status(&result, json)?;
  Ok(ExitCode::SUCCESS)
}

fn contract(cwd: &Path, command: ContractCommand) -> Result<ExitCode> {
  match command {
    ContractCommand::Schema { .. } => {
      let schema = schema_for!(ContractProposal);
      println!("{}", serde_json::to_string_pretty(&schema)?);
      Ok(ExitCode::SUCCESS)
    }
    ContractCommand::Propose { file, json } => propose(cwd, &file, json),
    ContractCommand::Approve {
      proposal,
      digest,
      json,
    } => approve(cwd, &proposal, &digest, json),
  }
}

fn propose(cwd: &Path, file: &Path, json: bool) -> Result<ExitCode> {
  let root = initialized_root(cwd)?;
  let policy = load_policy(&root)?;
  let current_spec_digest = specification_digest(&root, &policy)?;
  let current_policy_digest = policy_digest(&policy)?;
  let file = absolute_from(cwd, file);
  let bytes = fs::read(&file).with_context(|| format!("read proposal {}", file.display()))?;
  let proposal: ContractProposal =
    serde_json::from_slice(&bytes).context("parse contract proposal")?;
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
  let result = ProposalResult {
    schema_version: 1,
    proposal_id,
    proposal_digest: digest,
    approval_required: true,
  };
  print_value(&result, json, |value| {
    println!("proposal: {}", value.proposal_id);
    println!("digest: {}", value.proposal_digest);
    println!("approval required: true");
  })?;
  Ok(ExitCode::SUCCESS)
}

fn approve(cwd: &Path, proposal_id: &str, digest: &str, json: bool) -> Result<ExitCode> {
  let root = initialized_root(cwd)?;
  let path = proposal_path(&root, digest);
  let bytes = fs::read(&path).with_context(|| {
    format!("pending proposal `{digest}` not found; submit it with `tenet contract propose`")
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
  let result = ApprovalResult {
    schema_version: 1,
    proposal_id: admitted.proposal_id,
    proposal_digest: admitted.proposal_digest,
    contract_digest,
    contract_path: CONTRACT_PATH.into(),
  };
  print_value(&result, json, |value| {
    println!("admitted proposal: {}", value.proposal_id);
    println!("proposal digest: {}", value.proposal_digest);
    println!("contract digest: {}", value.contract_digest);
  })?;
  Ok(ExitCode::SUCCESS)
}

fn gate(cwd: &Path, revision: &str, json: bool) -> Result<ExitCode> {
  let root = discover_root(cwd)?;
  let revision = repository::resolve_revision(&root, revision)?;
  let materialized = MaterializedRevision::create(&root, &revision)?;
  let checkout = materialized.path();

  if !checkout.join(CONFIG_PATH).exists() {
    let result = control_plane_gate(
      revision,
      BlockerCode::RepositoryNotInitialized,
      "selected revision is not Tenet-enabled",
    );
    return finish_gate(&root, result, Vec::new(), json);
  }
  let policy = load_policy(checkout)?;
  let spec_digest = specification_digest(checkout, &policy)?;
  let policy_digest = policy_digest(&policy)?;
  let Some(contract) = load_contract_optional(checkout)? else {
    let mut result = control_plane_gate(
      revision,
      BlockerCode::ContractMissing,
      "selected revision has no admitted completion contract",
    );
    result.spec_digest = spec_digest;
    result.policy_digest = policy_digest;
    return finish_gate(&root, result, Vec::new(), json);
  };
  let contract_digest = canonical_digest(&contract)?;
  if contract.spec_digest != spec_digest {
    let result = mismatch_gate(
      revision,
      spec_digest,
      contract_digest,
      policy_digest,
      BlockerCode::SpecificationChanged,
      "specification digest differs from the admitted contract",
    );
    return finish_gate(&root, result, Vec::new(), json);
  }
  if contract.policy_digest != policy_digest {
    let result = mismatch_gate(
      revision,
      spec_digest,
      contract_digest,
      policy_digest,
      BlockerCode::PolicyChanged,
      "verification policy digest differs from the admitted contract",
    );
    return finish_gate(&root, result, Vec::new(), json);
  }
  validate_admitted_contract(&contract, &policy)?;

  let mut observations = BTreeMap::new();
  let mut infrastructure_errors = BTreeMap::new();
  for obligation in contract.obligations() {
    let verifier_id = &obligation.evidence_contract.verifier_id;
    if observations.contains_key(verifier_id) || infrastructure_errors.contains_key(verifier_id) {
      continue;
    }
    let verifier = policy
      .verifiers
      .iter()
      .find(|item| &item.id == verifier_id)
      .context("validated verifier disappeared")?;
    match run_verifier(checkout, verifier) {
      Ok(observation) if observation.timed_out => {
        infrastructure_errors.insert(verifier_id.clone(), "verifier timed out".to_owned());
      }
      Ok(observation) if observation.exit_code.is_none() => {
        infrastructure_errors.insert(
          verifier_id.clone(),
          "verifier terminated without an exit code".to_owned(),
        );
      }
      Ok(observation) => {
        observations.insert(verifier_id.clone(), observation);
      }
      Err(error) => {
        infrastructure_errors.insert(verifier_id.clone(), format!("{error:#}"));
      }
    }
  }

  let mut artifacts = Vec::new();
  let mut obligations = Vec::new();
  let context = DerivationContext {
    revision: &revision,
    spec_digest: &spec_digest,
    contract_digest: &contract_digest,
    policy_digest: &policy_digest,
    contract: &contract,
    policy: &policy,
  };
  for obligation in contract.obligations() {
    let verifier_id = &obligation.evidence_contract.verifier_id;
    if let Some(message) = infrastructure_errors.get(verifier_id) {
      obligations.push(infrastructure_result(obligation, verifier_id, message));
      continue;
    }
    let observation = observations
      .get(verifier_id)
      .context("verifier produced neither observation nor error")?
      .clone();
    if observation.exit_code == Some(125) {
      obligations.push(derive_obligation_state(
        &context,
        &obligation.id,
        &artifacts,
      ));
      continue;
    }
    let verifier = policy
      .verifiers
      .iter()
      .find(|item| &item.id == verifier_id)
      .context("validated verifier disappeared")?;
    let effect = match observation.exit_code {
      Some(0) => EvidenceEffect::Supports,
      Some(126) => EvidenceEffect::Inconclusive,
      _ => EvidenceEffect::Contradicts,
    };
    artifacts.push(EvidenceArtifact {
      obligation_id: obligation.id.clone(),
      revision: revision.clone(),
      verifier_id: verifier_id.clone(),
      policy_digest: policy_digest.clone(),
      spec_digest: spec_digest.clone(),
      contract_digest: contract_digest.clone(),
      authority: match verifier.authority {
        VerifierAuthority::Project => ArtifactAuthority::TenetObservedProjectVerifier,
        VerifierAuthority::Protected => ArtifactAuthority::TenetObservedProtectedVerifier,
      },
      provenance: ArtifactProvenance::TenetLocalVerifier,
      effect,
      validity: ArtifactValidity::Valid,
      dependency_surface: DependencySurface::RepositoryWide,
      observation,
    });
    let mut derived = derive_obligation_state(&context, &obligation.id, &artifacts);
    if derived.state == ObligationState::Contradicted {
      derived.blockers.push(Blocker {
        code: BlockerCode::VerifierFailed,
        obligation_id: Some(obligation.id.clone()),
        verifier_id: Some(verifier_id.clone()),
        message: "configured verifier exited unsuccessfully".into(),
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
    revision,
    spec_digest,
    contract_digest,
    policy_digest,
    verdict,
    obligations,
    blockers,
  };
  finish_gate(&root, result, artifacts, json)
}

fn evidence(cwd: &Path, revision: Option<&str>, json: bool) -> Result<ExitCode> {
  let root = initialized_root(cwd)?;
  let audit = AuditState::load(&root)?;
  let artifacts = audit
    .evidence
    .into_iter()
    .filter(|item| revision.is_none_or(|revision| item.revision == revision))
    .collect();
  let gates = audit
    .gates
    .into_iter()
    .filter(|item| revision.is_none_or(|revision| item.revision == revision))
    .collect();
  let result = EvidenceResult {
    schema_version: 1,
    revision: revision.map(str::to_owned),
    artifacts,
    gates,
  };
  print_value(&result, json, |value| {
    println!("revision: {}", value.revision.as_deref().unwrap_or("all"));
    println!("artifacts: {}", value.artifacts.len());
    for gate in &value.gates {
      println!("gate: {} {:?}", gate.revision, gate.verdict);
      for blocker in &gate.blockers {
        println!("  {:?}: {}", blocker.code, blocker.message);
      }
    }
  })?;
  Ok(ExitCode::SUCCESS)
}

fn finish_gate(
  root: &Path,
  result: GateResult,
  artifacts: Vec<EvidenceArtifact>,
  json: bool,
) -> Result<ExitCode> {
  let mut audit = AuditState::load(root)?;
  audit.evidence.extend(artifacts);
  audit.gates.push(result.clone());
  audit.save(root)?;
  print_value(&result, json, |value| {
    println!("revision: {}", value.revision);
    println!("verdict: {:?}", value.verdict);
    println!("specification: {}", value.spec_digest);
    println!("contract: {}", value.contract_digest);
    println!("policy: {}", value.policy_digest);
    for blocker in &value.blockers {
      println!("blocker {:?}: {}", blocker.code, blocker.message);
    }
  })?;
  Ok(match result.verdict {
    Verdict::Done => ExitCode::SUCCESS,
    Verdict::NotDone => ExitCode::from(2),
    Verdict::Inconclusive => ExitCode::from(3),
    Verdict::InfrastructureError => ExitCode::from(4),
  })
}

fn control_plane_gate(revision: String, code: BlockerCode, message: &str) -> GateResult {
  mismatch_gate(
    revision,
    String::new(),
    String::new(),
    String::new(),
    code,
    message,
  )
}

fn mismatch_gate(
  revision: String,
  spec_digest: String,
  contract_digest: String,
  policy_digest: String,
  code: BlockerCode,
  message: &str,
) -> GateResult {
  GateResult {
    schema_version: 1,
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

fn infrastructure_result(
  obligation: &VerificationObligation,
  verifier_id: &str,
  message: &str,
) -> ObligationResult {
  ObligationResult {
    obligation_id: obligation.id.clone(),
    state: ObligationState::InfrastructureError,
    blockers: vec![Blocker {
      code: BlockerCode::VerifierInfrastructureError,
      obligation_id: Some(obligation.id.clone()),
      verifier_id: Some(verifier_id.into()),
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
    bail!("repository is not initialized; run `tenet init --spec <path>`");
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

fn absolute_from(cwd: &Path, path: &Path) -> PathBuf {
  if path.is_absolute() {
    path.to_path_buf()
  } else {
    cwd.join(path)
  }
}

fn print_status(result: &StatusResult, json: bool) -> Result<()> {
  print_value(result, json, |value| {
    println!("initialized: {}", value.initialized);
    println!("contract: {:?}", value.contract_state);
    if let Some(spec_digest) = &value.spec_digest {
      println!("specification: {spec_digest}");
    }
    if let Some(last_revision) = &value.last_gated_revision {
      println!("last gate: {} {:?}", last_revision, value.last_verdict);
    }
    println!(
      "unresolved obligations: {}",
      value.unresolved_obligations.len()
    );
  })
}

fn print_value<T: serde::Serialize>(value: &T, json: bool, human: impl FnOnce(&T)) -> Result<()> {
  if json {
    println!("{}", serde_json::to_string_pretty(value)?);
  } else {
    human(value);
  }
  Ok(())
}
