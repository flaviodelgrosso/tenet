use std::collections::BTreeMap;

use chrono::{TimeZone, Utc};
use ed25519_dalek::SigningKey;
use serde_json::json;
use tenet_domain::{
  evidence::{ObligationAssessmentResult, SemanticAssessmentReport},
  falsifier::{
    FalsificationExecutionRecord, FalsifierProtocol, FalsifierSpec, StructuredFalsifierInputSpec,
  },
  human_attestation::{HumanAttestationBinding, HumanAttestationRecord, HumanAttestorSpec},
  ids::{ArtifactId, ObligationId, VerificationRunId},
  proof::{
    statement_hash, ArtifactAuthority, AssessmentJudgment, DependencyPolicy, DependencySurface,
    EvidenceContract, EvidencePredicate, ExecutionObservation, ProofState,
  },
  trusted_verifier::{
    CandidateFilesystemPolicy, ControlChannel, EnvironmentPolicy, GuestSecurityProfile,
    HostRepositoryMountPolicy, IsolationBoundary, IsolationCapabilityReport, NetworkPolicy,
    TrustedExecutionBackend, TrustedExecutionRecord, TrustedExecutionResult,
    TrustedIsolationPolicy, TrustedResourcePolicy, TrustedVerificationSpec,
    TrustedVerifierProtocol, WritableStoragePolicy,
  },
  verification::{CommandResult, ProjectCheckResult, ProjectVerificationRun, VerificationSpec},
};
use tenet_storage::{install_controller_authority_key, Storage};

mod support;

fn project_run(revision: &str) -> ProjectVerificationRun {
  let now = Utc.with_ymd_and_hms(2026, 8, 26, 10, 0, 0).unwrap();
  ProjectVerificationRun {
    run_id: VerificationRunId::new(),
    revision: revision.into(),
    suite_hash: "suite".into(),
    checks: vec![ProjectCheckResult {
      name: "quality".into(),
      spec: VerificationSpec {
        program: "true".into(),
        args: Vec::new(),
        working_directory: ".".into(),
        environment: BTreeMap::new(),
      },
      timeout_secs: 10,
      result: CommandResult {
        command: "true".into(),
        exit_code: Some(0),
        timed_out: false,
        duration_ms: 1,
        stdout: String::new(),
        stderr: String::new(),
      },
    }],
    passed: true,
    started_at: now,
    finished_at: now,
  }
}

async fn prepared_storage() -> (
  tempfile::TempDir,
  Storage,
  tenet_domain::model::RequirementCatalog,
) {
  install_controller_authority_key("tenet-storage-tests", b"tenet-storage-test-authority")
    .expect("install project authority key");
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let mut catalog = support::catalog();
  catalog.verification_obligations[0].evidence_contract = EvidenceContract::Any {
    requirements: vec![
      EvidenceContract::Artifact {
        predicate: EvidencePredicate::NamedProjectCheck {
          name: "quality".into(),
        },
      },
      EvidenceContract::Artifact {
        predicate: EvidencePredicate::SourceInspection,
      },
    ],
  };
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("catalog");
  storage.create_run("run-1").await.expect("run");
  (project, storage, catalog)
}

#[tokio::test]
async fn artifact_and_derivation_survive_restart_with_provenance() {
  let (project_dir, storage, catalog) = prepared_storage().await;
  let project = project_run("revision-1");
  storage
    .record_project_verification("run-1", &project)
    .await
    .expect("project verification");
  let mut graph = support::empty_graph(&catalog);
  graph.record_project_verification(&project);
  let ids = graph.record_project_artifacts(&project).expect("artifacts");
  graph.derive_proofs("revision-1");
  storage
    .persist_evidence_graph("run-1", &graph)
    .await
    .expect("persist graph");

  let reopened = Storage::open(project_dir.path())
    .await
    .expect("reopen storage");
  let loaded = reopened
    .load_evidence_graph(&catalog, &[], &[], &[])
    .await
    .expect("load graph");
  assert_eq!(loaded.artifacts.get(&ids[0]), graph.artifacts.get(&ids[0]));
  assert_eq!(
    loaded.proof_derivations[&ObligationId::from("REQ-001/AC-01/VO-01")].state,
    ProofState::Proven
  );
  assert_eq!(loaded.proof_derivations, graph.proof_derivations);
}

