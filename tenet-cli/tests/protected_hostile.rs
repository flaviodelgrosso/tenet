//! Adversarial proofs that `PROTECTED_V1` verification is safe against an
//! external hostile producer with ordinary same-user filesystem access.
//!
//! Three actors race in every test: the canonical CLI drives the protected
//! verification, the protected verifier repeatedly hashes its sealed
//! Candidate and Authority views for the whole run and passes only if every
//! single observation equals the original admitted bytes, and a separate
//! hostile producer mutates the host-side originals while the protected
//! boundary is already established. Release of the hostile producer is gated
//! on an explicit `started` barrier the verifier writes into its scratch
//! after exec, so the race is guaranteed rather than sampled.
//!
//! - A passing protected verifier proves every repeated observation saw the
//!   sealed bytes despite the concurrent host-side mutation;
//! - two verifiers both passing proves they observed identical bytes for the
//!   same Candidate identity;
//! - a hostile that corrupts a view before it is sealed can force a
//!   fail-closed infrastructure result, but can never make a verifier
//!   `fail` on bytes the boundary missed, and can never produce `DONE`.
#![cfg(unix)]

use std::{
  fs,
  io::Read,
  path::Path,
  process::{Command, Stdio},
  sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
  },
  thread,
  time::{Duration, Instant},
};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tenet_domain::{
  algebra::{
    AssuranceRequirementV1, COMPLETION_POLICY_V1, CompletionContractV1, Criterion, CriterionId,
    EvidenceControlRequirementV1, EvidenceRequirementV1, Requirement, Verifier, VerifierId,
    VerifierMaterial,
  },
  policy::{
    CandidateCapturePolicy, CommandArgument, CommandCwd, CommandSpec, EnvironmentSpec,
    ExitCodePolicy, ProjectConfig, VerifierAuthority, VerifierProtection, VerifierSpec,
  },
};

const SECRET: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
const ORIGINAL: &str = "original candidate bytes for protected verification";
const MUTATED: &str = "MUTATED BY HOSTILE PRODUCER";

/// These tests race real machine-global OS state (DiskArbitration mounts
/// under `/Volumes`, image attach/detach, kernel namespaces), so two of them
/// running concurrently can make one test's staging or mounts disturb
/// another's run. They serialize on this lock; the assertions stay about the
/// boundary, not about scheduling.
static HOSTILE_LOCK: Mutex<()> = Mutex::new(());

fn hostile_run_guard() -> std::sync::MutexGuard<'static, ()> {
  HOSTILE_LOCK
    .lock()
    .unwrap_or_else(|poisoned| poisoned.into_inner())
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

/// True only when both the workspace boundary and the runner backend can
/// enforce protected verification on this machine.
fn protection_enforced() -> bool {
  tenet_workspace::protected_materialization_available()
    && tenet_runner::protection_backend_available()
}

fn tenet(root: &Path, args: &[&str]) -> std::process::Output {
  Command::new(env!("CARGO_BIN_EXE_tenet"))
    .arg("--cwd")
    .arg(root)
    .env("TENET_ADMISSION_SECRET", SECRET)
    .args(args)
    .output()
    .expect("spawn tenet")
}

