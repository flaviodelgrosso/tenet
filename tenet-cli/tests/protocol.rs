use std::{
  fs,
  path::{Path, PathBuf},
  process::Command as ProcessCommand,
  sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
  },
};

use tenet_application::{
  application::{AuthoritySubmitRequest, InitializeRequest, RequirementCheckRequest, Tenet},
  ports::{ExecutedVerifier, Repository, VerifierRun, VerifierRunner},
  response::AuthoritySubmissionResult,
};
use tenet_domain::{
  algebra::{
    AssuranceProfileId, AssuranceRequirementV1, COMPLETION_POLICY_V1, CompletionContractV1,
    CompletionPolicyId, CompletionState, Criterion, CriterionId, Evaluation, EvaluationScope,
    EvidenceControlRequirementV1, EvidenceRequirementV1, EvidenceResult, LOCAL_V1,
    PlatformInformation, RUNNER_SEMANTICS_V1, Requirement, RunnerSemanticsId, Verifier, VerifierId,
    VerifierMaterial,
  },
  authority::AdmissionId,
  completion::Verdict,
  contract::RequirementId,
  evidence::{
    AuthorityId, ExecutionEnvironmentIdentity, ExecutionProvenance, RunnerIdentity,
    VerifierObservation,
  },
  policy::{
    CandidateCapturePolicy, CommandArgument, CommandCwd, CommandSpec, EnvironmentSpec,
    ProjectConfig, VerifierAuthority, VerifierSpec,
  },
  protocol::WorkflowPhase,
  snapshot::{SnapshotSemanticsId, TreeManifest},
};
use tenet_workspace::LocalWorkspace;

#[derive(Default)]
struct RecordingRunner {
  calls: AtomicUsize,
  observed: Mutex<Vec<String>>,
  mutate_project: Option<PathBuf>,
  mutate_on_call: Option<usize>,
  mutate_view: bool,
}

impl VerifierRunner for RecordingRunner {
  fn run(&self, request: &VerifierRun<'_>) -> anyhow::Result<ExecutedVerifier> {
    let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
    let candidate_file = request.candidate_root.join("candidate.txt");
    let content = fs::read_to_string(&candidate_file)?;
    self.observed.lock().unwrap().push(content);
    if self.mutate_view {
      fs::write(candidate_file, "contaminated materialization")?;
    }
    if self.mutate_on_call == Some(call)
      && let Some(root) = &self.mutate_project
    {
      fs::write(
        root.join("candidate.txt"),
        "candidate changed during verification",
      )?;
    }
    let context = tenet_domain::algebra::ExecutionContext {
      assurance: AssuranceProfileId(LOCAL_V1.into()),
      runner_semantics: RunnerSemanticsId(RUNNER_SEMANTICS_V1.into()),
      platform: PlatformInformation {
        os: "test".into(),
        architecture: "test".into(),
      },
      resolved_program: Some("test-runner".into()),
      resolved_program_digest: None,
    };
    Ok(ExecutedVerifier {
      observation: VerifierObservation {
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        timed_out: false,
      },
      result: EvidenceResult::Pass,
      infrastructure_error: None,
      execution: ExecutionProvenance {
        runner_identity: RunnerIdentity("test-runner".into()),
        runner_semantics: context.runner_semantics.clone(),
        assurance: context.assurance.clone(),
        tenet_version: "test".into(),
        platform: context.platform.clone(),
        resolved_program: context.resolved_program.clone(),
        resolved_program_digest: None,
        oracle_identity: request.oracle_identity.clone(),
        execution_environment_identity: ExecutionEnvironmentIdentity(format!("call-{call}")),
      },
      context,
    })
  }
}
#[derive(Default)]
struct ErrorRunner {
  calls: AtomicUsize,
}

impl VerifierRunner for ErrorRunner {
  fn run(&self, _: &VerifierRun<'_>) -> anyhow::Result<ExecutedVerifier> {
    self.calls.fetch_add(1, Ordering::SeqCst);
    anyhow::bail!("runner unavailable")
  }
}

#[derive(Default)]
struct InconsistentRunner;

impl VerifierRunner for InconsistentRunner {
  fn run(&self, request: &VerifierRun<'_>) -> anyhow::Result<ExecutedVerifier> {
    let context = tenet_domain::algebra::ExecutionContext {
      assurance: AssuranceProfileId(LOCAL_V1.into()),
      runner_semantics: RunnerSemanticsId(RUNNER_SEMANTICS_V1.into()),
      platform: PlatformInformation {
        os: "test".into(),
        architecture: "test".into(),
      },
      resolved_program: None,
      resolved_program_digest: None,
    };
    Ok(ExecutedVerifier {
      observation: VerifierObservation {
        exit_code: Some(1),
        stdout: String::new(),
        stderr: String::new(),
        timed_out: false,
      },
      result: EvidenceResult::Pass,
      infrastructure_error: None,
      execution: ExecutionProvenance {
        runner_identity: RunnerIdentity("inconsistent".into()),
        runner_semantics: context.runner_semantics.clone(),
        assurance: context.assurance.clone(),
        tenet_version: "test".into(),
        platform: context.platform.clone(),
        resolved_program: None,
        resolved_program_digest: None,
        oracle_identity: request.oracle_identity.clone(),
        execution_environment_identity: ExecutionEnvironmentIdentity("inconsistent".into()),
      },
      context,
    })
  }
}

