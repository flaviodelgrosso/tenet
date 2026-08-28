use std::{fs, path::Path};

use tenet_application::{
  application::{ApproveRequest, GateRequest, InitializeRequest, Tenet},
  response::ContractState,
};
use tenet_domain::{
  completion::Verdict,
  contract::{
    ClaimEvidenceContractInput, ContractProposalInput, EvidenceContractInput, ObligationId,
    RequirementId, RequirementInput, VerificationObligationInput,
  },
  policy::{ProjectConfig, VerifierAuthority, VerifierSpec},
};

fn write(path: &Path, content: &str) {
  fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
  fs::write(path, content).expect("write file");
}
#[cfg(unix)]
fn executable(path: &Path) {
  use std::os::unix::fs::PermissionsExt;
  let mut permissions = fs::metadata(path).expect("metadata").permissions();
  permissions.set_mode(0o755);
  fs::set_permissions(path, permissions).expect("chmod");
}

fn proposal(tenet: &Tenet) -> ContractProposalInput {
  let status = tenet.status().expect("status");
  ContractProposalInput {
    spec_digest: status.spec_digest.expect("spec digest"),
    policy_digest: status.policy_digest.expect("policy digest"),
    requirements: vec![RequirementInput {
      id: RequirementId("REQ-001".into()),
      statement: "candidate passes quality check".into(),
      obligations: vec![VerificationObligationInput {
        id: ObligationId("REQ-001/VO-001".into()),
        statement: "quality verifier succeeds".into(),
        evidence_contract: EvidenceContractInput {
          claim: ClaimEvidenceContractInput {
            verifier_id: "quality".into(),
          },
          oracle_assurances: Vec::new(),
        },
      }],
    }],
  }
}

fn configure_project_verifier(root: &Path) {
  let policy = ProjectConfig {
    version: 1,
    spec_path: "SPEC.md".into(),
    verifiers: vec![VerifierSpec {
      id: "quality".into(),
      argv: vec!["verify.sh".into()],
      cwd: ".".into(),
      timeout_seconds: 10,
      max_output_bytes: 4096,
      env: Default::default(),
      environment_mode: Default::default(),
      authority: VerifierAuthority::Project,
      oracle_path: None,
    }],
  };
  write(
    &root.join(".tenet/tenet.toml"),
    &toml::to_string_pretty(&policy).expect("policy"),
  );
}

fn configure_authority_verifier(root: &Path, oracle_path: &str, argv: &str, cwd: &str) {
  let policy = ProjectConfig {
    version: 1,
    spec_path: "SPEC.md".into(),
    verifiers: vec![VerifierSpec {
      id: "quality".into(),
      argv: vec![argv.into()],
      cwd: cwd.into(),
      timeout_seconds: 10,
      max_output_bytes: 4096,
      env: Default::default(),
      environment_mode: Default::default(),
      authority: VerifierAuthority::AuthoritySnapshot,
      oracle_path: Some(oracle_path.into()),
    }],
  };
  write(
    &root.join(".tenet/tenet.toml"),
    &toml::to_string_pretty(&policy).expect("policy"),
  );
}

#[test]
fn plain_directory_workflow_seals_and_gates_exact_content() {
  let directory = tempfile::tempdir().expect("plain directory");
  let root = directory.path();
  let tenet = Tenet::new(root.to_path_buf());
  let initialized = tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init without VCS");
  assert!(initialized.created);
  assert!(!root.join(".git").exists());
  configure_project_verifier(root);
  write(&root.join("verify.sh"), "#!/bin/sh\nexit 0\n");
  executable(&root.join("verify.sh"));
  let proposed = tenet.propose(proposal(&tenet)).expect("propose");
  let approved = tenet
    .approve(&ApproveRequest {
      proposal_id: proposed.proposal_id,
      proposal_digest: proposed.proposal_digest,
    })
    .expect("explicit approval");
  assert!(!approved.contract_digest.is_empty());
  let authority = tenet.authority_seal().expect("seal authority");
  let candidate = tenet.candidate_capture().expect("capture candidate");
  let result = tenet
    .gate(&GateRequest {
      authority_id: authority.authority_id.clone(),
      candidate_id: candidate.candidate_id.clone(),
    })
    .expect("gate snapshots");
  assert_eq!(result.verdict, Verdict::Done, "{result:#?}");
  assert_eq!(result.authority_id, authority.authority_id);
  assert_eq!(result.candidate_id, candidate.candidate_id);
}

