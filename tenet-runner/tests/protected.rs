//! Adversarial tests for `PROTECTED_V1` enforcement.
//!
//! Every test that requires real OS enforcement is runtime-gated on backend
//! availability and skips explicitly when absent, so a platform without
//! Seatbelt/Bubblewrap never hard-fails. The fail-closed direction is checked
//! on the opposite gate: when no backend exists, a protected run must return an
//! infrastructure result and must never claim `PROTECTED_V1`.

#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

use tenet_application::ports::{ExecutedVerifier, VerifierRun, VerifierRunner};
use tenet_domain::{
  algebra::{EvidenceResult, LOCAL_V1, PROTECTED_V1},
  evidence::{AuthorityId, CandidateId, ContentObjectId, OracleIdentity},
  policy::{
    CommandArgument, CommandCwd, CommandSpec, EnvironmentSpec, ExitCodePolicy, VerifierAuthority,
    VerifierProtection, VerifierSpec,
  },
};
use tenet_runner::{LocalProcessRunner, protection_backend_available};

fn content(byte: char) -> ContentObjectId {
  ContentObjectId(format!("sha256:{}", byte.to_string().repeat(64)))
}

fn exit_policy() -> ExitCodePolicy {
  ExitCodePolicy {
    pass: [0].into(),
    fail: [1].into(),
    inconclusive: [].into(),
  }
}

struct Sandbox {
  candidate: tempfile::TempDir,
  authority: tempfile::TempDir,
  scratch: tempfile::TempDir,
  output: tempfile::TempDir,
}

impl Sandbox {
  fn new(script: &str) -> Self {
    let sandbox = Self {
      candidate: tempfile::tempdir().expect("candidate"),
      authority: tempfile::tempdir().expect("authority"),
      scratch: tempfile::tempdir().expect("scratch"),
      output: tempfile::tempdir().expect("output"),
    };
    fs::write(
      sandbox.candidate.path().join("candidate.txt"),
      "original candidate",
    )
    .expect("candidate file");
    fs::write(sandbox.authority.path().join("authority.json"), "{}").expect("authority file");
    let program = sandbox.candidate.path().join("verify.sh");
    fs::write(&program, format!("#!/bin/sh\n{script}\nexit 0\n")).expect("script");
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).expect("executable");
    sandbox
  }

  fn run(&self, protection: VerifierProtection) -> ExecutedVerifier {
    self.run_with_scratch(self.scratch.path(), protection)
  }

  fn run_with_scratch(
    &self,
    scratch: &std::path::Path,
    protection: VerifierProtection,
  ) -> ExecutedVerifier {
    let verifier = VerifierSpec {
      id: "V1".into(),
      command: CommandSpec {
        argv: vec![CommandArgument::CandidatePath("verify.sh".into())],
        cwd: CommandCwd::Candidate(".".into()),
        env: EnvironmentSpec::default(),
        timeout_ms: 5_000,
        result: exit_policy(),
      },
      max_output_bytes: 4_096,
      authority: VerifierAuthority::Project,
      oracle_path: None,
      protection,
    };
    let candidate_id = CandidateId(content('b'));
    let oracle = OracleIdentity::Project {
      verifier_id: "V1".into(),
      candidate_id: candidate_id.clone(),
      definition_digest: "sha256:definition".into(),
    };
    LocalProcessRunner
      .run(&VerifierRun {
        candidate_root: self.candidate.path(),
        authority_root: self.authority.path(),
        scratch_root: scratch,
        output_root: self.output.path(),
        verifier: &verifier,
        authority_id: &AuthorityId(content('a')),
        candidate_id: &candidate_id,
        oracle_identity: &oracle,
      })
      .expect("run verifier")
  }

  fn candidate_text(&self) -> String {
    fs::read_to_string(self.candidate.path().join("candidate.txt")).expect("candidate text")
  }

  fn authority_text(&self) -> String {
    fs::read_to_string(self.authority.path().join("authority.json")).expect("authority text")
  }
}

fn backend_present() -> bool {
  if protection_backend_available() {
    true
  } else {
    eprintln!("SKIP: no protected-verification backend on this platform");
    false
  }
}

fn assert_backend_enforced(executed: &ExecutedVerifier) {
  if !backend_present() {
    return;
  }
  assert_eq!(executed.result, EvidenceResult::Pass);
  assert_eq!(executed.context.assurance.0, PROTECTED_V1);
  assert_eq!(executed.execution.assurance.0, PROTECTED_V1);
}

#[test]
fn protected_verifier_reports_protected_v1_assurance() {
  let sandbox = Sandbox::new("true");
  let executed = sandbox.run(VerifierProtection::Protected);
  assert_backend_enforced(&executed);
}

