use std::{fs, path::Path};

use tenet_application::{
  application::{
    ApproveRequest, CandidateCaptureRequest, EvidenceRequest, GateRequest, InitializeRequest, Tenet,
  },
  response::ContractState,
};
use tenet_domain::{
  completion::{BlockerCode, Verdict},
  contract::{
    ClaimEvidenceContractInput, ContractProposalInput, EvidenceContractInput, ObligationId,
    RequirementId, RequirementInput, VerificationObligationInput,
  },
  evidence::{AuthorityId, CandidateId, ContentObjectId},
  policy::{CandidateCapturePolicy, ProjectConfig, VerifierAuthority, VerifierSpec},
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
  configure_project_verifier_with_candidate(root, CandidateCapturePolicy::default());
}

fn configure_project_verifier_with_candidate(root: &Path, candidate: CandidateCapturePolicy) {
  let policy = ProjectConfig {
    version: 1,
    spec_path: "SPEC.md".into(),
    candidate,
    verifiers: vec![VerifierSpec {
      id: "quality".into(),
      argv: vec!["./verify.sh".into()],
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

fn configure_project_verifier_with_environment(root: &Path, marker: &str) {
  configure_project_verifier(root);
  let path = root.join(".tenet/tenet.toml");
  let mut policy: ProjectConfig =
    toml::from_str(&fs::read_to_string(&path).expect("read policy")).expect("parse policy");
  policy.verifiers[0]
    .env
    .insert("TENET_MARKER".into(), marker.into());
  write(
    &path,
    &toml::to_string_pretty(&policy).expect("policy with environment"),
  );
}

fn configure_authority_verifier(root: &Path, oracle_path: &str, argv: &str, cwd: &str) {
  let policy = ProjectConfig {
    version: 1,
    spec_path: "SPEC.md".into(),
    candidate: Default::default(),
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
  let candidate = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: authority.authority_id.clone(),
    })
    .expect("capture candidate");
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

#[cfg(unix)]
#[test]
fn project_verifier_uses_os_path_search_in_exact_candidate_cwd() {
  let directory = tempfile::tempdir().expect("plain directory");
  let root = directory.path();
  let tenet = Tenet::new(root.to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  configure_project_verifier(root);
  let policy_path = root.join(".tenet/tenet.toml");
  let mut policy: ProjectConfig =
    toml::from_str(&fs::read_to_string(&policy_path).expect("read policy")).expect("parse policy");
  policy.verifiers[0].argv = vec!["sh".into(), "-c".into(), "test -f subject".into()];
  write(
    &policy_path,
    &toml::to_string_pretty(&policy).expect("path-search policy"),
  );
  write(&root.join("subject"), "captured");
  let proposed = tenet.propose(proposal(&tenet)).expect("proposal");
  tenet
    .approve(&ApproveRequest {
      proposal_id: proposed.proposal_id,
      proposal_digest: proposed.proposal_digest,
    })
    .expect("approval");
  let authority = tenet.authority_seal().expect("seal");
  let candidate = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: authority.authority_id.clone(),
    })
    .expect("capture");
  fs::remove_file(root.join("subject")).expect("remove mutable subject");
  let result = tenet
    .gate(&GateRequest {
      authority_id: authority.authority_id,
      candidate_id: candidate.candidate_id,
    })
    .expect("gate");
  assert_eq!(result.verdict, Verdict::Done, "{result:#?}");
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
  let candidate = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: authority.authority_id.clone(),
    })
    .expect("capture candidate");
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
  let candidate = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: authority.authority_id.clone(),
    })
    .expect("capture candidate");
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
  configure_project_verifier(root);
  write(&root.join("verify.sh"), "#!/bin/sh\nexit 0\n");
  executable(&root.join("verify.sh"));
  write(&root.join(".git/objects/pack"), "ignored one");
  write(&root.join(".git/logs/HEAD"), "ignored log");
  write(&root.join(".gitignore"), "tracked ignore");
  write(
    &root.join(".github/workflows/check.yml"),
    "tracked workflow",
  );
  let proposed = tenet.propose(proposal(&tenet)).expect("proposal");
  tenet
    .approve(&ApproveRequest {
      proposal_id: proposed.proposal_id,
      proposal_digest: proposed.proposal_digest,
    })
    .expect("approval");
  let authority = tenet.authority_seal().expect("seal");
  write(&root.join("candidate"), "one");
  write(
    &root.join(".tenet/store/metadata"),
    "ignored store metadata",
  );
  let first = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: authority.authority_id.clone(),
    })
    .expect("first capture");
  write(&root.join(".git/objects/pack"), "ignored two");
  write(&root.join(".git/logs/HEAD"), "ignored log two");
  write(&root.join(".tenet/state.json"), "{\"mutable\":\"audit\"}");
  let repeated = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: authority.authority_id.clone(),
    })
    .expect("second capture");
  assert_eq!(first.candidate_id, repeated.candidate_id);
  write(&root.join(".gitignore"), "tracked ignore changed");
  write(
    &root.join(".github/workflows/check.yml"),
    "tracked workflow changed",
  );
  let changed_tracked_file = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: authority.authority_id.clone(),
    })
    .expect("changed tracked metadata capture");
  assert_ne!(first.candidate_id, changed_tracked_file.candidate_id);
  write(&root.join("candidate"), "two");
  let changed = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: authority.authority_id,
    })
    .expect("changed capture");
  assert_ne!(first.candidate_id, changed.candidate_id);
}