/// A dishonest runner that over-claims `PROTECTED_V1` for a `local` verifier.
#[derive(Default)]
struct OverclaimingRunner;

impl VerifierRunner for OverclaimingRunner {
  fn run(&self, request: &VerifierRun<'_>) -> anyhow::Result<ExecutedVerifier> {
    let context = tenet_domain::algebra::ExecutionContext {
      assurance: AssuranceProfileId(tenet_domain::algebra::PROTECTED_V1.into()),
      runner_semantics: RunnerSemanticsId(RUNNER_SEMANTICS_V1.into()),
      platform: PlatformInformation {
        os: "test".into(),
        architecture: "test".into(),
      },
      resolved_program: Some("test-runner".into()),
      resolved_program_digest: None,
    };
    Ok(ExecutedVerifier {
      observation: VerifierObservation {
        exit_code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        timed_out: false,
      },
      result: EvidenceResult::Pass,
      infrastructure_error: None,
      execution: ExecutionProvenance {
        runner_identity: RunnerIdentity("test-runner".into()),
        runner_semantics: context.runner_semantics.clone(),
        assurance: context.assurance.clone(),
        tenet_version: "test".into(),
        platform: context.platform.clone(),
        resolved_program: context.resolved_program.clone(),
        resolved_program_digest: None,
        oracle_identity: request.oracle_identity.clone(),
        execution_environment_identity: ExecutionEnvironmentIdentity("overclaim".into()),
      },
      context,
    })
  }
}

struct Fixture {
  directory: tempfile::TempDir,
  tenet: Tenet,
  runner: Arc<RecordingRunner>,
}

fn admission_secret() -> Vec<u8> {
  b"trusted-admission-secret-0123456789abcdef".to_vec()
}

fn hex_secret() -> String {
  admission_secret()
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect()
}

impl Fixture {
  fn new(mutate_on_call: Option<usize>) -> Self {
    Self::configured(mutate_on_call, false)
  }

  fn configured(mutate_on_call: Option<usize>, mutate_view: bool) -> Self {
    Self::with_secret(mutate_on_call, mutate_view, Some(admission_secret()))
  }

  fn without_secret() -> Self {
    Self::with_secret(None, false, None)
  }

  fn with_secret(
    mutate_on_call: Option<usize>,
    mutate_view: bool,
    admission_secret: Option<Vec<u8>>,
  ) -> Self {
    let directory = tempfile::tempdir().expect("temporary repository");
    let root = directory.path().to_path_buf();
    let runner = Arc::new(RecordingRunner {
      calls: AtomicUsize::new(0),
      observed: Mutex::new(Vec::new()),
      mutate_project: mutate_on_call.map(|_| root.clone()),
      mutate_on_call,
      mutate_view,
    });
    let tenet = Tenet::new(
      root.clone(),
      Arc::new(LocalWorkspace),
      runner.clone(),
      admission_secret,
    );
    tenet
      .initialize(&InitializeRequest { spec_path: None })
      .expect("initialize");
    fs::write(root.join("candidate.txt"), "original candidate").expect("candidate");
    let policy = ProjectConfig {
      version: 1,
      spec_path: "SPEC.md".into(),
      candidate: CandidateCapturePolicy {
        root: ".".into(),
        include: vec!["candidate.txt".into()],
        exclude: vec![],
      },
      verifiers: vec![verifier_spec("V1"), verifier_spec("V2")],
    };
    fs::write(
      root.join(".tenet/tenet.toml"),
      toml::to_string_pretty(&policy).unwrap(),
    )
    .expect("policy");
    Self {
      directory,
      tenet,
      runner,
    }
  }

  fn root(&self) -> &Path {
    self.directory.path()
  }

  fn admit(&self) -> (AdmissionId, AuthorityId) {
    let proposal = self
      .tenet
      .authority_submit(AuthoritySubmitRequest::Proposal {
        contract: contract(),
        issues: vec![],
      })
      .expect("proposal");
    let AuthoritySubmissionResult::Proposal {
      proposal_id,
      authority_id,
      ..
    } = proposal
    else {
      panic!("expected proposal")
    };
    let reconciliation = self
      .tenet
      .authority_submit(AuthoritySubmitRequest::Reconciliation {
        proposal_id: proposal_id.clone(),
        findings: vec![],
      })
      .expect("reconciliation");
    let AuthoritySubmissionResult::Reconciliation {
      reconciliation_id, ..
    } = reconciliation
    else {
      panic!("expected reconciliation")
    };
    let grant = self
      .tenet
      .mint_admission_grant(&proposal_id, &authority_id)
      .expect("grant");
    let admitted = self
      .tenet
      .authority_submit(AuthoritySubmitRequest::Admission {
        proposal_id,
        reconciliation_id,
        authority_id: authority_id.clone(),
        grant,
      })
      .expect("admission");
    let AuthoritySubmissionResult::Admission { admission_id, .. } = admitted else {
      panic!("expected admission")
    };
    (admission_id, authority_id)
  }
}