#[test]
fn protected_verifier_cannot_write_candidate() {
  let sandbox =
    Sandbox::new("echo tampered >> \"$TENET_CANDIDATE_ROOT/candidate.txt\" 2>/dev/null || true");
  let executed = sandbox.run(VerifierProtection::Protected);
  assert_backend_enforced(&executed);
  if backend_present() {
    assert_eq!(sandbox.candidate_text(), "original candidate");
  }
}

#[test]
fn protected_verifier_cannot_write_authority() {
  let sandbox =
    Sandbox::new("echo tampered >> \"$TENET_AUTHORITY_ROOT/authority.json\" 2>/dev/null || true");
  let executed = sandbox.run(VerifierProtection::Protected);
  assert_backend_enforced(&executed);
  if backend_present() {
    assert_eq!(sandbox.authority_text(), "{}");
  }
}

#[test]
fn protected_verifier_scratch_and_output_are_writable() {
  let sandbox = Sandbox::new(
    "echo work > \"$TENET_SCRATCH_ROOT/work.txt\" && echo report > \"$TENET_OUTPUT_ROOT/report.txt\" && test -n \"$TMPDIR\"",
  );
  let executed = sandbox.run(VerifierProtection::Protected);
  assert_backend_enforced(&executed);
  if backend_present() {
    assert_eq!(
      fs::read_to_string(sandbox.scratch.path().join("work.txt")).expect("scratch write"),
      "work\n"
    );
    assert_eq!(
      fs::read_to_string(sandbox.output.path().join("report.txt")).expect("output write"),
      "report\n"
    );
  }
}

#[test]
fn protected_verifier_sees_scratch_isolated_from_previous_run() {
  let sandbox = Sandbox::new("test ! -e \"$TENET_SCRATCH_ROOT/work.txt\"");
  if backend_present() {
    // Seed the first scratch; a fresh run gets a different scratch directory.
    fs::write(sandbox.scratch.path().join("work.txt"), "stale").expect("seed");
    let fresh = tempfile::tempdir().expect("fresh scratch");
    let executed = sandbox.run_with_scratch(fresh.path(), VerifierProtection::Protected);
    assert_backend_enforced(&executed);
  } else {
    let executed = sandbox.run(VerifierProtection::Protected);
    assert_backend_enforced(&executed);
  }
}

#[test]
fn protected_background_child_cannot_mutate_candidate_after_exit() {
  let sandbox = Sandbox::new(
    "( sleep 0.5; echo tampered >> \"$TENET_CANDIDATE_ROOT/candidate.txt\" ) & exit 0",
  );
  let executed = sandbox.run(VerifierProtection::Protected);
  assert_backend_enforced(&executed);
  std::thread::sleep(Duration::from_millis(900));
  if backend_present() {
    assert_eq!(sandbox.candidate_text(), "original candidate");
  }
}

#[test]
fn local_protection_keeps_local_v1_assurance() {
  let sandbox = Sandbox::new("echo local >> \"$TENET_CANDIDATE_ROOT/candidate.txt\"");
  let executed = sandbox.run(VerifierProtection::Local);
  assert_eq!(executed.result, EvidenceResult::Pass);
  assert_eq!(executed.context.assurance.0, LOCAL_V1);
  assert_eq!(executed.execution.assurance.0, LOCAL_V1);
}

#[test]
fn protected_run_fails_closed_without_a_backend() {
  if protection_backend_available() {
    eprintln!("SKIP: a protected-verification backend exists on this platform");
    return;
  }
  let sandbox = Sandbox::new("true");
  let executed = sandbox.run(VerifierProtection::Protected);
  assert_eq!(executed.result, EvidenceResult::InfrastructureError);
  let message = executed
    .infrastructure_error
    .as_deref()
    .expect("infrastructure error");
  assert!(
    message.contains("protected") && message.contains("downgrade"),
    "unexpected message: {message}"
  );
  assert_ne!(executed.context.assurance.0, PROTECTED_V1);
}

#[test]
fn protected_run_cannot_escape_through_parent_directory_write() {
  let sandbox =
    Sandbox::new("echo escaped > \"$TENET_CANDIDATE_ROOT/../escape.txt\" 2>/dev/null || true");
  let executed = sandbox.run(VerifierProtection::Protected);
  assert_backend_enforced(&executed);
  if backend_present() {
    let escape = sandbox
      .candidate
      .path()
      .parent()
      .expect("parent")
      .join("escape.txt");
    assert!(!escape.exists(), "protected verifier escaped the sandbox");
  }
}