#[test]
fn candidate_capture_uses_the_selected_authority_boundary() {
  let directory = tempfile::tempdir().expect("plain directory");
  let root = directory.path();
  let tenet = Tenet::new(root.to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  fs::create_dir_all(root.join("candidate/ignored")).expect("candidate root");
  write(
    &root.join("candidate/verify.sh"),
    "#!/bin/sh\ntest -f subject && test ! -e ignored/secret && test ! -e ../outside\n",
  );
  executable(&root.join("candidate/verify.sh"));
  write(&root.join("candidate/subject"), "candidate");
  write(&root.join("candidate/ignored/secret"), "excluded");
  write(&root.join("outside"), "not in candidate root");
  configure_project_verifier_with_candidate(
    root,
    CandidateCapturePolicy {
      root: "candidate".into(),
      exclude: vec!["ignored/**".into()],
    },
  );
  let proposed = tenet.propose(proposal(&tenet)).expect("proposal");
  tenet
    .approve(&ApproveRequest {
      proposal_id: proposed.proposal_id,
      proposal_digest: proposed.proposal_digest,
    })
    .expect("approval");
  let authority = tenet.authority_seal().expect("seal");

  configure_project_verifier(root);
  let candidate = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: authority.authority_id.clone(),
    })
    .expect("capture with sealed boundary");
  let result = tenet
    .gate(&GateRequest {
      authority_id: authority.authority_id,
      candidate_id: candidate.candidate_id,
    })
    .expect("gate");
  assert_eq!(result.verdict, Verdict::Done, "{result:#?}");
}

#[test]
fn corrupt_snapshot_content_is_reported_as_infrastructure_failure() {
  let directory = tempfile::tempdir().expect("plain directory");
  let root = directory.path();
  let tenet = Tenet::new(root.to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  configure_project_verifier(root);
  write(&root.join("verify.sh"), "#!/bin/sh\nexit 0\n");
  executable(&root.join("verify.sh"));
  let proposed = tenet.propose(proposal(&tenet)).expect("proposal");
  tenet
    .approve(&ApproveRequest {
      proposal_id: proposed.proposal_id,
      proposal_digest: proposed.proposal_digest,
    })
    .expect("approval");
  let authority = tenet.authority_seal().expect("seal");
  write(&root.join("candidate"), "one");
  let candidate = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: authority.authority_id.clone(),
    })
    .expect("capture");
  let object = root.join(".tenet/store/snapshots").join(
    candidate
      .candidate_id
      .0
      .0
      .strip_prefix("sha256:")
      .expect("digest"),
  );
  fs::write(object.join("tree/candidate"), "corrupt").expect("corrupt object");

  let result = tenet
    .gate(&GateRequest {
      authority_id: authority.authority_id,
      candidate_id: candidate.candidate_id,
    })
    .expect("gate reports infrastructure result");
  assert_eq!(result.verdict, Verdict::InfrastructureError);
  assert_eq!(
    result.blockers.first().map(|blocker| &blocker.code),
    Some(&BlockerCode::ContentIntegrityFailure)
  );
}