#[tokio::test]
async fn tampered_project_verification_record_cannot_mint_configured_check_authority() {
  let (_project_dir, storage, catalog) = prepared_storage().await;
  let project = project_run("revision-1");
  storage
    .record_project_verification("run-1", &project)
    .await
    .expect("project verification");
  let mut graph = support::empty_graph(&catalog);
  graph.record_project_verification(&project);
  graph.record_project_artifacts(&project).expect("artifacts");
  graph.derive_proofs("revision-1");
  storage
    .persist_evidence_graph("run-1", &graph)
    .await
    .expect("persist graph");

  let mut forged = project;
  forged.checks[0].result.exit_code = Some(99);
  sqlx::query(
    "UPDATE project_verification_runs SET verification_json = ?, passed = 1 WHERE id = ?",
  )
  .bind(serde_json::to_string(&forged).expect("forged project JSON"))
  .bind(forged.run_id.to_string())
  .execute(storage.pool())
  .await
  .expect("tamper project verification issuer record");
  sqlx::query(
    "UPDATE project_verification_checks SET exit_code = 99 WHERE verification_run_id = ?",
  )
  .bind(forged.run_id.to_string())
  .execute(storage.pool())
  .await
  .expect("tamper project verification check");

  let error = storage
    .load_evidence_graph(&catalog, &[], &[], &[])
    .await
    .expect_err("forged project verification authority must fail authentication");
  assert!(error
    .to_string()
    .contains("controller authority authentication tag is invalid"));
}

#[tokio::test]
async fn stale_artifact_and_blocking_proof_survive_restart() {
  let (_project, storage, catalog) = prepared_storage().await;
  let project = project_run("revision-1");
  let mut graph = support::empty_graph(&catalog);
  graph.record_project_verification(&project);
  graph.record_project_artifacts(&project).expect("artifacts");
  graph.derive_proofs("revision-1");
  graph.transition_artifacts("revision-2", None);
  storage
    .persist_evidence_graph("run-1", &graph)
    .await
    .expect("persist stale graph");

  let loaded = storage
    .load_evidence_graph(&catalog, &[], &[], &[])
    .await
    .expect("load graph");
  assert_eq!(
    loaded.proof_derivations[&ObligationId::from("REQ-001/AC-01/VO-01")].state,
    ProofState::Stale
  );
}

#[tokio::test]
async fn fabricated_artifact_reference_is_rejected() {
  let (_project, _storage, catalog) = prepared_storage().await;
  let mut graph = support::empty_graph(&catalog);
  let report = SemanticAssessmentReport {
    summary: "fabricated".into(),
    assessments: vec![ObligationAssessmentResult {
      obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
      assessment: AssessmentJudgment::Supported {
        artifact_ids: vec![ArtifactId::new()],
        rationale: "invented".into(),
      },
    }],
  };
  let error = graph
    .record_semantic_assessment("revision-1", Utc::now(), "assessor", &report)
    .expect_err("fabricated artifact rejected");
  assert!(error.to_string().contains("unknown evidence artifact"));
}
#[tokio::test]
async fn forged_controller_execution_cannot_be_persisted() {
  let (_project, storage, catalog) = prepared_storage().await;
  let project = project_run("revision-1");
  let mut graph = support::empty_graph(&catalog);
  graph.record_project_verification(&project);
  graph.record_project_artifacts(&project).expect("artifacts");
  let artifact = graph.artifacts.values_mut().next().expect("artifact");
  if let tenet_domain::proof::EvidenceArtifactKind::CommandExecution { result, .. } =
    &mut artifact.kind
  {
    result.exit_code = Some(99);
  } else {
    panic!("expected command artifact");
  }

  let error = storage
    .persist_evidence_graph("run-1", &graph)
    .await
    .expect_err("forged execution rejected");
  assert!(error.to_string().contains("unauthorized combination"));
}

fn trusted_spec() -> TrustedVerificationSpec {
  TrustedVerificationSpec {
    name: "expiry-boundary".into(),
    backend: TrustedExecutionBackend::Microsandbox,
    image: format!("example/verifier@sha256:{}", "a".repeat(64)),
    program: "verify".into(),
    args: Vec::new(),
    working_directory: ".".into(),
    environment: BTreeMap::new(),
    timeout_secs: 30,
    isolation: TrustedIsolationPolicy::default(),
    resources: TrustedResourcePolicy::default(),
    protocol: TrustedVerifierProtocol::ExitCode,
    dependencies: Default::default(),
  }
}