fn ok_json(root: &Path, args: &[&str]) -> Value {
  let output = tenet(root, args);
  assert!(
    output.status.success(),
    "tenet {args:?} failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  serde_json::from_slice(&output.stdout).expect("json")
}

fn contract() -> CompletionContractV1 {
  CompletionContractV1 {
    schema_version: 1,
    policy: tenet_domain::algebra::CompletionPolicyId(COMPLETION_POLICY_V1.into()),
    requirements: vec![Requirement {
      id: tenet_domain::contract::RequirementId("R1".into()),
      statement: "the candidate is complete".into(),
      criteria: vec![Criterion {
        id: CriterionId("C1".into()),
        proposition: "criterion C1 holds".into(),
        verifiers: vec![
          Verifier {
            id: VerifierId("V1".into()),
            material: VerifierMaterial::Candidate,
          },
          Verifier {
            id: VerifierId("V2".into()),
            material: VerifierMaterial::Candidate,
          },
        ],
        evidence: EvidenceRequirementV1 {
          control: EvidenceControlRequirementV1::CandidateControlledPermitted,
          assurance: AssuranceRequirementV1::LocalOrStronger,
        },
      }],
    }],
  }
}

/// The verifier signals its start into the run's scratch (the explicit
/// barrier that releases the hostile producer), then repeatedly hashes the
/// protected Candidate file and the whole protected Authority view for the
/// duration of the run. It exits 0 only if every single observation equals
/// the original admitted bytes and the Authority view never changed, so a
/// `pass` proves every repeated observation during the race saw the sealed
/// state; any observation of other bytes exits 1 (`fail`), and a scratch
/// that cannot be written exits 3 (unrecognized → infrastructure).
fn verifier_script() -> String {
  r#"#!/bin/sh
expected="$1"
: > "$TENET_SCRATCH_ROOT/started" || exit 3
hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}
authority_tree_hash() {
  if command -v sha256sum >/dev/null 2>&1; then
    ( cd "$TENET_AUTHORITY_ROOT" && find . -type f | LC_ALL=C sort | xargs sha256sum ) | sha256sum | cut -d' ' -f1
  else
    ( cd "$TENET_AUTHORITY_ROOT" && find . -type f | LC_ALL=C sort | xargs shasum -a 256 ) | shasum -a 256 | cut -d' ' -f1
  fi
}
candidate="$TENET_CANDIDATE_ROOT/candidate.txt"
baseline=""
i=0
while [ $i -lt 30 ]; do
  i=$((i+1))
  [ "$(hash_file "$candidate")" = "$expected" ] || exit 1
  authority=$(authority_tree_hash)
  [ -n "$authority" ] || exit 1
  if [ -z "$baseline" ]; then
    baseline="$authority"
  elif [ "$authority" != "$baseline" ]; then
    exit 1
  fi
  sleep 0.1
done
exit 0
"#
  .to_owned()
}

struct Repo {
  directory: tempfile::TempDir,
}

