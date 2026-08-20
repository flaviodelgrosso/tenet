use std::{
  path::PathBuf,
  process::Command,
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

#[test]
fn init_writes_a_local_configuration_schema() {
  let project = TempProject::new();

  let output = Command::new(env!("CARGO_BIN_EXE_tenet"))
    .args(["init", "--cwd"])
    .arg(&project.0)
    .output()
    .expect("run tenet init");

  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let config =
    std::fs::read_to_string(project.0.join("tenet.toml")).expect("read generated config");
  assert!(config.starts_with("#:schema ./.tenet/config.schema.json\n\n"));
  assert!(
    config.contains("[verification]\nchecks = []\ntimeout_secs = 300\nmax_output_bytes = 65536")
  );
  assert!(!config.contains("Configure at least one trusted project check"));
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
}