fn verifier_spec(id: &str) -> VerifierSpec {
  VerifierSpec {
    id: id.into(),
    command: CommandSpec {
      argv: vec![CommandArgument::Literal("true".into())],
      cwd: CommandCwd::Candidate(".".into()),
      env: EnvironmentSpec::default(),
      timeout_ms: 1_000,
      result: Default::default(),
    },
    max_output_bytes: 1_024,
    authority: VerifierAuthority::Project,
    oracle_path: None,
    protection: tenet_domain::policy::VerifierProtection::Local,
  }
}

fn contract() -> CompletionContractV1 {
  CompletionContractV1 {
    schema_version: 1,
    policy: CompletionPolicyId(COMPLETION_POLICY_V1.into()),
    requirements: vec![Requirement {
      id: RequirementId("R1".into()),
      statement: "candidate behavior is complete".into(),
      criteria: vec![criterion("C1", "V1"), criterion("C2", "V2")],
    }],
  }
}

fn criterion(id: &str, verifier: &str) -> Criterion {
  Criterion {
    id: CriterionId(id.into()),
    proposition: format!("criterion {id} holds"),
    verifiers: vec![Verifier {
      id: VerifierId(verifier.into()),
      material: VerifierMaterial::Candidate,
    }],
    evidence: EvidenceRequirementV1 {
      control: EvidenceControlRequirementV1::CandidateControlledPermitted,
      assurance: AssuranceRequirementV1::LocalOrStronger,
    },
  }
}

#[test]
fn context_derives_authority_lifecycle_and_completed_from_refs() {
  let fixture = Fixture::new(None);
  assert_eq!(
    fixture.tenet.context().unwrap().phase,
    WorkflowPhase::AuthorityRequired
  );

  let proposal = fixture
    .tenet
    .authority_submit(AuthoritySubmitRequest::Proposal {
      contract: contract(),
      issues: vec![],
    })
    .unwrap();
  let AuthoritySubmissionResult::Proposal {
    proposal_id,
    authority_id,
    ..
  } = proposal
  else {
    panic!("expected proposal")
  };
  assert_eq!(
    fixture.tenet.context().unwrap().phase,
    WorkflowPhase::AuthorityReconciliation
  );

  let reconciliation = fixture
    .tenet
    .authority_submit(AuthoritySubmitRequest::Reconciliation {
      proposal_id: proposal_id.clone(),
      findings: vec![],
    })
    .unwrap();
  let AuthoritySubmissionResult::Reconciliation {
    reconciliation_id, ..
  } = reconciliation
  else {
    panic!("expected reconciliation")
  };
  assert_eq!(
    fixture.tenet.context().unwrap().phase,
    WorkflowPhase::AuthorityAdmission
  );

  let grant = fixture
    .tenet
    .mint_admission_grant(&proposal_id, &authority_id)
    .unwrap();
  fixture
    .tenet
    .authority_submit(AuthoritySubmitRequest::Admission {
      proposal_id,
      reconciliation_id,
      authority_id,
      grant,
    })
    .unwrap();
  assert_eq!(
    fixture.tenet.context().unwrap().phase,
    WorkflowPhase::Implementation
  );
  assert_eq!(fixture.tenet.verify().unwrap().verdict, Verdict::Done);
  assert_eq!(
    fixture.tenet.context().unwrap().phase,
    WorkflowPhase::Completed
  );
  assert!(!fixture.root().join(".tenet/phase").exists());
}

#[test]
fn requirement_check_is_development_only_and_verify_reruns_every_verifier() {
  let fixture = Fixture::new(None);
  fixture.admit();
  let checked = fixture
    .tenet
    .requirement_check(&RequirementCheckRequest {
      requirement_id: RequirementId("R1".into()),
    })
    .unwrap();
  assert_eq!(checked.result.state, CompletionState::Satisfied);
  assert_eq!(checked.result.verdict, None);
  assert!(!fixture.root().join(".tenet/refs/final").exists());
  assert_eq!(fixture.runner.calls.load(Ordering::SeqCst), 2);
  let context = fixture.tenet.context().unwrap();
  assert_eq!(context.phase, WorkflowPhase::Implementation);
  assert_eq!(context.requirement_checks.len(), 1);
  assert_eq!(
    context.requirement_checks[0].evaluation_id,
    checked.evaluation_id
  );
  assert_eq!(
    context.requirement_checks[0].candidate_id,
    checked.candidate_id
  );

  let verified = fixture.tenet.verify().unwrap();
  assert_eq!(verified.verdict, Verdict::Done);
  assert_eq!(fixture.runner.calls.load(Ordering::SeqCst), 4);
  let observed = fixture.runner.observed.lock().unwrap();
  assert_eq!(observed.as_slice(), ["original candidate"; 4]);

  let final_id = LocalWorkspace
    .read_ref(fixture.root(), "final")
    .unwrap()
    .unwrap();
  let bytes = LocalWorkspace
    .load_object(fixture.root(), &final_id)
    .unwrap();
  let evaluation: Evaluation = serde_json::from_slice(&bytes).unwrap();
  assert_eq!(final_id, verified.evaluation_id.0);
  assert_eq!(evaluation.admission, verified.admission_id);
  assert_eq!(evaluation.authority, verified.authority_id);
  assert_eq!(evaluation.candidate, verified.candidate_id);
  assert!(matches!(evaluation.scope, EvaluationScope::Final));
  assert_eq!(evaluation.runs.len(), 2);
  assert!(evaluation.runs.iter().all(|run| {
    run.authority == verified.authority_id && run.candidate == verified.candidate_id
  }));
}