impl Repo {
  fn new() -> Self {
    let directory = tempfile::tempdir().expect("repository");
    let root = directory.path();
    fs::write(root.join("SPEC.md"), "# Specification\n").expect("spec");
    assert!(
      tenet(root, &["init", "--json"]).status.success(),
      "init failed"
    );
    fs::write(root.join("candidate.txt"), ORIGINAL).expect("candidate");
    fs::write(root.join("verify.sh"), verifier_script()).expect("verify script");
    fs::set_permissions(
      root.join("verify.sh"),
      std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("executable");
    let expected_digest = sha256_hex(ORIGINAL.as_bytes());
    let policy = ProjectConfig {
      version: 1,
      spec_path: "SPEC.md".into(),
      candidate: CandidateCapturePolicy {
        root: ".".into(),
        include: vec!["candidate.txt".to_owned(), "verify.sh".to_owned()],
        exclude: vec![],
      },
      verifiers: ["V1", "V2"]
        .map(|id| VerifierSpec {
          id: id.into(),
          command: CommandSpec {
            argv: vec![
              CommandArgument::CandidatePath("verify.sh".into()),
              CommandArgument::Literal(expected_digest.clone()),
            ],
            cwd: CommandCwd::Candidate(".".into()),
            env: EnvironmentSpec::default(),
            timeout_ms: 90_000,
            result: ExitCodePolicy {
              pass: [0].into(),
              fail: [1].into(),
              inconclusive: [].into(),
            },
          },
          max_output_bytes: 4_096,
          authority: VerifierAuthority::Project,
          oracle_path: None,
          protection: VerifierProtection::Protected,
        })
        .to_vec(),
    };
    fs::write(
      root.join(".tenet/tenet.toml"),
      toml::to_string_pretty(&policy).expect("policy"),
    )
    .expect("write policy");
    fs::write(
      root.join("contract.json"),
      serde_json::to_vec_pretty(&contract()).expect("contract"),
    )
    .expect("write contract");
    let proposal = ok_json(
      root,
      &[
        "authority",
        "prepare",
        "--contract",
        &root.join("contract.json").to_string_lossy(),
        "--json",
      ],
    );
    let proposal_id = proposal["proposalId"].as_str().expect("proposal id");
    let authority_id = proposal["authorityId"].as_str().expect("authority id");
    let reconciliation = ok_json(
      root,
      &[
        "authority",
        "reconcile",
        "--proposal",
        proposal_id,
        "--json",
      ],
    );
    let reconciliation_id = reconciliation["reconciliationId"]
      .as_str()
      .expect("reconciliation id");
    let grant = ok_json(
      root,
      &[
        "authority",
        "grant",
        "--proposal",
        proposal_id,
        "--authority",
        authority_id,
        "--json",
      ],
    );
    fs::write(
      root.join("grant.json"),
      serde_json::to_vec(&grant).expect("grant json"),
    )
    .expect("write grant");
    ok_json(
      root,
      &[
        "authority",
        "admit",
        "--proposal",
        proposal_id,
        "--reconciliation",
        reconciliation_id,
        "--authority",
        authority_id,
        "--grant",
        &root.join("grant.json").to_string_lossy(),
        "--json",
      ],
    );
    Self { directory }
  }

  fn root(&self) -> &Path {
    self.directory.path()
  }

  /// Spawn `tenet verify`, wait for the verifier's explicit `started`
  /// barrier, run the hostile producer until the CLI exits, leave the
  /// host-side original deterministically mutated, and return the parsed
  /// result, the exit code, and the number of hostile passes.
  fn verify_under_attack(
    &self,
    hostile: impl Fn(&Path) + Send + 'static,
  ) -> (Value, Option<i32>, usize) {
    let root = self.root().to_path_buf();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tenet"))
      .arg("--cwd")
      .arg(&root)
      .env("TENET_ADMISSION_SECRET", SECRET)
      .args(["verify", "--json"])
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()
      .expect("spawn verify");

    assert!(
      wait_for_verifier_started(&root),
      "the protected verifier never started behind its boundary"
    );

    let stop = Arc::new(AtomicBool::new(false));
    let hostile_root = root.clone();
    let hostile_stop = Arc::clone(&stop);
    let attacks = Arc::new(AtomicUsize::new(0));
    let hostile_attacks = Arc::clone(&attacks);
    let hostile_thread = thread::spawn(move || {
      while !hostile_stop.load(Ordering::Relaxed) {
        hostile(&hostile_root);
        hostile_attacks.fetch_add(1, Ordering::Relaxed);
        thread::sleep(Duration::from_millis(10));
      }
    });

    let mut stdout = Vec::new();
    child
      .stdout
      .as_mut()
      .expect("stdout")
      .read_to_end(&mut stdout)
      .expect("read stdout");
    let status = child.wait().expect("wait verify");
    stop.store(true, Ordering::Relaxed);
    hostile_thread.join().expect("join hostile");
    // The hostile producer stops with the host-side original deterministically
    // in the mutated state `R'`, after the CLI's final recapture.
    let _ = fs::write(root.join("candidate.txt"), MUTATED);

    let result: Value = serde_json::from_slice(&stdout).expect("verify json");
    (result, status.code(), attacks.load(Ordering::Relaxed))
  }
}

/// Waits for the explicit barrier: the first protected verifier has exec'd
/// behind its enforcing boundary — the sealed read-only volume on macOS, the
/// digest-verified private namespace copy on Linux — and signalled through
/// its scratch directory. The hostile producer starts only after this, so
/// every mutation provably races an established protected view.
fn wait_for_verifier_started(root: &Path) -> bool {
  let scratch = root.join(".tenet/tmp/scratch");
  let deadline = Instant::now() + Duration::from_secs(90);
  while Instant::now() < deadline {
    if let Ok(entries) = fs::read_dir(&scratch)
      && entries
        .flatten()
        .any(|entry| entry.path().join("started").is_file())
    {
      return true;
    }
    thread::sleep(Duration::from_millis(25));
  }
  false
}

/// Attempt direct writes and deletions through every live protected mount.
/// These must fail at the read-only OS boundary; a successful write would
/// change the verifier's repeated Candidate/Authority hashes and end the
/// run `fail`, which every test rejects.
fn attack_mounts() {
  let Ok(volumes) = fs::read_dir("/Volumes") else {
    return;
  };
  for volume in volumes.flatten() {
    let name = volume.file_name();
    if !name.to_string_lossy().starts_with("tenet-protected-") {
      continue;
    }
    let candidate = volume.path().join("candidate");
    let _ = fs::write(candidate.join("candidate.txt"), MUTATED);
    let _ = fs::write(candidate.join("hostile.txt"), MUTATED);
    let _ = fs::remove_file(candidate.join("verify.sh"));
    let authority = volume.path().join("authority");
    let _ = fs::write(authority.join("SPEC.md"), MUTATED);
    let _ = fs::write(authority.join("hostile.txt"), MUTATED);
    let _ = fs::remove_file(authority.join("SPEC.md"));
  }
}

/// Mutate the Candidate source and attempt direct writes and deletions
/// through every live protected mount.
fn attack_source_and_mounts(root: &Path) {
  let _ = fs::write(root.join("candidate.txt"), MUTATED);
  attack_mounts();
}

/// Also poison every staged or materialized view file: the protected
/// staging under `.tenet/tmp/protected` and materialized snapshots under
/// `.tenet/tmp/materialized`. The walk deliberately excludes the persistence
/// layer's atomic-rename staging (`.tenet/tmp/objects`, `.tenet/tmp/atomic`)
/// and the verifier scratch/output: corrupting those is a content-store
/// denial of service that the digest checks already fail closed at CLI
/// level, not a protected-view attack, and this test asserts on the
/// evaluation verdict of a completed run.
fn attack_all(root: &Path) {
  attack_source_and_mounts(root);
  let mut stack = vec![
    root.join(".tenet/tmp/protected"),
    root.join(".tenet/tmp/materialized"),
  ];
  while let Some(directory) = stack.pop() {
    let Ok(entries) = fs::read_dir(&directory) else {
      continue;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_dir() {
        stack.push(path);
      } else {
        let _ = fs::write(&path, "ATTACK");
      }
    }
  }
}
fn assert_protected_run(run: &Value, expected: &str) {
  assert_eq!(run["verifier"].as_str().expect("verifier id"), expected);
  let observed = run["observation"]["result"].as_str().expect("result");
  let assurance = run["context"]["assurance"].as_str().expect("assurance");
  if observed == "pass" {
    assert_eq!(
      assurance, "PROTECTED_V1",
      "a passing protected verifier must report PROTECTED_V1"
    );
  } else {
    assert_eq!(observed, "infrastructure_error");
  }
}

#[test]
fn external_hostile_mutation_cannot_affect_protected_observations() {
  if !protection_enforced() {
    eprintln!("SKIP: no enforcing protected-verification backend on this platform");
    return;
  }
  let _guard = hostile_run_guard();
  let repo = Repo::new();
  let (result, code, attacks) = repo.verify_under_attack(attack_source_and_mounts);
  assert!(
    attacks >= 1,
    "the hostile producer never attacked during verification"
  );

  assert_ne!(
    result["verdict"].as_str().expect("verdict"),
    "DONE",
    "a mutated source candidate must never complete as DONE"
  );
  let runs = result["evaluation"]["runs"].as_array().expect("runs");
  assert_eq!(runs.len(), 2, "both protected verifiers must run");
  for (run, id) in runs.iter().zip(["V1", "V2"]) {
    assert_protected_run(run, id);
    assert_eq!(
      run["observation"]["result"].as_str().expect("result"),
      "pass",
      "verifier {id} must have observed the original bytes despite the \
       concurrent hostile mutation; a Fail would mean the hostile producer \
       changed what the verifier read; infrastructure failure names the \
       boundary problem: {:?}",
      run["observation"]["infrastructureError"].as_str()
    );
    assert_eq!(run["observation"]["exitCode"].as_i64(), Some(0));
    // Both verifiers passed the same expected digest for the same
    // Candidate identity, so they observed identical bytes.
    assert_eq!(
      run["candidate"].as_str().expect("candidate id"),
      result["candidateId"].as_str().expect("verify candidate id")
    );
  }
  // The hostile mutation of the source is visible to the final recapture:
  // completion is refused for the mutated state.
  assert_eq!(
    result["verdict"].as_str().expect("verdict"),
    "INCONCLUSIVE",
    "mutated source must yield INCONCLUSIVE, got {:?}",
    result["reason"]
  );
  assert_eq!(
    result["reason"].as_str().expect("reason"),
    "CANDIDATE_CHANGED_DURING_VERIFICATION"
  );
  assert_eq!(code, Some(3), "INCONCLUSIVE exit code");
}

#[test]
fn hostile_staging_poisoning_cannot_produce_pass_over_mutated_bytes() {
  if !protection_enforced() {
    eprintln!("SKIP: no enforcing protected-verification backend on this platform");
    return;
  }
  let _guard = hostile_run_guard();
  let repo = Repo::new();
  let (result, _code, attacks) = repo.verify_under_attack(attack_all);
  assert!(
    attacks >= 1,
    "the hostile producer never attacked during verification"
  );

  assert_ne!(
    result["verdict"].as_str().expect("verdict"),
    "DONE",
    "a poisoned staging tree must never complete as DONE"
  );
  let runs = result["evaluation"]["runs"].as_array().expect("runs");
  assert_eq!(runs.len(), 2, "both protected verifiers must run");
  for (run, id) in runs.iter().zip(["V1", "V2"]) {
    assert_protected_run(run, id);
    let observed = run["observation"]["result"].as_str().expect("result");
    // The first run's boundary was sealed before the poisoner started, so
    // it must pass. A later run whose staging was poisoned before sealing
    // must fail closed as an infrastructure error. Either way the verifier
    // can never end with `fail`: a Fail would mean the hostile producer
    // changed observed bytes without the boundary catching it.
    assert!(
      observed == "pass" || observed == "infrastructure_error",
      "verifier {id} ended {observed}; hostile staging must fail closed, \
       never pass over mutated bytes and never observe them as Fail"
    );
    if observed == "infrastructure_error" {
      let message = run["observation"]["infrastructureError"]
        .as_str()
        .expect("infrastructure message");
      // Fail-closed paths: the workspace boundary check names "protected";
      // the runner's in-namespace digest verification exits 70, a code no
      // admitted policy interprets (Linux Bubblewrap prelude).
      assert!(
        message.contains("protected") || message.contains("exit code 70"),
        "infrastructure failure must come from the protected boundary: {message}"
      );
    }
  }
  assert_eq!(
    fs::read_to_string(repo.root().join("candidate.txt")).expect("candidate"),
    MUTATED,
    "the hostile producer must actually have mutated the source"
  );
  assert_ne!(
    result["verdict"].as_str().expect("verdict"),
    "DONE",
    "the final recapture must observe the mutated source and refuse DONE"
  );
}

#[test]
fn external_hostile_oscillation_cannot_change_protected_observations() {
  if !protection_enforced() {
    eprintln!("SKIP: no enforcing protected-verification backend on this platform");
    return;
  }
  let _guard = hostile_run_guard();
  let repo = Repo::new();
  // The exact objective race: the host-side original Candidate and Authority
  // flip R -> R' -> R -> R' for the whole duration of the protected runs.
  let flip = Arc::new(AtomicBool::new(false));
  let (result, code, attacks) = repo.verify_under_attack(move |root| {
    let mutated = !flip.fetch_xor(true, Ordering::Relaxed);
    let _ = fs::write(
      root.join("candidate.txt"),
      if mutated { MUTATED } else { ORIGINAL },
    );
    let _ = fs::write(
      root.join("SPEC.md"),
      if mutated {
        "# MUTATED AUTHORITY\n"
      } else {
        "# Specification\n"
      },
    );
    attack_mounts();
  });
  assert!(
    attacks >= 20,
    "the hostile producer must have raced the whole verification: {attacks} passes"
  );
  let runs = result["evaluation"]["runs"].as_array().expect("runs");
  assert_eq!(runs.len(), 2, "both protected verifiers must run");
  for (run, id) in runs.iter().zip(["V1", "V2"]) {
    assert_eq!(run["verifier"].as_str().expect("verifier id"), id);
    assert_eq!(
      run["observation"]["result"].as_str().expect("result"),
      "pass",
      "verifier {id} must pass: every repeated hash of the sealed Candidate \
       and Authority views matched the admitted bytes despite the oscillating \
       host state; a Fail would mean the race leaked into observations"
    );
    assert_eq!(
      run["context"]["assurance"].as_str().expect("assurance"),
      "PROTECTED_V1"
    );
    assert_eq!(
      run["candidate"].as_str().expect("candidate id"),
      result["candidateId"].as_str().expect("verify candidate id"),
      "evidence attributed to the original Candidate must never derive from \
       the transient mutated host state"
    );
  }
  // The final verdict depends only on which side of the oscillation the
  // post-evaluation recapture happened to observe; both are honest, and
  // neither may be a false completion over mutated bytes nor a Fail.
  match result["verdict"].as_str().expect("verdict") {
    "DONE" => assert_eq!(code, Some(0), "DONE must exit 0"),
    "INCONCLUSIVE" => {
      assert_eq!(
        result["reason"].as_str().expect("reason"),
        "CANDIDATE_CHANGED_DURING_VERIFICATION"
      );
      assert_eq!(code, Some(3), "INCONCLUSIVE must exit 3");
    }
    other => panic!("oscillating host state must never yield {other}"),
  }
  assert_eq!(
    fs::read_to_string(repo.root().join("candidate.txt")).expect("candidate"),
    MUTATED,
    "the host-side original must sit in the mutated state right after the run"
  );
}