fn trusted_record(
  spec: &TrustedVerificationSpec,
  result: TrustedExecutionResult,
) -> TrustedExecutionRecord {
  let exit_code = match result {
    TrustedExecutionResult::Supports => 0,
    TrustedExecutionResult::Contradicts { exit_code } => exit_code,
    _ => panic!("fixture requires a semantic result"),
  };
  let now = Utc.with_ymd_and_hms(2026, 8, 26, 11, 0, 0).unwrap();
  TrustedExecutionRecord {
    id: VerificationRunId::new(),
    revision: "revision-1".into(),
    input_materialization_hash: "archive-hash".into(),
    verifier_name: spec.name.clone(),
    spec_hash: spec.fingerprint().expect("spec hash"),
    isolation_policy_hash: spec.isolation_policy_hash().expect("policy hash"),
    isolation_report: Some(IsolationCapabilityReport {
      backend: TrustedExecutionBackend::Microsandbox,
      backend_version: "microsandbox-rust-sdk/0.6.15".into(),
      runtime_identity: "local-msb/sdk-protocol-compatible".into(),
      boundary: IsolationBoundary::HardwareVirtualizedMicroVm,
      image: spec.image.clone(),
      resolved_image_digest: format!("sha256:{}", "a".repeat(64)),
      input_revision: "revision-1".into(),
      input_materialization_hash: "archive-hash".into(),
      input_archive_bytes: 1024,
      input_tree_bytes: 512,
      input_entries: 2,
      candidate_filesystem: CandidateFilesystemPolicy::PrivateWritable,
      host_repository_mounts: HostRepositoryMountPolicy::None,
      writable_storage: WritableStoragePolicy::DisposableSandboxPrivate,
      network: NetworkPolicy::Disabled,
      environment: EnvironmentPolicy::ExplicitOnly,
      guest_security_profile: GuestSecurityProfile::Restricted,
      guest_user: "65532".into(),
      unprivileged_user: true,
      control_channel: ControlChannel::LocalHostDriven,
      memory_mib: spec.resources.memory_mib,
      vcpus: spec.resources.vcpus,
      process_limit: spec.resources.process_limit,
      writable_root_mib: spec.resources.writable_root_mib,
      max_input_archive_bytes: spec.resources.max_input_archive_bytes,
      max_input_tree_bytes: spec.resources.max_input_tree_bytes,
      max_input_entries: spec.resources.max_input_entries,
      execution_timeout_secs: spec.timeout_secs,
      sandbox_lifetime_secs: spec.timeout_secs + 30,
    }),
    started_at: now,
    finished_at: now,
    result,
    observation: ExecutionObservation {
      command: spec.fingerprint().expect("command identity"),
      exit_code: Some(exit_code),
      timed_out: false,
      duration_ms: 1,
      stdout: String::new(),
      stderr: String::new(),
    },
    obligation_ids: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
  }
}

async fn persisted_falsifier_states(
  result: TrustedExecutionResult,
  input: Option<serde_json::Value>,
) -> (ProofState, ProofState) {
  install_controller_authority_key("tenet-storage-tests", b"tenet-storage-test-authority")
    .expect("install authority key");
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let mut catalog = support::catalog();
  catalog.verification_obligations[0].evidence_contract = EvidenceContract::Artifact {
    predicate: EvidencePredicate::FalsifierCheck {
      name: "boundary-search".into(),
    },
  };
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("catalog");
  storage.create_run("run-falsifier").await.expect("run");
  let spec = FalsifierSpec {
    execution: TrustedVerificationSpec {
      name: "boundary-search".into(),
      ..trusted_spec()
    },
    protocol: FalsifierProtocol::ExitCode,
    input: input.as_ref().map(|_| StructuredFalsifierInputSpec {
      schema: json!({"type": "object", "required": ["seed"]}),
      argument: "--input-json".into(),
      max_bytes: 128,
    }),
  };
  let execution_spec = spec.execution_spec(input.as_ref()).expect("execution spec");
  let record = FalsificationExecutionRecord::from_trusted_execution(
    trusted_record(&execution_spec, result),
    &spec,
    input,
  )
  .expect("falsification record");
  storage
    .record_falsification("run-falsifier", &record, &spec)
    .await
    .expect("persist falsification");
  let mut graph = support::empty_graph(&catalog);
  graph
    .record_falsification(&record, &spec, DependencySurface::RepositoryWide)
    .expect("issue artifact");
  graph.derive_proofs("revision-1");
  let before_restart = graph.proof_derivations[&ObligationId::from("REQ-001/AC-01/VO-01")].state;
  storage
    .persist_evidence_graph("run-falsifier", &graph)
    .await
    .expect("persist graph");

  let reopened = Storage::open(project.path()).await.expect("reopen");
  let loaded = reopened
    .load_evidence_graph(&catalog, &[], std::slice::from_ref(&spec), &[])
    .await
    .expect("reload falsifier observation");
  let after_restart = loaded.proof_derivations[&ObligationId::from("REQ-001/AC-01/VO-01")].state;
  (before_restart, after_restart)
}

