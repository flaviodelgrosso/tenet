use std::{
  path::{Path, PathBuf},
  process::{Command, Stdio},
  sync::atomic::{AtomicU64, Ordering},
  time::{SystemTime, UNIX_EPOCH},
};

static NEXT_PROJECT: AtomicU64 = AtomicU64::new(0);

struct TempProject(PathBuf);

impl TempProject {
  fn new() -> Self {
    let nonce = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .expect("system clock")
      .as_nanos();
    let sequence = NEXT_PROJECT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
      "tenet-cli-init-{}-{nonce}-{sequence}",
      std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create project");
    Self(path)
  }
}

impl Drop for TempProject {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn initialized_command(project: &Path) -> Command {
  let key_path = project.with_extension("controller-authority-key");
  std::fs::write(&key_path, b"0123456789abcdef0123456789abcdef")
    .expect("write controller authority fixture");
  let key = std::fs::File::open(&key_path).expect("open controller authority fixture");
  std::fs::remove_file(key_path).expect("unlink controller authority fixture");
  let mut command = Command::new(env!("CARGO_BIN_EXE_tenet"));
  command
    .env(
      "TENET_CONTROLLER_AUTHORITY_NAMESPACE",
      "tenet-cli-init-tests",
    )
    .env("TENET_CONTROLLER_AUTHORITY_KEY_FD", "0")
    .stdin(Stdio::from(key))
    .args(["init", "--cwd"])
    .arg(project);
  command
}

#[test]
fn init_writes_a_local_configuration_schema() {
  let project = TempProject::new();

  let output = initialized_command(&project.0)
    .output()
    .expect("run tenet init");

  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout
      .contains("Controller authority: tenet-cli-init-tests (inherited descriptor (advanced/CI))"),
    "{stdout}"
  );
  let config =
    std::fs::read_to_string(project.0.join("tenet.toml")).expect("read generated config");
  assert!(config.starts_with("#:schema ./.tenet/config.schema.json\n\n"));
  assert!(
    config.contains("[verification]\nchecks = []\ntimeout_secs = 300\nmax_output_bytes = 65536")
  );
  assert!(!config.contains("Configure at least one trusted project check"));
  assert!(!config.contains("tenet-cli-init-tests"));
  assert!(!config.contains("0123456789abcdef0123456789abcdef"));
  let schema: serde_json::Value = serde_json::from_str(
    &std::fs::read_to_string(project.0.join(".tenet/config.schema.json"))
      .expect("read generated schema"),
  )
  .expect("generated schema is JSON");
  assert_eq!(schema["$id"], "config.schema.json");
  assert_eq!(
    schema["$defs"]["verificationConfig"]["properties"]["checks"]["minItems"],
    1
  );
  let authority = std::fs::read_to_string(project.0.join(".tenet/authority.json"))
    .expect("read authority metadata");
  assert!(authority.contains("tenet-cli-init-tests"));
  assert!(!authority.contains("0123456789abcdef0123456789abcdef"));
  let database = std::fs::read(project.0.join(".tenet/tenet.db")).expect("read database");
  assert!(!database
    .windows(b"0123456789abcdef0123456789abcdef".len())
    .any(|window| window == b"0123456789abcdef0123456789abcdef"));
  let gitignore = std::fs::read_to_string(project.0.join(".gitignore")).expect("read gitignore");
  assert!(gitignore.lines().any(|line| line == ".tenet/"));
  let git_init = Command::new("git")
    .args(["init", "-q"])
    .current_dir(&project.0)
    .status()
    .expect("initialize Git repository");
  assert!(git_init.success());
  let ignored = Command::new("git")
    .args(["check-ignore", "-q", ".tenet/authority.json"])
    .current_dir(&project.0)
    .status()
    .expect("check authority metadata ignore rule");
  assert!(ignored.success());
}

#[test]
fn init_does_not_open_storage_when_existing_authority_is_unavailable() {
  let project = TempProject::new();
  let initialized = initialized_command(&project.0)
    .output()
    .expect("initialize inherited authority");
  assert!(initialized.status.success());
  let before = std::fs::read(project.0.join(".tenet/tenet.db")).expect("read database before");

  let output = Command::new(env!("CARGO_BIN_EXE_tenet"))
    .args(["init", "--cwd"])
    .arg(&project.0)
    .env_remove("TENET_CONTROLLER_AUTHORITY_NAMESPACE")
    .env_remove("TENET_CONTROLLER_AUTHORITY_KEY_FD")
    .output()
    .expect("repeat init without credential");
  let after = std::fs::read(project.0.join(".tenet/tenet.db")).expect("read database after");

  assert!(!output.status.success());
  assert_eq!(after, before);
}
