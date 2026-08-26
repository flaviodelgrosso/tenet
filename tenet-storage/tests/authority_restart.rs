use std::{collections::BTreeMap, path::Path, process::Command};

use chrono::Utc;
use tenet_domain::{
  ids::{ObligationId, VerificationRunId},
  proof::{EvidenceContract, EvidencePredicate, ExecutionObservation, ProofState},
  trusted_verifier::{
    CandidateFilesystemPolicy, ControlChannel, EnvironmentPolicy, GuestSecurityProfile,
    HostRepositoryMountPolicy, IsolationBoundary, IsolationCapabilityReport, NetworkPolicy,
    TrustedExecutionBackend, TrustedExecutionRecord, TrustedExecutionResult,
    TrustedIsolationPolicy, TrustedResourcePolicy, TrustedVerificationSpec,
    TrustedVerifierProtocol, WritableStoragePolicy,
  },
};
use tenet_storage::{install_controller_authority_key, Storage};

mod support;

const TEST_NAME: &str = "controller_authority_identity_revalidates_only_with_the_same_key";
const MODE: &str = "TENET_TEST_AUTHORITY_RESTART_MODE";
const REPOSITORY: &str = "TENET_TEST_AUTHORITY_RESTART_REPOSITORY";
const NAMESPACE: &str = "tenet-authority-restart-tests";
const VALID_KEY: &[u8] = b"0123456789abcdef0123456789abcdef";
const INVALID_KEY: &[u8] = b"fedcba9876543210fedcba9876543210";

fn spec() -> TrustedVerificationSpec {
  TrustedVerificationSpec {
    name: "restart-boundary".into(),
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
  }
}

fn record(spec: &TrustedVerificationSpec) -> TrustedExecutionRecord {
  let now = Utc::now();
  TrustedExecutionRecord {
    id: VerificationRunId::new(),
    revision: "revision-1".into(),
    input_materialization_hash: "archive-hash".into(),
    verifier_name: spec.name.clone(),
    spec_hash: spec.fingerprint().expect("spec fingerprint"),
    isolation_policy_hash: spec.isolation_policy_hash().expect("policy fingerprint"),
    isolation_report: Some(IsolationCapabilityReport {
      backend: TrustedExecutionBackend::Microsandbox,
      backend_version: "microsandbox-rust-sdk/0.6.15".into(),
      runtime_identity: "microsandbox-local-runtime/0.6.15".into(),
      boundary: IsolationBoundary::HardwareVirtualizedMicroVm,
      image: spec.image.clone(),
      resolved_image_digest: format!("sha256:{}", "b".repeat(64)),
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
    result: TrustedExecutionResult::Supports,
    observation: ExecutionObservation {
      command: spec.fingerprint().expect("command identity"),
      exit_code: Some(0),
      timed_out: false,
      duration_ms: 1,
      stdout: String::new(),
      stderr: String::new(),
    },
    obligation_ids: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
  }
}

async fn write_authority(repository: &Path) {
  install_controller_authority_key(NAMESPACE, VALID_KEY).expect("install writer identity");
  let storage = Storage::open(repository)
    .await
    .expect("open writer storage");
  let mut catalog = support::catalog();
  catalog.verification_obligations[0].evidence_contract = EvidenceContract::Artifact {
    predicate: EvidencePredicate::TrustedVerifierCheck {
      name: "restart-boundary".into(),
    },
  };
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("persist catalog");
  storage.create_run("run-1").await.expect("create run");
  let spec = spec();
  let record = record(&spec);
  storage
    .record_trusted_execution("run-1", &record, &spec)
    .await
    .expect("persist trusted execution");
  let mut graph = support::empty_graph(&catalog);
  graph
    .record_trusted_execution(&record, &spec)
    .expect("record trusted artifact")
    .expect("authoritative artifact");
  graph.derive_proofs("revision-1");
  storage
    .persist_evidence_graph("run-1", &graph)
    .await
    .expect("persist evidence graph");
}

async fn read_authority(repository: &Path, key: &[u8], should_validate: bool) {
  install_controller_authority_key(NAMESPACE, key).expect("install reader identity");
  let storage = Storage::open_existing(repository)
    .await
    .expect("open reader storage");
  let catalog = storage
    .load_active_catalog()
    .await
    .expect("load catalog")
    .expect("active catalog");
  let graph = storage
    .load_evidence_graph(&catalog, &[spec()])
    .await
    .expect("load evidence graph");
  let proof = &graph.proof_derivations[&ObligationId::from("REQ-001/AC-01/VO-01")];
  if should_validate {
    assert_eq!(graph.artifacts.len(), 1);
    assert_eq!(proof.state, ProofState::Proven);
  } else {
    assert!(graph.artifacts.is_empty());
    assert_ne!(proof.state, ProofState::Proven);
  }
}

#[tokio::test]
async fn controller_authority_identity_revalidates_only_with_the_same_key() {
  if let Ok(mode) = std::env::var(MODE) {
    let repository = std::env::var(REPOSITORY).expect("child repository path");
    match mode.as_str() {
      "write" => write_authority(Path::new(&repository)).await,
      "read-valid" => read_authority(Path::new(&repository), VALID_KEY, true).await,
      "read-invalid" => read_authority(Path::new(&repository), INVALID_KEY, false).await,
      other => panic!("unknown child mode {other}"),
    }
    return;
  }

  let repository = tempfile::tempdir().expect("authority restart repository");
  for mode in ["write", "read-valid", "read-invalid"] {
    let status = Command::new(std::env::current_exe().expect("current test executable"))
      .args(["--exact", TEST_NAME, "--nocapture"])
      .env(MODE, mode)
      .env(REPOSITORY, repository.path())
      .status()
      .expect("run authority restart child");
    assert!(status.success(), "authority restart child {mode} failed");
  }
}