#[tokio::test]
async fn fixed_no_counterexample_is_proven_before_and_after_restart() {
  let states = persisted_falsifier_states(TrustedExecutionResult::Supports, None).await;

  assert_eq!(states, (ProofState::Proven, ProofState::Proven));
}

#[tokio::test]
async fn dynamic_no_counterexample_is_non_proving_before_and_after_restart() {
  let states =
    persisted_falsifier_states(TrustedExecutionResult::Supports, Some(json!({"seed": 7}))).await;

  assert_eq!(states, (ProofState::Insufficient, ProofState::Insufficient));
}

#[tokio::test]
async fn dynamic_counterexample_is_contradicted_before_and_after_restart() {
  let states = persisted_falsifier_states(
    TrustedExecutionResult::Contradicts { exit_code: 1 },
    Some(json!({"seed": 7})),
  )
  .await;

  assert_eq!(states, (ProofState::Contradicted, ProofState::Contradicted));
}

async fn prepared_trusted_storage() -> (
  tempfile::TempDir,
  Storage,
  tenet_domain::model::RequirementCatalog,
  TrustedVerificationSpec,
) {
  install_controller_authority_key("tenet-storage-tests", b"tenet-storage-test-authority")
    .expect("install test authority identity");
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let mut catalog = support::catalog();
  catalog.verification_obligations[0].evidence_contract = EvidenceContract::Artifact {
    predicate: EvidencePredicate::TrustedVerifierCheck {
      name: "expiry-boundary".into(),
    },
  };
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("catalog");
  storage.create_run("run-1").await.expect("run");
  (project, storage, catalog, trusted_spec())
}

#[tokio::test]
async fn trusted_execution_authority_survives_restart_and_revalidation() {
  let (project, storage, catalog, spec) = prepared_trusted_storage().await;
  let record = trusted_record(&spec, TrustedExecutionResult::Supports);
  storage
    .record_trusted_execution("run-1", &record, &spec)
    .await
    .expect("trusted record");
  let mut graph = support::empty_graph(&catalog);
  let artifact_id = graph
    .record_trusted_execution(&record, &spec, DependencySurface::RepositoryWide)
    .expect("trusted artifact")
    .expect("authoritative artifact");
  graph.derive_proofs("revision-1");
  storage
    .persist_evidence_graph("run-1", &graph)
    .await
    .expect("persist graph");

  let reopened = Storage::open(project.path()).await.expect("reopen");
  let loaded = reopened
    .load_evidence_graph(&catalog, std::slice::from_ref(&spec), &[], &[])
    .await
    .expect("revalidated graph");

  assert_eq!(
    loaded.artifacts[&artifact_id].authority,
    ArtifactAuthority::Authoritative
  );
  assert_eq!(
    loaded.proof_derivations[&ObligationId::from("REQ-001/AC-01/VO-01")].state,
    ProofState::Proven
  );
}

#[tokio::test]
async fn mismatched_resolved_image_digest_cannot_enter_persistence() {
  let (_project, storage, _catalog, spec) = prepared_trusted_storage().await;
  let mut record = trusted_record(&spec, TrustedExecutionResult::Supports);
  record
    .isolation_report
    .as_mut()
    .expect("capability report")
    .resolved_image_digest = format!("sha256:{}", "b".repeat(64));

  let error = storage
    .record_trusted_execution("run-1", &record, &spec)
    .await
    .expect_err("mismatched report must not persist");

  assert!(error
    .to_string()
    .contains("trusted execution record failed authority admission"));
}

