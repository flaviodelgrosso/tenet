use std::process::Command;

#[test]
fn missing_controller_authority_identity_fails_explicitly() {
  const CHILD: &str = "TENET_TEST_MISSING_AUTHORITY_CHILD";
  const NAMESPACE: &str = "TENET_CONTROLLER_AUTHORITY_NAMESPACE";
  const KEY_FD: &str = "TENET_CONTROLLER_AUTHORITY_KEY_FD";

  if std::env::var_os(CHILD).is_some() {
    let error = tenet_runtime::store::bootstrap_controller_authority_identity()
      .expect_err("missing authority identity must fail closed");
    assert!(error.to_string().contains(NAMESPACE));
    return;
  }

  let status = Command::new(std::env::current_exe().expect("current test executable"))
    .args([
      "--exact",
      "missing_controller_authority_identity_fails_explicitly",
      "--nocapture",
    ])
    .env(CHILD, "1")
    .env_remove(NAMESPACE)
    .env_remove(KEY_FD)
    .status()
    .expect("run isolated authority bootstrap test");
  assert!(status.success());
}
