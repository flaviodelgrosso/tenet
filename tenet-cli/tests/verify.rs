use std::{
  path::{Path, PathBuf},
  process::{Command, Stdio},
  sync::atomic::{AtomicU64, Ordering},
  time::{SystemTime, UNIX_EPOCH},
};

static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);

struct TempRepo(PathBuf);

impl TempRepo {
  fn new(config: &str) -> Self {
    let nonce = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .expect("system clock")
      .as_nanos();
    let sequence = NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
      "tenet-cli-verify-{}-{nonce}-{sequence}",
      std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create repository");
    let init = authority_command(&path, &["init"])
      .output()
      .expect("initialize controller authority");
    assert!(
      init.status.success(),
      "{}",
      String::from_utf8_lossy(&init.stderr)
    );
    run_git(&path, &["init", "-q"]);
    run_git(&path, &["config", "user.name", "Tenet Test"]);
    run_git(&path, &["config", "user.email", "tenet-test@localhost"]);
    std::fs::write(path.join("README.txt"), "tracked\n").expect("write tracked file");
    std::fs::write(path.join("tenet.toml"), config).expect("write config");
    run_git(
      &path,
      &["add", ".gitignore", "README.txt", "spec.md", "tenet.toml"],
    );
    run_git(&path, &["commit", "-q", "-m", "configure"]);
    Self(path)
  }

  fn path(&self) -> &Path {
    &self.0
  }
}

impl Drop for TempRepo {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn run_git(cwd: &Path, args: &[&str]) {
  let output = Command::new("git")
    .args(args)
    .current_dir(cwd)
    .output()
    .expect("run git");
  assert!(
    output.status.success(),
    "git {} failed: {}",
    args.join(" "),
    String::from_utf8_lossy(&output.stderr)
  );
}

fn base_config(verification: &str) -> String {
  format!(
    "version = 1\nspec_file = \"spec.md\"\nmax_cycles = 25\nmax_repair_attempts = 3\n[agent]\nid = \"test-agent\"\n{verification}"
  )
}

fn authority_command_with(
  repository: &Path,
  args: &[&str],
  namespace: &str,
  secret: &[u8],
) -> Command {
  let key_path = repository.with_extension("controller-authority-key");
  std::fs::write(&key_path, secret).expect("write controller authority fixture");
  let key = std::fs::File::open(&key_path).expect("open controller authority fixture");
  std::fs::remove_file(key_path).expect("unlink controller authority fixture");
  let mut command = Command::new(env!("CARGO_BIN_EXE_tenet"));
  command
    .env("TENET_CONTROLLER_AUTHORITY_NAMESPACE", namespace)
    .env("TENET_CONTROLLER_AUTHORITY_KEY_FD", "0")
    .stdin(Stdio::from(key))
    .args(args)
    .args(["--cwd"])
    .arg(repository);
  command
}

fn authority_command(repository: &Path, args: &[&str]) -> Command {
  authority_command_with(
    repository,
    args,
    "tenet-cli-verify-tests",
    b"abcdef0123456789abcdef0123456789",
  )
}

fn tenet_verify(repository: &TempRepo) -> Command {
  authority_command(repository.path(), &["verify"])
}

#[test]
fn verify_reports_each_configured_project_check_without_catalog() {
  let repository = TempRepo::new(&base_config(
    "[verification]\n[[verification.checks]]\nname = \"tracked file\"\ncommand = [\"sh\", \"-c\", \"test -f README.txt\"]\n",
  ));

  let output = tenet_verify(&repository)
    .output()
    .expect("run tenet verify");
  let stdout = String::from_utf8_lossy(&output.stdout);

  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(stdout.contains("[PASS] tracked file:"), "{stdout}");
  assert!(stdout.contains("verification: PASS"), "{stdout}");
  assert!(!repository.path().join(".tenet/requirements.json").exists());
}

#[test]
fn verify_fails_clearly_without_project_checks() {
  let repository = TempRepo::new(&base_config(""));

  let output = tenet_verify(&repository)
    .output()
    .expect("run tenet verify");
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(!output.status.success());
  assert!(
    stderr.contains("No trusted project verification checks are configured"),
    "{stderr}"
  );
  assert!(stderr.contains("[[verification.checks]]"), "{stderr}");
}

#[test]
fn verify_explains_when_an_inherited_authority_credential_is_not_supplied() {
  let repository = TempRepo::new(&base_config(
    "[verification]\n[[verification.checks]]\nname = \"tracked file\"\ncommand = [\"sh\", \"-c\", \"test -f README.txt\"]\n",
  ));

  let output = Command::new(env!("CARGO_BIN_EXE_tenet"))
    .args(["verify", "--cwd"])
    .arg(repository.path())
    .output()
    .expect("run tenet verify without inherited credential");
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(!output.status.success());
  assert!(
    stderr.contains("uses the advanced inherited-FD authority provider"),
    "{stderr}"
  );
}

#[test]
fn verify_rejects_a_credential_from_another_repository_identity() {
  let repository = TempRepo::new(&base_config(
    "[verification]\n[[verification.checks]]\nname = \"tracked file\"\ncommand = [\"sh\", \"-c\", \"test -f README.txt\"]\n",
  ));

  let output = authority_command_with(
    repository.path(),
    &["verify"],
    "another-repository-authority",
    b"abcdef0123456789abcdef0123456789",
  )
  .output()
  .expect("run tenet verify with mismatched authority");
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(!output.status.success());
  assert!(
    stderr.contains("does not match this repository"),
    "{stderr}"
  );
}