#[test]
fn mutated_verifier_view_cannot_contribute_to_done() {
  let fixture = Fixture::configured(None, true);
  fixture.admit();
  let result = fixture.tenet.verify().unwrap();
  assert_eq!(result.verdict, Verdict::InfrastructureError);
  assert!(
    result.result.requirements[0].criteria.iter().all(
      |criterion| criterion.state == tenet_domain::algebra::CriterionState::InfrastructureError
    )
  );
}

#[test]
fn final_evaluation_receipt_detects_tampering() {
  let fixture = Fixture::new(None);
  fixture.admit();
  let verified = fixture.tenet.verify().unwrap();
  let receipt = fixture
    .tenet
    .receipt_verify(&verified.evaluation_id)
    .unwrap();
  assert_eq!(receipt.verdict, Verdict::Done);

  let object = fixture
    .root()
    .join(".tenet/objects")
    .join(&verified.evaluation_id.0.0[7..]);
  fs::write(object, b"{}").unwrap();
  assert_eq!(
    fixture
      .tenet
      .receipt_verify(&verified.evaluation_id)
      .unwrap_err()
      .code,
    "content_integrity_failure"
  );
}

#[test]
fn receipt_verification_is_available_to_non_mcp_process_callers() {
  let fixture = Fixture::new(None);
  fixture.admit();
  let verified = fixture.tenet.verify().unwrap();
  let output = ProcessCommand::new(env!("CARGO_BIN_EXE_tenet"))
    .arg("--cwd")
    .arg(fixture.root())
    .env("TENET_ADMISSION_SECRET", hex_secret())
    .arg("doctor")
    .arg("--receipt")
    .arg(&verified.evaluation_id.0.0)
    .arg("--json")
    .output()
    .unwrap();
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(receipt["receiptId"], verified.evaluation_id.0.0);
  assert_eq!(receipt["verdict"], "DONE");
}

#[test]
fn successful_historical_evaluation_does_not_complete_mutated_candidate() {
  let fixture = Fixture::new(Some(1));
  fixture.admit();
  let result = fixture.tenet.verify().unwrap();
  assert_eq!(result.verdict, Verdict::Inconclusive);
  assert_eq!(
    result.reason.as_deref(),
    Some("CANDIDATE_CHANGED_DURING_VERIFICATION")
  );
  assert_ne!(
    result.current_candidate_id.as_ref(),
    Some(&result.candidate_id)
  );
  assert_eq!(result.result.verdict, Some(Verdict::Done));
  assert_eq!(
    fixture.tenet.context().unwrap().phase,
    WorkflowPhase::Implementation
  );
}

#[test]
fn exact_authority_stage_bindings_cannot_transfer() {
  let fixture = Fixture::new(None);
  let first = fixture
    .tenet
    .authority_submit(AuthoritySubmitRequest::Proposal {
      contract: contract(),
      issues: vec![],
    })
    .unwrap();
  let AuthoritySubmissionResult::Proposal {
    proposal_id: p1,
    authority_id: a1,
    ..
  } = first
  else {
    panic!("expected proposal")
  };
  let report = fixture
    .tenet
    .authority_submit(AuthoritySubmitRequest::Reconciliation {
      proposal_id: p1.clone(),
      findings: vec![],
    })
    .unwrap();
  let AuthoritySubmissionResult::Reconciliation {
    reconciliation_id: r1,
    ..
  } = report
  else {
    panic!("expected reconciliation")
  };
  fs::write(fixture.root().join("SPEC.md"), "changed specification").unwrap();
  let second = fixture
    .tenet
    .authority_submit(AuthoritySubmitRequest::Proposal {
      contract: contract(),
      issues: vec![],
    })
    .unwrap();
  let AuthoritySubmissionResult::Proposal {
    proposal_id: p2,
    authority_id: a2,
    ..
  } = second
  else {
    panic!("expected proposal")
  };
  assert_ne!(p1, p2);
  assert_ne!(a1, a2);
  let grant = fixture.tenet.mint_admission_grant(&p2, &a2).unwrap();
  let error = fixture
    .tenet
    .authority_submit(AuthoritySubmitRequest::Admission {
      proposal_id: p2,
      reconciliation_id: r1,
      authority_id: a2,
      grant,
    })
    .unwrap_err();
  assert_eq!(error.code, "admission_invalid");
}

