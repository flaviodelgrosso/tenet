//! Adversarial tests for `PROTECTED_V1` enforcement.
//!
//! Every test that requires real OS enforcement is runtime-gated on backend
//! availability and skips explicitly when absent, so a platform without
//! Seatbelt/Bubblewrap never hard-fails. The fail-closed direction is checked
//! on the opposite gate: when no backend exists, a protected run must return an
//! infrastructure result and must never claim `PROTECTED_V1`.

#![cfg(unix)]

use std::{
  fs,
  os::unix::fs::PermissionsExt,
  path::Path,
  sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
  },
  thread,
  time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use tenet_application::ports::{ExecutedVerifier, VerifierRun, VerifierRunner, ViewDigest};
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

fn sha256_hex(bytes: &[u8]) -> String {
  let mut hasher = Sha256::new();
  hasher.update(bytes);
  hasher
    .finalize()
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect()
}

/// Walks a staged root and records the exact file expectations the private
/// namespace backend builds and verifies its copy from.
fn walk_view(
  root: &Path,
  prefix: &str,
  digests: &mut Vec<ViewDigest>,
  directories: &mut Vec<String>,
) {
  for entry in fs::read_dir(root).expect("read view root") {
    let entry = entry.expect("view entry");
    let path = entry.path();
    let view_path = format!(
      "{prefix}/{}",
      path
        .strip_prefix(root)
        .expect("relative path")
        .to_string_lossy()
    );
    if path.is_dir() {
      directories.push(view_path);
      walk_view(&path, prefix, digests, directories);
    } else {
      let mut hasher = Sha256::new();
      hasher.update(fs::read(&path).expect("view file"));
      let executable = entry
        .metadata()
        .expect("view metadata")
        .permissions()
        .mode()
        & 0o111
        != 0;
      digests.push(ViewDigest {
        path: view_path,
        sha256_hex: hasher
          .finalize()
          .iter()
          .map(|byte| format!("{byte:02x}"))
          .collect(),
        executable,
      });
    }
  }
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
    self.run_with_scratch(self.scratch.path(), protection, 5_000)
  }

  fn run_with_scratch(
    &self,
    scratch: &std::path::Path,
    protection: VerifierProtection,
    timeout_ms: u64,
  ) -> ExecutedVerifier {
    let verifier = VerifierSpec {
      id: "V1".into(),
      command: CommandSpec {
        argv: vec![CommandArgument::CandidatePath("verify.sh".into())],
        cwd: CommandCwd::Candidate(".".into()),
        env: EnvironmentSpec::default(),
        timeout_ms,
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
    let (mut digests, mut directories) = (Vec::new(), Vec::new());
    walk_view(
      self.candidate.path(),
      "candidate",
      &mut digests,
      &mut directories,
    );
    walk_view(
      self.authority.path(),
      "authority",
      &mut digests,
      &mut directories,
    );
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
        view_digests: &digests,
        view_directories: &directories,
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
  assert_eq!(
    executed.result,
    EvidenceResult::Pass,
    "protected run did not pass: {:?} {}",
    executed.observation,
    executed.infrastructure_error.as_deref().unwrap_or_default()
  );
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
    let executed = sandbox.run_with_scratch(fresh.path(), VerifierProtection::Protected, 5_000);
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

/// Adversarial regression for the external race: while a protected verifier
/// runs, a hostile same-user producer on the host flips the original backing
/// Candidate between R and R' continuously. The verifier hashes the protected
/// view thirty times across its run and exits 0 only if every observation
/// equals the original admitted bytes, so a `Pass` proves the private view
/// isolated the verifier from the concurrent host mutation; a `Fail` would
/// prove the race leaked into the verifier's observations.
///
/// The Bubblewrap backend is where the runner itself builds the private view
/// from host-side roots, so the race is exercised against exactly that
/// boundary. On macOS the immutable view is staged by the workspace
/// (read-only volume), never by the runner, and the composition is covered
/// by the `tenet-cli` hostile tests; that backend mismatch is the explicit
/// skip reason below.
#[test]
fn external_mutator_cannot_race_the_protected_view() {
  if !cfg!(target_os = "linux") {
    eprintln!(
      "SKIP: the runner creates the private view only for the Bubblewrap backend; \
       the macOS boundary is workspace-staged and covered by the tenet-cli hostile tests"
    );
    return;
  }
  if !protection_backend_available() {
    eprintln!("SKIP: no protected-verification backend on this platform");
    return;
  }
  let expected = sha256_hex(b"original candidate");
  let sandbox = Sandbox::new(&format!(
    ": > \"$TENET_SCRATCH_ROOT/started\" || exit 3
i=0
while [ $i -lt 30 ]; do
  i=$((i+1))
  observed=$(sha256sum \"$TENET_CANDIDATE_ROOT/candidate.txt\" | cut -d' ' -f1)
  [ \"$observed\" = \"{expected}\" ] || exit 1
  sleep 0.05
done"
  ));
  let stop = Arc::new(AtomicBool::new(false));
  let flips = Arc::new(AtomicUsize::new(0));
  let hostile_stop = Arc::clone(&stop);
  let hostile_flips = Arc::clone(&flips);
  let marker = sandbox.scratch.path().join("started");
  let candidate = sandbox.candidate.path().join("candidate.txt");
  let hostile = thread::spawn(move || {
    // Explicit barrier: mutate only after the verifier exec'd, which
    // happens only after the private namespace view was built and every
    // byte verified against the trusted digests.
    let started = Instant::now();
    while !marker.is_file() {
      if hostile_stop.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(30) {
        return;
      }
      thread::sleep(Duration::from_millis(5));
    }
    let mut mutated = false;
    while !hostile_stop.load(Ordering::Relaxed) {
      mutated = !mutated;
      let _ = fs::write(
        &candidate,
        if mutated {
          "MUTATED BY EXTERNAL PRODUCER"
        } else {
          "original candidate"
        },
      );
      hostile_flips.fetch_add(1, Ordering::Relaxed);
      thread::sleep(Duration::from_millis(1));
    }
  });
  let executed = sandbox.run_with_scratch(
    sandbox.scratch.path(),
    VerifierProtection::Protected,
    30_000,
  );
  stop.store(true, Ordering::Relaxed);
  hostile.join().expect("join hostile mutator");
  assert!(
    flips.load(Ordering::Relaxed) >= 50,
    "the external mutator never raced the verifier; the test proves nothing"
  );
  assert_eq!(
    executed.result,
    EvidenceResult::Pass,
    "the protected verifier observed mutated host bytes; the private view \
     did not isolate it from the external race"
  );
  assert_backend_enforced(&executed);
}