#[tokio::test]
async fn changed_controller_verifier_spec_rejects_persisted_authority() {
  let (_project, storage, catalog, spec) = prepared_trusted_storage().await;
  let record = trusted_record(&spec, TrustedExecutionResult::Supports);
  storage
    .record_trusted_execution("run-1", &record, &spec)
    .await
    .expect("trusted record");
  let mut graph = support::empty_graph(&catalog);
  graph
    .record_trusted_execution(&record, &spec, DependencySurface::RepositoryWide)
    .expect("trusted artifact");
  graph.derive_proofs("revision-1");
  storage
    .persist_evidence_graph("run-1", &graph)
    .await
    .expect("persist graph");
  let mut changed_args = spec.clone();
  changed_args.args.push("--changed".into());
  let mut changed_image = spec;
  changed_image.image = format!("example/verifier@sha256:{}", "b".repeat(64));

  for changed in [changed_args, changed_image] {
    let loaded = storage
      .load_evidence_graph(&catalog, &[changed], &[], &[])
      .await
      .expect("changed verifier authority is rejected without blocking reload");
    assert!(loaded.artifacts.is_empty());
    assert_eq!(
      loaded.proof_derivations[&ObligationId::from("REQ-001/AC-01/VO-01")].state,
      ProofState::Insufficient
    );
  }
}

#[tokio::test]
async fn prior_catalog_authority_cannot_replay_for_a_reused_obligation_id() {
  let (project, storage, catalog, spec) = prepared_trusted_storage().await;
  let record = trusted_record(&spec, TrustedExecutionResult::Supports);
  storage
    .record_trusted_execution("run-1", &record, &spec)
    .await
    .expect("trusted record");
  let mut graph = support::empty_graph(&catalog);
  graph
    .record_trusted_execution(&record, &spec, DependencySurface::RepositoryWide)
    .expect("trusted artifact");
  graph.derive_proofs("revision-1");
  storage
    .persist_evidence_graph("run-1", &graph)
    .await
    .expect("persist graph");

  let changed_description = "A different claim that deliberately reuses the old obligation ID";
  let mut changed_catalog = catalog;
  changed_catalog.verification_obligations[0].description = changed_description.into();
  let database = project.path().join(".tenet/tenet.db");
  let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", database.display()))
    .await
    .expect("open database for replay mutation");
  sqlx::query("UPDATE verification_obligations SET description = ? WHERE id = ?")
    .bind(changed_description)
    .bind("REQ-001/AC-01/VO-01")
    .execute(&pool)
    .await
    .expect("reuse obligation ID for a changed claim");
  pool.close().await;

  let loaded = storage
    .load_evidence_graph(&changed_catalog, &[spec], &[], &[])
    .await
    .expect("stale catalog authority is rejected without blocking reload");

  assert!(loaded.artifacts.is_empty());
}

#[tokio::test]
async fn forged_trusted_artifact_without_execution_record_is_rejected() {
  let (_project, storage, catalog, spec) = prepared_trusted_storage().await;
  let record = trusted_record(&spec, TrustedExecutionResult::Supports);
  let mut graph = support::empty_graph(&catalog);
  graph
    .record_trusted_execution(&record, &spec, DependencySurface::RepositoryWide)
    .expect("domain artifact");

  let error = storage
    .persist_evidence_graph("run-1", &graph)
    .await
    .expect_err("unbacked trusted artifact rejected");

  assert!(error.to_string().contains("unknown controller execution"));
}

#[tokio::test]
async fn tampered_trusted_execution_record_fails_authentication() {
  let (project, storage, catalog, spec) = prepared_trusted_storage().await;
  let record = trusted_record(&spec, TrustedExecutionResult::Supports);
  storage
    .record_trusted_execution("run-1", &record, &spec)
    .await
    .expect("trusted record");
  let mut graph = support::empty_graph(&catalog);
  graph
    .record_trusted_execution(&record, &spec, DependencySurface::RepositoryWide)
    .expect("trusted artifact");
  graph.derive_proofs("revision-1");
  storage
    .persist_evidence_graph("run-1", &graph)
    .await
    .expect("persist graph");

  let database = project.path().join(".tenet/tenet.db");
  let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", database.display()))
    .await
    .expect("open database for adversarial mutation");
  sqlx::query("UPDATE trusted_verifier_executions SET verifier_name = 'forged', record_json = json_set(record_json, '$.verifier_name', 'forged')")
    .execute(&pool)
    .await
    .expect("tamper execution record");
  pool.close().await;
  let loaded = storage
    .load_evidence_graph(&catalog, &[spec], &[], &[])
    .await
    .expect("tampered authority is rejected without blocking reload");

  assert!(loaded.artifacts.is_empty());
  assert_eq!(
    loaded.proof_derivations[&ObligationId::from("REQ-001/AC-01/VO-01")].state,
    ProofState::Insufficient
  );
}

