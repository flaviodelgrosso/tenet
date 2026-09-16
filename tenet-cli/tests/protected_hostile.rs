//! Adversarial proofs that `PROTECTED_V1` verification is safe against an
//! external hostile producer with ordinary same-user filesystem access.
//!
//! A separate hostile process continuously mutates the Candidate source,
//! attempts to write through the live protected mount, and (in the staging
//! test) poisons every staged view file under `.tenet/tmp` while the real
//! `LocalWorkspace` + `LocalProcessRunner` protected verification runs
//! through the canonical CLI. Each protected verifier passes only if the
//! sha256 of the file it reads equals the digest of the original captured
//! bytes, so:
//!
//! - a `pass` observation proves the verifier read exactly the original
//!   bytes despite the concurrent mutation;
//! - two verifiers both passing proves they observed identical bytes for
//!   the same Candidate identity;
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
    Arc,
    atomic::{AtomicBool, Ordering},
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

/// The verifier reads its candidate file after a window wide enough for the
/// hostile producer to be active and passes only on the expected digest.
fn verifier_script() -> String {
  "#!/bin/sh
expected=\"$1\"
i=0
while [ \"$i\" -lt 20 ]; do i=$((i+1)); sleep 0.1; done
f=\"$TENET_CANDIDATE_ROOT/candidate.txt\"
if command -v sha256sum >/dev/null 2>&1; then
  observed=$(sha256sum \"$f\" | cut -d' ' -f1)
else
  observed=$(shasum -a 256 \"$f\" | cut -d' ' -f1)
fi
[ \"$observed\" = \"$expected\" ] && exit 0
exit 1
"
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

  /// Spawn `tenet verify`, wait for the first protected boundary to become
  /// observable, run the hostile producer until the CLI exits, and return
  /// the parsed result plus exit code.
  fn verify_under_attack(&self, hostile: impl Fn(&Path) + Send + 'static) -> (Value, Option<i32>) {
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
      wait_for_boundary(&root),
      "protected boundary never became observable"
    );

    let stop = Arc::new(AtomicBool::new(false));
    let hostile_root = root.clone();
    let hostile_stop = Arc::clone(&stop);
    let attacks = Arc::new(AtomicBool::new(false));
    let hostile_attacks = Arc::clone(&attacks);
    let hostile_thread = thread::spawn(move || {
      while !hostile_stop.load(Ordering::Relaxed) {
        hostile(&hostile_root);
        hostile_attacks.store(true, Ordering::Relaxed);
        thread::sleep(Duration::from_millis(20));
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
    assert!(
      attacks.load(Ordering::Relaxed),
      "hostile producer never completed a pass"
    );

    let result: Value = serde_json::from_slice(&stdout).expect("verify json");
    (result, status.code())
  }
}

/// Waits until the enforcing boundary for the first protected run exists:
/// the backing image is unlinked and the read-only volume is mounted
/// (macOS), or the staging directory is materialized and the prelude has
/// had time to build and verify the private namespace copy (Linux).
fn wait_for_boundary(root: &Path) -> bool {
  let protected = root.join(".tenet/tmp/protected");
  let deadline = Instant::now() + Duration::from_secs(90);
  #[cfg(target_os = "macos")]
  {
    while Instant::now() < deadline {
      let mounted = fs::read_dir("/Volumes")
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
          entry
            .file_name()
            .to_string_lossy()
            .starts_with("tenet-protected-")
        });
      // The image is unlinked immediately after a successful attach, so
      // "mounted and no image left" means the bytes are sealed.
      let image_gone = fs::read_dir(&protected)
        .map(|entries| {
          entries
            .into_iter()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".dmg"))
        })
        .unwrap_or(!mounted);
      if mounted && image_gone {
        return true;
      }
      thread::sleep(Duration::from_millis(50));
    }
    false
  }
  #[cfg(not(target_os = "macos"))]
  {
    while Instant::now() < deadline {
      let staged = fs::read_dir(&protected)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| entry.path().join("candidate").is_dir());
      if staged {
        // The runner's prelude copies and verifies inside the namespace
        // immediately after staging; let it finish before attacking.
        thread::sleep(Duration::from_millis(2_000));
        return true;
      }
      thread::sleep(Duration::from_millis(50));
    }
    false
  }
}

/// Mutate the Candidate source and attempt direct writes and deletions
/// through every live protected mount.
fn attack_source_and_mounts(root: &Path) {
  let _ = fs::write(root.join("candidate.txt"), "MUTATED BY HOSTILE PRODUCER");
  let Ok(volumes) = fs::read_dir("/Volumes") else {
    return;
  };
  for volume in volumes.flatten() {
    let name = volume.file_name();
    if !name.to_string_lossy().starts_with("tenet-protected-") {
      continue;
    }
    let candidate = volume.path().join("candidate");
    let _ = fs::write(
      candidate.join("candidate.txt"),
      "MUTATED BY HOSTILE PRODUCER",
    );
    let _ = fs::write(candidate.join("hostile.txt"), "MUTATED BY HOSTILE PRODUCER");
    let _ = fs::remove_file(candidate.join("verify.sh"));
  }
}

/// Also poison every staged or materialized view file under `.tenet/tmp`.
fn attack_all(root: &Path) {
  attack_source_and_mounts(root);
  let temporary = root.join(".tenet/tmp");
  let mut stack = vec![temporary];
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
  let repo = Repo::new();
  let (result, code) = repo.verify_under_attack(attack_source_and_mounts);

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
       changed what the verifier read"
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
  let repo = Repo::new();
  let (result, _code) = repo.verify_under_attack(attack_all);

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
      assert!(
        message.contains("protected"),
        "infrastructure failure must name the protected boundary: {message}"
      );
    }
  }
  assert_eq!(
    fs::read_to_string(repo.root().join("candidate.txt")).expect("candidate"),
    "MUTATED BY HOSTILE PRODUCER",
    "the hostile producer must actually have mutated the source"
  );
}