#[test]
fn unknown_completion_and_candidate_semantics_fail_closed() {
  let fixture = Fixture::new(None);
  let mut unknown = contract();
  unknown.policy = CompletionPolicyId("tenet:completion-policy:v999".into());
  let error = fixture
    .tenet
    .authority_submit(AuthoritySubmitRequest::Proposal {
      contract: unknown,
      issues: vec![],
    })
    .unwrap_err();
  assert_eq!(error.code, "semantics_incompatible");

  let manifest = TreeManifest {
    version: 1,
    semantics: SnapshotSemanticsId("tenet:candidate-semantics:v999".into()),
    entries: vec![],
  };
  let id = LocalWorkspace
    .store_object(fixture.root(), &serde_json::to_vec(&manifest).unwrap())
    .unwrap();
  assert!(LocalWorkspace.materialize(fixture.root(), &id).is_err());
}

#[test]
fn authority_submit_rejects_producer_supplied_verdict() {
  let value = serde_json::json!({
    "stage": "PROPOSAL",
    "contract": contract(),
    "issues": [],
    "verdict": "DONE"
  });
  assert!(serde_json::from_value::<AuthoritySubmitRequest>(value).is_err());
}

#[test]
fn doctor_validates_initialized_repository_invariants() {
  let fixture = Fixture::new(None);
  fixture.admit();
  let result = fixture.tenet.doctor().unwrap();
  assert!(result.healthy, "{result:#?}");
  let names = result
    .checks
    .iter()
    .map(|check| check.name.as_str())
    .collect::<Vec<_>>();
  assert_eq!(
    names,
    [
      "repository_root",
      "specification",
      "object_blob_ref_integrity",
      "active_admission_chain",
      "supported_semantic_versions",
      "repository_write_scope",
      "integration_consistency",
    ]
  );
  for path in [
    ".tenet/format",
    ".tenet/.gitignore",
    ".tenet/objects",
    ".tenet/blobs",
    ".tenet/refs",
    ".tenet/refs/proposal",
    ".tenet/refs/reconciliation",
    ".tenet/refs/active-admission",
    ".tenet/refs/requirements",
    ".tenet/tmp",
    ".tenet/lock",
  ] {
    assert!(fixture.root().join(path).exists(), "missing {path}");
  }
}

#[test]
fn blocking_reconciliation_derives_clarification_and_stale_admission_is_detected() {
  let fixture = Fixture::new(None);
  let proposal = fixture
    .tenet
    .authority_submit(AuthoritySubmitRequest::Proposal {
      contract: contract(),
      issues: vec![],
    })
    .unwrap();
  let AuthoritySubmissionResult::Proposal { proposal_id, .. } = proposal else {
    panic!("expected proposal")
  };
  fixture
    .tenet
    .authority_submit(AuthoritySubmitRequest::Reconciliation {
      proposal_id,
      findings: vec![tenet_domain::authority::Finding {
        code: "ambiguity".into(),
        message: "clarification required".into(),
        blocking: true,
      }],
    })
    .unwrap();
  assert_eq!(
    fixture.tenet.context().unwrap().phase,
    WorkflowPhase::AuthorityClarification
  );

  let admitted = Fixture::new(None);
  admitted.admit();
  fs::write(admitted.root().join("SPEC.md"), "stale").unwrap();
  assert_eq!(
    admitted.tenet.context().unwrap().phase,
    WorkflowPhase::AuthorityStale
  );
}

#[test]
fn candidate_controlled_verifier_requires_explicit_contract_policy() {
  let fixture = Fixture::new(None);
  let mut contract = contract();
  contract.requirements[0].criteria[0].verifiers[0].material = VerifierMaterial::AuthorityBundle;
  let error = fixture
    .tenet
    .authority_submit(AuthoritySubmitRequest::Proposal {
      contract,
      issues: vec![],
    })
    .unwrap_err();
  assert_eq!(error.code, "verifier_material_mismatch");
}

#[test]
fn unknown_repository_format_is_not_reinterpreted() {
  let fixture = Fixture::new(None);
  fs::write(fixture.root().join(".tenet/format"), "999\n").unwrap();
  let result = fixture.tenet.doctor().unwrap();
  assert!(!result.healthy);
  assert!(
    result
      .checks
      .iter()
      .any(|check| check.name == "object_blob_ref_integrity" && !check.passed)
  );
}

#[test]
fn admitted_context_ignores_mutable_live_policy_redirection() {
  let fixture = Fixture::new(None);
  let (admission_id, authority_id) = fixture.admit();
  fs::write(
    fixture.root().join(".tenet/tenet.toml"),
    "version = 999\nspec_path = \"missing.md\"\n",
  )
  .unwrap();
  let context = fixture.tenet.context().unwrap();
  assert_eq!(context.phase, WorkflowPhase::Implementation);
  assert_eq!(context.active_admission_id, Some(admission_id));
  assert_eq!(context.authority_id, Some(authority_id));
}

