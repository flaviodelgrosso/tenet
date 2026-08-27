#![cfg(target_os = "macos")]

use std::{path::Path, process::Command};

struct CredentialCleanup(Option<String>);

impl Drop for CredentialCleanup {
  fn drop(&mut self) {
    if let Some(authority_id) = self.0.take() {
      let _ = Command::new("security")
        .args([
          "delete-generic-password",
          "-s",
          "dev.tenet.controller-authority",
          "-a",
          &authority_id,
        ])
        .status();
    }
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

#[test]
#[ignore = "requires an unlocked native OS credential store"]
fn os_keyring_init_then_verify_resolves_authority_without_launcher_inputs() {
  let repository = tempfile::tempdir().expect("temporary repository");
  let init = Command::new(env!("CARGO_BIN_EXE_tenet"))
    .args(["init", "--cwd"])
    .arg(repository.path())
    .env_remove("TENET_CONTROLLER_AUTHORITY_NAMESPACE")
    .env_remove("TENET_CONTROLLER_AUTHORITY_KEY_FD")
    .output()
    .expect("run tenet init");
  assert!(
    init.status.success(),
    "{}",
    String::from_utf8_lossy(&init.stderr)
  );

  let metadata: serde_json::Value = serde_json::from_slice(
    &std::fs::read(repository.path().join(".tenet/authority.json"))
      .expect("read authority metadata"),
  )
  .expect("parse authority metadata");
  let authority_id = metadata["authority_id"]
    .as_str()
    .expect("authority identity")
    .to_owned();
  let mut cleanup = CredentialCleanup(Some(authority_id.clone()));

  run_git(repository.path(), &["init", "-q"]);
  run_git(repository.path(), &["config", "user.name", "Tenet Test"]);
  run_git(
    repository.path(),
    &["config", "user.email", "tenet-test@localhost"],
  );
  std::fs::write(repository.path().join("README.txt"), "tracked\n").expect("write tracked file");
  std::fs::write(
    repository.path().join("tenet.toml"),
    "version = 1\nspec_file = \"spec.md\"\nmax_cycles = 25\nmax_repair_attempts = 3\n[agent]\nid = \"test-agent\"\n[verification]\n[[verification.checks]]\nname = \"tracked file\"\ncommand = [\"sh\", \"-c\", \"test -f README.txt\"]\n",
  )
  .expect("write configuration");
  run_git(
    repository.path(),
    &["add", ".gitignore", "README.txt", "spec.md", "tenet.toml"],
  );
  run_git(repository.path(), &["commit", "-q", "-m", "configure"]);

  let verify = Command::new(env!("CARGO_BIN_EXE_tenet"))
    .args(["verify", "--cwd"])
    .arg(repository.path())
    .env_remove("TENET_CONTROLLER_AUTHORITY_NAMESPACE")
    .env_remove("TENET_CONTROLLER_AUTHORITY_KEY_FD")
    .output()
    .expect("run tenet verify");
  assert!(
    verify.status.success(),
    "{}",
    String::from_utf8_lossy(&verify.stderr)
  );
  assert!(
    String::from_utf8_lossy(&verify.stdout).contains("verification: PASS"),
    "{}",
    String::from_utf8_lossy(&verify.stdout)
  );

  let removed = Command::new("security")
    .args([
      "delete-generic-password",
      "-s",
      "dev.tenet.controller-authority",
      "-a",
      &authority_id,
    ])
    .status()
    .expect("remove native test credential");
  assert!(removed.success());
  cleanup.0 = None;
}