#[test]
fn changed_controller_authority_identity_is_rejected() {
  install_controller_authority_key("tenet-storage-tests", b"tenet-storage-test-authority")
    .expect("install stable test authority identity");

  let error = install_controller_authority_key(
    "different-repository-authority",
    b"different-controller-authority-key",
  )
  .expect_err("changed authority identity must fail closed");

  assert!(error
    .to_string()
    .contains("controller authority identity changed"));
}

#[tokio::test]
async fn authenticated_human_attestation_survives_restart_and_proves_exact_contract() {
  install_controller_authority_key("tenet-storage-tests", b"tenet-storage-test-authority")
    .expect("install authority key");
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let mut catalog = support::catalog();
  let statement = "Manual visual review confirms the exact interaction";
  catalog.verification_obligations[0].evidence_contract = EvidenceContract::HumanAttestation {
    statement: statement.into(),
  };
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("persist catalog");
  storage.create_run("run-human").await.expect("create run");
  let secret = [7_u8; 32];
  let verifying_key = SigningKey::from_bytes(&secret).verifying_key();
  let attestor = HumanAttestorSpec {
    id: "alice".into(),
    public_key: verifying_key
      .to_bytes()
      .iter()
      .map(|byte| format!("{byte:02x}"))
      .collect(),
    dependencies: DependencyPolicy::Paths {
      patterns: vec!["src/**".into()],
    },
  };
  let obligation_id = catalog.verification_obligations[0].id.clone();
  let record = HumanAttestationRecord::sign(
    &attestor,
    &secret,
    HumanAttestationBinding {
      statement_hash: statement_hash(statement),
      obligation_id: obligation_id.clone(),
      catalog_hash: catalog.catalog_hash().expect("catalog hash"),
      revision: "revision-human".into(),
      issued_at: Utc::now(),
      dependencies: DependencySurface::Paths {
        patterns: vec!["src/**".into()],
        blob_hashes: BTreeMap::from([("src/lib.rs".into(), "blob-1".into())]),
      },
    },
  )
  .expect("sign attestation");
  storage
    .record_human_attestation("run-human", &record, &attestor)
    .await
    .expect("persist attestation issuer record");
  let mut graph = support::empty_graph(&catalog);
  graph
    .record_human_attestation(
      &record,
      &attestor,
      &catalog.catalog_hash().expect("catalog hash"),
    )
    .expect("issue human artifact");
  graph.derive_proofs("revision-human");
  storage
    .persist_evidence_graph("run-human", &graph)
    .await
    .expect("persist human artifact");
  drop(storage);

  let reopened = Storage::open(project.path()).await.expect("reopen storage");
  let loaded = reopened
    .load_evidence_graph(&catalog, &[], &[], std::slice::from_ref(&attestor))
    .await
    .expect("reload authenticated human authority");
  assert_eq!(
    loaded.proof_derivations[&obligation_id].state,
    ProofState::Proven
  );
  let unknown_attestor = reopened
    .load_evidence_graph(&catalog, &[], &[], &[])
    .await
    .expect("reject unknown attestor without blocking reload");
  assert!(unknown_attestor.artifacts.is_empty());
  assert_eq!(
    unknown_attestor.proof_derivations[&obligation_id].state,
    ProofState::Insufficient
  );
  let database = project.path().join(".tenet/tenet.db");
  let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", database.display()))
    .await
    .expect("open database for human reuse mutation");
  sqlx::query("UPDATE evidence_artifacts SET artifact_json = json_set(artifact_json, '$.compatibleRevisions', json_array('forged-revision'))")
    .execute(&pool)
    .await
    .expect("forge human compatibility revision");
  pool.close().await;
  let forged = reopened
    .load_evidence_graph(&catalog, &[], &[], std::slice::from_ref(&attestor))
    .await
    .expect("reject forged human reuse without blocking reload");
  assert!(forged.artifacts.is_empty());
  assert_eq!(
    forged.proof_derivations[&obligation_id].state,
    ProofState::Insufficient
  );
}