#[test]
fn runner_errors_are_persisted_for_every_required_verifier() {
  let fixture = Fixture::new(None);
  fixture.admit();
  let runner = Arc::new(ErrorRunner::default());
  let tenet = Tenet::new(
    fixture.root().to_path_buf(),
    Arc::new(LocalWorkspace),
    runner.clone(),
    Some(admission_secret()),
  );
  let result = tenet.verify().unwrap();
  assert_eq!(result.verdict, Verdict::InfrastructureError);
  assert_eq!(runner.calls.load(Ordering::SeqCst), 2);
  let bytes = LocalWorkspace
    .load_object(fixture.root(), &result.evaluation_id.0)
    .unwrap();
  let evaluation: Evaluation = serde_json::from_slice(&bytes).unwrap();
  assert_eq!(evaluation.runs.len(), 2);
  assert!(evaluation.runs.iter().all(|run| {
    run.observation.result == EvidenceResult::InfrastructureError
      && run.authority == result.authority_id
      && run.candidate == result.candidate_id
  }));
}

#[test]
fn inconsistent_runner_claim_cannot_produce_done() {
  let fixture = Fixture::new(None);
  fixture.admit();
  let tenet = Tenet::new(
    fixture.root().to_path_buf(),
    Arc::new(LocalWorkspace),
    Arc::new(InconsistentRunner),
    Some(admission_secret()),
  );
  assert_eq!(
    tenet.verify().unwrap().verdict,
    Verdict::InfrastructureError
  );
}

#[test]
fn unknown_draft_lifecycle_version_derives_incompatible() {
  let fixture = Fixture::new(None);
  let root = fixture.root().canonicalize().unwrap();
  let proposal = tenet_domain::authority::AuthorityProposal {
    schema_version: 999,
    authority: AuthorityId(tenet_domain::evidence::ContentObjectId(format!(
      "sha256:{}",
      "a".repeat(64)
    ))),
    issues: vec![],
  };
  let id = LocalWorkspace
    .store_object(&root, &serde_json::to_vec(&proposal).unwrap())
    .unwrap();
  LocalWorkspace.write_ref(&root, "proposal", &id).unwrap();
  assert_eq!(
    fixture.tenet.context().unwrap().phase,
    WorkflowPhase::Incompatible
  );
}

fn staged_pair(fixture: &Fixture) -> (tenet_domain::authority::ProposalId, AuthorityId) {
  let proposal = fixture
    .tenet
    .authority_submit(AuthoritySubmitRequest::Proposal {
      contract: contract(),
      issues: vec![],
    })
    .expect("proposal");
  let AuthoritySubmissionResult::Proposal {
    proposal_id,
    authority_id,
    ..
  } = proposal
  else {
    unreachable!()
  };
  fixture
    .tenet
    .authority_submit(AuthoritySubmitRequest::Reconciliation {
      proposal_id: proposal_id.clone(),
      findings: vec![],
    })
    .expect("reconciliation");
  (proposal_id, authority_id)
}

fn reconciliation_id(fixture: &Fixture) -> tenet_domain::authority::ReconciliationReportId {
  let id = LocalWorkspace
    .read_ref(fixture.root(), "reconciliation")
    .expect("read ref")
    .expect("reconciliation ref");
  tenet_domain::authority::ReconciliationReportId(id)
}

#[test]
fn grant_minting_and_admission_require_the_trusted_secret() {
  let fixture = Fixture::without_secret();
  let (proposal_id, authority_id) = staged_pair(&fixture);
  assert_eq!(
    fixture
      .tenet
      .mint_admission_grant(&proposal_id, &authority_id)
      .unwrap_err()
      .code,
    "admission_secret_unavailable"
  );
  let fabricated = tenet_domain::authority::AdmissionGrant {
    schema_version: 1,
    semantics: tenet_domain::authority::ADMISSION_GRANT_SEMANTICS_V1.into(),
    proposal: proposal_id.clone(),
    authority: authority_id.clone(),
    mac: "0".repeat(64),
  };
  assert_eq!(
    fixture
      .tenet
      .authority_submit(AuthoritySubmitRequest::Admission {
        proposal_id,
        reconciliation_id: reconciliation_id(&fixture),
        authority_id,
        grant: fabricated,
      })
      .unwrap_err()
      .code,
    "admission_secret_unavailable"
  );
  assert_eq!(
    fixture.tenet.context().unwrap().phase,
    WorkflowPhase::AuthorityAdmission
  );
}

#[test]
fn fabricated_grant_mac_cannot_admit() {
  let fixture = Fixture::new(None);
  let (proposal_id, authority_id) = staged_pair(&fixture);
  let forged = tenet_domain::authority::AdmissionGrant {
    schema_version: 1,
    semantics: tenet_domain::authority::ADMISSION_GRANT_SEMANTICS_V1.into(),
    proposal: proposal_id.clone(),
    authority: authority_id.clone(),
    mac: "a".repeat(64),
  };
  assert_eq!(
    fixture
      .tenet
      .authority_submit(AuthoritySubmitRequest::Admission {
        proposal_id,
        reconciliation_id: reconciliation_id(&fixture),
        authority_id,
        grant: forged,
      })
      .unwrap_err()
      .code,
    "admission_grant_invalid"
  );
}