#[test]
fn missing_or_corrupt_authority_and_candidate_are_infrastructure_failures() {
  let directory = tempfile::tempdir().expect("plain directory");
  let root = directory.path();
  let tenet = Tenet::new(root.to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  configure_project_verifier(root);
  write(&root.join("verify.sh"), "#!/bin/sh\nexit 0\n");
  executable(&root.join("verify.sh"));
  let proposed = tenet.propose(proposal(&tenet)).expect("proposal");
  tenet
    .approve(&ApproveRequest {
      proposal_id: proposed.proposal_id,
      proposal_digest: proposed.proposal_digest,
    })
    .expect("approval");
  let authority = tenet.authority_seal().expect("seal");
  write(&root.join("candidate"), "candidate");
  let candidate = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: authority.authority_id.clone(),
    })
    .expect("capture");

  let missing_candidate = CandidateId(ContentObjectId(
    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".into(),
  ));
  let missing = tenet
    .gate(&GateRequest {
      authority_id: authority.authority_id.clone(),
      candidate_id: missing_candidate,
    })
    .expect("missing candidate gate");
  assert_eq!(missing.verdict, Verdict::InfrastructureError);
  assert_eq!(
    missing.blockers.first().map(|blocker| &blocker.code),
    Some(&BlockerCode::ContentObjectMissing)
  );

  let authority_object = root.join(".tenet/store/snapshots").join(
    authority
      .authority_id
      .0
      .0
      .strip_prefix("sha256:")
      .expect("authority digest"),
  );
  fs::write(
    authority_object.join("tree/.tenet/contract.json"),
    "corrupt authority",
  )
  .expect("corrupt authority object");
  let corrupt = tenet
    .gate(&GateRequest {
      authority_id: authority.authority_id,
      candidate_id: candidate.candidate_id,
    })
    .expect("corrupt authority gate");
  assert_eq!(corrupt.verdict, Verdict::InfrastructureError);
  assert_eq!(
    corrupt.blockers.first().map(|blocker| &blocker.code),
    Some(&BlockerCode::ContentIntegrityFailure)
  );
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
  assert_eq!(error.code, "oracle_executable_not_executable");
  executable(&root.join(".tenet/oracles/verify.sh"));
  configure_authority_verifier(root, ".tenet/oracles", "verify.sh", "missing");
  let error = tenet
    .propose(proposal(&tenet))
    .expect_err("missing bundle cwd must fail");
  assert_eq!(error.code, "oracle_cwd_missing");
}

