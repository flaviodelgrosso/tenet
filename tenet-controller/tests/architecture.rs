use std::{fs, path::Path};

fn manifest(path: &Path) -> String {
  fs::read_to_string(path).expect("read crate manifest")
}

#[test]
fn controller_does_not_depend_on_acp() {
  let root = Path::new(env!("CARGO_MANIFEST_DIR"));
  let controller = manifest(&root.join("Cargo.toml"));

  assert!(!controller.contains("tenet-acp"));
}

#[test]
fn runtime_does_not_depend_on_controller() {
  let root = Path::new(env!("CARGO_MANIFEST_DIR"));
  let runtime = manifest(&root.join("../tenet-runtime/Cargo.toml"));

  assert!(!runtime.contains("tenet-controller"));
}