#[test]
fn tampered_grant_mac_cannot_admit() {
  let fixture = Fixture::new(None);
  let (proposal_id, authority_id) = staged_pair(&fixture);
  let mut grant = fixture
    .tenet
    .mint_admission_grant(&proposal_id, &authority_id)
    .expect("grant");
  let mut mac = grant.mac.clone();
  mac.replace_range(63..64, if &mac[63..64] == "0" { "1" } else { "0" });
  grant.mac = mac;
  assert_eq!(
    fixture
      .tenet
      .authority_submit(AuthoritySubmitRequest::Admission {
        proposal_id,
        reconciliation_id: reconciliation_id(&fixture),
        authority_id,
        grant,
      })
      .unwrap_err()
      .code,
    "admission_grant_invalid"
  );
}

#[test]
fn grant_cannot_transfer_across_authority_identities() {
  let fixture = Fixture::new(None);
  let (first_proposal, first_authority) = staged_pair(&fixture);
  let grant = fixture
    .tenet
    .mint_admission_grant(&first_proposal, &first_authority)
    .expect("grant");
  let admitted = fixture
    .tenet
    .authority_submit(AuthoritySubmitRequest::Admission {
      proposal_id: first_proposal.clone(),
      reconciliation_id: reconciliation_id(&fixture),
      authority_id: first_authority.clone(),
      grant,
    })
    .expect("admission");
  let AuthoritySubmissionResult::Admission {
    admission_id: first_admission,
    ..
  } = admitted
  else {
    unreachable!()
  };

  // The producer proposes a replacement authority. Its reconciliation is valid,
  // but the only grant it can present is bound to the first identities.
  fs::write(
    fixture.root().join("SPEC.md"),
    "# Replacement specification\n",
  )
  .expect("spec");
  let (second_proposal, second_authority) = staged_pair(&fixture);
  let replayed = fixture
    .tenet
    .mint_admission_grant(&first_proposal, &first_authority)
    .expect("grant");
  assert_eq!(
    fixture
      .tenet
      .authority_submit(AuthoritySubmitRequest::Admission {
        proposal_id: second_proposal,
        reconciliation_id: reconciliation_id(&fixture),
        authority_id: second_authority,
        grant: replayed,
      })
      .unwrap_err()
      .code,
    "admission_grant_invalid"
  );
  let context = fixture.tenet.context().unwrap();
  assert_eq!(context.phase, WorkflowPhase::AuthorityStale);
  assert_eq!(context.active_admission_id, Some(first_admission));
}

#[test]
fn admission_submission_requires_a_grant_field() {
  let value = serde_json::json!({
    "stage": "ADMISSION",
    "proposalId": format!("sha256:{}", "a".repeat(64)),
    "reconciliationId": format!("sha256:{}", "b".repeat(64)),
    "authorityId": format!("sha256:{}", "c".repeat(64)),
  });
  assert!(serde_json::from_value::<AuthoritySubmitRequest>(value).is_err());
}

#[test]
fn unknown_grant_semantics_fail_closed_on_load() {
  let fixture = Fixture::new(None);
  fixture.admit();
  assert_eq!(
    fixture.tenet.context().unwrap().phase,
    WorkflowPhase::Implementation
  );
  let root = fixture.root().canonicalize().unwrap();
  let active_id = LocalWorkspace
    .read_ref(&root, "active-admission")
    .expect("read ref")
    .expect("active admission");
  let bytes = LocalWorkspace
    .load_object(&root, &active_id)
    .expect("admission");
  let mut admission: tenet_domain::authority::Admission = serde_json::from_slice(&bytes).unwrap();
  admission.grant.semantics = "tenet:admission-grant:v999".into();
  let forged = LocalWorkspace
    .store_object(&root, &serde_json::to_vec(&admission).unwrap())
    .expect("store");
  LocalWorkspace
    .write_ref(&root, "active-admission", &forged)
    .expect("write ref");
  assert_eq!(
    fixture.tenet.context().unwrap().phase,
    WorkflowPhase::Incompatible
  );
  assert_eq!(
    fixture.tenet.verify().unwrap_err().code,
    "admission_invalid"
  );
}

#[test]
fn grant_bound_to_foreign_ids_fails_structural_binding_on_load() {
  let fixture = Fixture::new(None);
  fixture.admit();
  let root = fixture.root().canonicalize().unwrap();
  let active_id = LocalWorkspace
    .read_ref(&root, "active-admission")
    .expect("read ref")
    .expect("active admission");
  let bytes = LocalWorkspace
    .load_object(&root, &active_id)
    .expect("admission");
  let mut admission: tenet_domain::authority::Admission = serde_json::from_slice(&bytes).unwrap();
  admission.grant.authority = AuthorityId(tenet_domain::evidence::ContentObjectId(format!(
    "sha256:{}",
    "f".repeat(64)
  )));
  let forged = LocalWorkspace
    .store_object(&root, &serde_json::to_vec(&admission).unwrap())
    .expect("store");
  LocalWorkspace
    .write_ref(&root, "active-admission", &forged)
    .expect("write ref");
  assert_eq!(
    fixture.tenet.context().unwrap().phase,
    WorkflowPhase::Incompatible
  );
}