#[test]
fn nearest_initialized_root_is_discovered_from_subdirectory() {
  let directory = tempfile::tempdir().expect("plain directory");
  let root = directory.path();
  let tenet = Tenet::new(root.to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  let nested = root.join("work/nested");
  fs::create_dir_all(&nested).expect("nested");
  let status = Tenet::new(nested).status().expect("discover nearest root");
  assert!(status.initialized);
  assert_eq!(status.contract_state, ContractState::Missing);
}

#[test]
fn captured_candidate_and_authority_are_isolated_from_workspace_changes() {
  let directory = tempfile::tempdir().expect("plain directory");
  let root = directory.path();
  let tenet = Tenet::new(root.to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  configure_project_verifier(root);
  write(&root.join("verify.sh"), "#!/bin/sh\nexit 0\n");
  executable(&root.join("verify.sh"));
  let proposed = tenet.propose(proposal(&tenet)).expect("propose");
  tenet
    .approve(&ApproveRequest {
      proposal_id: proposed.proposal_id,
      proposal_digest: proposed.proposal_digest,
    })
    .expect("approve");
  let authority = tenet.authority_seal().expect("seal");
  let candidate = tenet.candidate_capture().expect("capture");
  write(&root.join("SPEC.md"), "mutable replacement\n");
  write(&root.join("verify.sh"), "#!/bin/sh\nexit 1\n");
  executable(&root.join("verify.sh"));
  let result = tenet
    .gate(&GateRequest {
      authority_id: authority.authority_id,
      candidate_id: candidate.candidate_id,
    })
    .expect("gate sealed snapshots");
  assert_eq!(result.verdict, Verdict::Done, "{result:#?}");
}

#[test]
fn authority_snapshot_requires_directory_and_bundled_executable_before_proposal() {
  let directory = tempfile::tempdir().expect("plain directory");
  let root = directory.path();
  let tenet = Tenet::new(root.to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  write(
    &root.join(".tenet/oracles/not-a-directory"),
    "not a directory",
  );
  configure_authority_verifier(
    root,
    ".tenet/oracles/not-a-directory",
    "not-a-directory",
    ".",
  );
  let error = tenet
    .propose(proposal(&tenet))
    .expect_err("file oracle bundle must fail");
  assert!(format!("{error:#}").contains("oracle_bundle_not_directory"));

  fs::remove_file(root.join(".tenet/oracles/not-a-directory")).expect("remove file");
  fs::create_dir_all(root.join(".tenet/oracles")).expect("oracle directory");
  write(
    &root.join(".tenet/oracles/verify.sh"),
    "#!/bin/sh\nexit 0\n",
  );
  executable(&root.join(".tenet/oracles/verify.sh"));
  configure_authority_verifier(root, ".tenet/oracles", "sh", ".");
  let error = tenet
    .propose(proposal(&tenet))
    .expect_err("host shell must not be implicit");
  assert!(format!("{error:#}").contains("oracle_executable_missing"));
}

#[test]
fn authority_snapshot_verifier_runs_sealed_bundle_against_captured_candidate() {
  let directory = tempfile::tempdir().expect("plain directory");
  let root = directory.path();
  let tenet = Tenet::new(root.to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  fs::create_dir_all(root.join(".tenet/oracles")).expect("oracle directory");
  write(
    &root.join(".tenet/oracles/verify.sh"),
    "#!/bin/sh\ntest -f \"$TENET_CANDIDATE_ROOT/subject\"\n",
  );
  executable(&root.join(".tenet/oracles/verify.sh"));
  write(&root.join("subject"), "candidate content");
  configure_authority_verifier(root, ".tenet/oracles", "verify.sh", ".");
  let proposed = tenet.propose(proposal(&tenet)).expect("proposal");
  tenet
    .approve(&ApproveRequest {
      proposal_id: proposed.proposal_id,
      proposal_digest: proposed.proposal_digest,
    })
    .expect("approval");
  let authority = tenet.authority_seal().expect("seal valid authority");
  let candidate = tenet.candidate_capture().expect("capture candidate");
  let result = tenet
    .gate(&GateRequest {
      authority_id: authority.authority_id,
      candidate_id: candidate.candidate_id,
    })
    .expect("gate");
  assert_eq!(result.verdict, Verdict::Done, "{result:#?}");
}

#[test]
fn candidate_capture_excludes_tenet_administration_and_binds_candidate_content() {
  let directory = tempfile::tempdir().expect("plain directory");
  let root = directory.path();
  let tenet = Tenet::new(root.to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  write(&root.join("candidate"), "one");
  let first = tenet.candidate_capture().expect("first capture");
  write(&root.join(".tenet/state.json"), "{\"mutable\":\"audit\"}");
  let repeated = tenet.candidate_capture().expect("second capture");
  assert_eq!(first.candidate_id, repeated.candidate_id);
  write(&root.join("candidate"), "two");
  let changed = tenet.candidate_capture().expect("changed capture");
  assert_ne!(first.candidate_id, changed.candidate_id);
}

#[test]
fn authority_snapshot_validation_reports_executable_and_cwd_failures() {
  let directory = tempfile::tempdir().expect("plain directory");
  let root = directory.path();
  let tenet = Tenet::new(root.to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  fs::create_dir_all(root.join(".tenet/oracles")).expect("oracle directory");
  write(
    &root.join(".tenet/oracles/verify.sh"),
    "#!/bin/sh\nexit 0\n",
  );
  configure_authority_verifier(root, ".tenet/oracles", "verify.sh", ".");
  let error = tenet
    .propose(proposal(&tenet))
    .expect_err("non-executable oracle must fail");
  assert!(format!("{error:#}").contains("oracle_executable_not_executable"));
  executable(&root.join(".tenet/oracles/verify.sh"));
  configure_authority_verifier(root, ".tenet/oracles", "verify.sh", "missing");
  let error = tenet
    .propose(proposal(&tenet))
    .expect_err("missing bundle cwd must fail");
  assert!(format!("{error:#}").contains("oracle_cwd_missing"));
}