#[test]
fn evidence_lookup_requires_the_exact_authority_and_candidate_pair() {
  let directory = tempfile::tempdir().expect("plain directory");
  let root = directory.path();
  let tenet = Tenet::new(root.to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  configure_project_verifier(root);
  write(&root.join("verify.sh"), "#!/bin/sh\nexit 0\n");
  executable(&root.join("verify.sh"));
  let proposed = tenet.propose(proposal(&tenet)).expect("proposal");
  tenet
    .approve(&ApproveRequest {
      proposal_id: proposed.proposal_id,
      proposal_digest: proposed.proposal_digest,
    })
    .expect("approval");
  let authority = tenet.authority_seal().expect("seal");
  write(&root.join("candidate"), "one");
  let first = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: authority.authority_id.clone(),
    })
    .expect("first capture");
  tenet
    .gate(&GateRequest {
      authority_id: authority.authority_id.clone(),
      candidate_id: first.candidate_id.clone(),
    })
    .expect("first gate");
  write(&root.join("candidate"), "two");
  let second = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: authority.authority_id.clone(),
    })
    .expect("second capture");
  tenet
    .gate(&GateRequest {
      authority_id: authority.authority_id.clone(),
      candidate_id: second.candidate_id.clone(),
    })
    .expect("second gate");

  let first_evidence = tenet
    .exact_evidence(&EvidenceRequest {
      authority_id: authority.authority_id.clone(),
      candidate_id: first.candidate_id.clone(),
    })
    .expect("first evidence");
  assert_eq!(first_evidence.authority_id, authority.authority_id);
  assert_eq!(first_evidence.candidate_id, first.candidate_id);
  assert_eq!(first_evidence.gates.len(), 1);
  assert_eq!(first_evidence.gates[0].candidate_id, first.candidate_id);
  let second_evidence = tenet
    .exact_evidence(&EvidenceRequest {
      authority_id: authority.authority_id,
      candidate_id: second.candidate_id.clone(),
    })
    .expect("second evidence");
  assert_eq!(second_evidence.gates.len(), 1);
  assert_eq!(second_evidence.gates[0].candidate_id, second.candidate_id);
}

#[test]
fn evidence_lookup_separates_authorities_for_the_same_candidate() {
  let directory = tempfile::tempdir().expect("plain directory");
  let root = directory.path();
  let tenet = Tenet::new(root.to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  configure_project_verifier(root);
  write(&root.join("verify.sh"), "#!/bin/sh\nexit 0\n");
  executable(&root.join("verify.sh"));
  let first_proposal = tenet.propose(proposal(&tenet)).expect("first proposal");
  tenet
    .approve(&ApproveRequest {
      proposal_id: first_proposal.proposal_id,
      proposal_digest: first_proposal.proposal_digest,
    })
    .expect("first approval");
  let first_authority = tenet.authority_seal().expect("first seal");
  let candidate = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: first_authority.authority_id.clone(),
    })
    .expect("candidate capture");
  tenet
    .gate(&GateRequest {
      authority_id: first_authority.authority_id.clone(),
      candidate_id: candidate.candidate_id.clone(),
    })
    .expect("first gate");

  configure_project_verifier_with_environment(root, "second-authority");
  let second_proposal = tenet.propose(proposal(&tenet)).expect("second proposal");
  tenet
    .approve(&ApproveRequest {
      proposal_id: second_proposal.proposal_id,
      proposal_digest: second_proposal.proposal_digest,
    })
    .expect("second approval");
  let second_authority = tenet.authority_seal().expect("second seal");
  let same_candidate = tenet
    .candidate_capture(&CandidateCaptureRequest {
      authority_id: second_authority.authority_id.clone(),
    })
    .expect("capture under second authority");
  assert_ne!(first_authority.authority_id, second_authority.authority_id);
  assert_eq!(candidate.candidate_id, same_candidate.candidate_id);
  tenet
    .gate(&GateRequest {
      authority_id: second_authority.authority_id.clone(),
      candidate_id: same_candidate.candidate_id.clone(),
    })
    .expect("second gate");

  let first_evidence = tenet
    .exact_evidence(&EvidenceRequest {
      authority_id: first_authority.authority_id,
      candidate_id: candidate.candidate_id.clone(),
    })
    .expect("first exact evidence");
  let second_evidence = tenet
    .exact_evidence(&EvidenceRequest {
      authority_id: second_authority.authority_id,
      candidate_id: same_candidate.candidate_id,
    })
    .expect("second exact evidence");
  assert_eq!(first_evidence.gates.len(), 1);
  assert_eq!(second_evidence.gates.len(), 1);
  assert_eq!(first_evidence.gates[0].candidate_id, candidate.candidate_id);
  assert_eq!(
    second_evidence.gates[0].candidate_id,
    first_evidence.candidate_id
  );
}