#[test]
fn runner_cannot_overclaim_protected_v1_for_local_spec() {
  let fixture = Fixture::new(None);
  fixture.admit();
  let tenet = Tenet::new(
    fixture.root().to_path_buf(),
    Arc::new(LocalWorkspace),
    Arc::new(OverclaimingRunner),
    Some(admission_secret()),
  );
  assert_eq!(
    tenet.verify().unwrap().verdict,
    Verdict::InfrastructureError
  );
}

/// A process holding the trusted secret re-verifies the persisted grant mac
/// on every load that can influence verification or `DONE`.
fn trusted_reader(fixture: &Fixture, secret: Option<Vec<u8>>) -> Tenet {
  Tenet::new(
    fixture.root().to_path_buf(),
    Arc::new(LocalWorkspace),
    Arc::new(RecordingRunner::default()),
    secret,
  )
}

fn active_admission(fixture: &Fixture) -> tenet_domain::authority::Admission {
  let root = fixture.root().canonicalize().unwrap();
  let active_id = LocalWorkspace
    .read_ref(&root, "active-admission")
    .expect("read ref")
    .expect("active admission");
  let bytes = LocalWorkspace
    .load_object(&root, &active_id)
    .expect("admission object");
  serde_json::from_slice(&bytes).expect("admission")
}

fn replace_active_admission(fixture: &Fixture, admission: &tenet_domain::authority::Admission) {
  let root = fixture.root().canonicalize().unwrap();
  let forged = LocalWorkspace
    .store_object(&root, &serde_json::to_vec(admission).unwrap())
    .expect("store forged admission");
  LocalWorkspace
    .write_ref(&root, "active-admission", &forged)
    .expect("write forged ref");
}

#[test]
fn tampered_grant_mac_on_persisted_admission_cannot_verify() {
  let fixture = Fixture::new(None);
  fixture.admit();
  assert_eq!(
    fixture.tenet.verify().expect("verify").verdict,
    Verdict::Done,
    "the valid persisted Admission must be accepted on reload"
  );
  let mut admission = active_admission(&fixture);
  let mut mac = admission.grant.mac.clone();
  mac.replace_range(63..64, if &mac[63..64] == "0" { "1" } else { "0" });
  admission.grant.mac = mac;
  replace_active_admission(&fixture, &admission);
  // Structurally well-formed and internally canonical: only the mac betrays it.
  assert_eq!(
    fixture.tenet.verify().unwrap_err().code,
    "admission_grant_invalid"
  );
  assert_eq!(
    fixture
      .tenet
      .requirement_check(&RequirementCheckRequest {
        requirement_id: RequirementId("R1".into()),
      })
      .unwrap_err()
      .code,
    "admission_grant_invalid"
  );
  assert_eq!(
    fixture.tenet.context().unwrap().phase,
    WorkflowPhase::Incompatible
  );
}

#[test]
fn manually_forged_admission_in_repository_state_is_rejected() {
  let fixture = Fixture::new(None);
  let (proposal_id, authority_id) = staged_pair(&fixture);
  let reconciliation_id = reconciliation_id(&fixture);
  // A producer with repository write access hand-constructs a complete
  // content-addressed Admission whose chain and grant binding are exact; only
  // the mac was never issued under the trusted secret.
  let forged = tenet_domain::authority::Admission {
    schema_version: 1,
    proposal: proposal_id.clone(),
    reconciliation: reconciliation_id,
    authority: authority_id.clone(),
    grant: tenet_domain::authority::AdmissionGrant {
      schema_version: 1,
      semantics: tenet_domain::authority::ADMISSION_GRANT_SEMANTICS_V1.into(),
      proposal: proposal_id,
      authority: authority_id,
      mac: "e".repeat(64),
    },
  };
  replace_active_admission(&fixture, &forged);
  assert_eq!(
    fixture.tenet.verify().unwrap_err().code,
    "admission_grant_invalid"
  );
  assert_eq!(
    fixture
      .tenet
      .requirement_check(&RequirementCheckRequest {
        requirement_id: RequirementId("R1".into()),
      })
      .unwrap_err()
      .code,
    "admission_grant_invalid"
  );
}

#[test]
fn wrong_or_missing_secret_cannot_derive_completion_from_persisted_state() {
  let fixture = Fixture::new(None);
  fixture.admit();
  // A different trusted secret cannot authenticate the stored grant.
  let wrong = trusted_reader(
    &fixture,
    Some(b"other-trusted-secret-0123456789abcdef".to_vec()),
  );
  assert_eq!(wrong.verify().unwrap_err().code, "admission_grant_invalid");
  // No secret at all: the mac cannot be re-verified, so verification fails
  // closed instead of trusting the persisted chain.
  let none = trusted_reader(&fixture, None);
  assert_eq!(
    none.verify().unwrap_err().code,
    "admission_secret_unavailable"
  );
  assert_eq!(
    none
      .requirement_check(&RequirementCheckRequest {
        requirement_id: RequirementId("R1".into()),
      })
      .unwrap_err()
      .code,
    "admission_secret_unavailable"
  );
  // The valid chain still completes for the holder of the real secret.
  assert_eq!(
    trusted_reader(&fixture, Some(admission_secret()))
      .verify()
      .expect("trusted verify")
      .verdict,
    Verdict::Done
  );
}