#[cfg(unix)]
#[test]
fn authority_trust_surfaces_reject_symlink_components() {
  let directory = tempfile::tempdir().expect("plain directory");
  let external = tempfile::tempdir().expect("external directory");
  let root = directory.path();
  let tenet = Tenet::new(root.to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  fs::create_dir_all(external.path().join("oracles")).expect("external oracle");
  fs::write(
    external.path().join("oracles/verify.sh"),
    "#!/bin/sh\nexit 0\n",
  )
  .expect("external executable");
  executable(&external.path().join("oracles/verify.sh"));
  std::os::unix::fs::symlink(external.path().join("oracles"), root.join(".tenet/oracles"))
    .expect("oracle symlink");
  configure_authority_verifier(root, ".tenet/oracles", "verify.sh", ".");
  let error = tenet
    .propose(proposal(&tenet))
    .expect_err("symlinked authority bundle must fail");
  assert_eq!(error.code, "unsupported_symlink");

  fs::remove_file(root.join(".tenet/oracles")).expect("remove bundle symlink");
  fs::create_dir_all(root.join(".tenet/oracles")).expect("bundle directory");
  std::os::unix::fs::symlink(
    external.path().join("oracles/verify.sh"),
    root.join(".tenet/oracles/verify.sh"),
  )
  .expect("executable symlink");
  configure_authority_verifier(root, ".tenet/oracles", "verify.sh", ".");
  let error = tenet
    .propose(proposal(&tenet))
    .expect_err("symlinked executable must fail");
  assert_eq!(error.code, "unsupported_symlink");

  fs::remove_file(root.join(".tenet/oracles/verify.sh")).expect("remove executable symlink");
  write(
    &root.join(".tenet/oracles/verify.sh"),
    "#!/bin/sh\nexit 0\n",
  );
  executable(&root.join(".tenet/oracles/verify.sh"));
  fs::create_dir_all(external.path().join("cwd")).expect("external cwd");
  std::os::unix::fs::symlink(external.path().join("cwd"), root.join(".tenet/oracles/cwd"))
    .expect("cwd symlink");
  configure_authority_verifier(root, ".tenet/oracles", "verify.sh", "cwd");
  let error = tenet
    .propose(proposal(&tenet))
    .expect_err("symlinked cwd must fail");
  assert_eq!(error.code, "unsupported_symlink");

  fs::remove_file(root.join(".tenet/oracles/cwd")).expect("remove cwd symlink");
  configure_authority_verifier(root, ".tenet/oracles", "verify.sh", ".");
  let proposed = tenet.propose(proposal(&tenet)).expect("valid proposal");
  tenet
    .approve(&ApproveRequest {
      proposal_id: proposed.proposal_id,
      proposal_digest: proposed.proposal_digest,
    })
    .expect("valid approval");
  tenet.authority_seal().expect("valid authority seal");
}

#[test]
fn malformed_proposal_digest_is_a_structured_failure() {
  let directory = tempfile::tempdir().expect("plain directory");
  let tenet = Tenet::new(directory.path().to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  let error = tenet
    .approve(&ApproveRequest {
      proposal_id: "proposal".into(),
      proposal_digest: "../../outside".into(),
    })
    .expect_err("unsafe proposal digest must fail");
  assert_eq!(error.code, "proposal_digest_invalid");
  write(&directory.path().join(".tenet/tenet.toml"), "not valid = [");
  let error = tenet.status().expect_err("malformed policy must fail");
  assert_eq!(error.code, "policy_invalid");
}

#[test]
fn missing_snapshot_is_an_infrastructure_gate_result() {
  let directory = tempfile::tempdir().expect("plain directory");
  let tenet = Tenet::new(directory.path().to_path_buf());
  tenet
    .initialize(&InitializeRequest { spec_path: None })
    .expect("init");
  let authority_id = AuthorityId(ContentObjectId(
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
  ));
  let candidate_id = CandidateId(ContentObjectId(
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
  ));
  let result = tenet
    .gate(&GateRequest {
      authority_id,
      candidate_id,
    })
    .expect("missing content is represented by gate result");
  assert_eq!(result.verdict, Verdict::InfrastructureError);
  assert_eq!(
    result.blockers.first().map(|blocker| &blocker.code),
    Some(&BlockerCode::ContentObjectMissing)
  );
}
