use std::process::Command;

#[test]
fn missing_controller_authority_identity_fails_explicitly() {
  const CHILD: &str = "TENET_TEST_MISSING_AUTHORITY_CHILD";
  const NAMESPACE: &str = "TENET_CONTROLLER_AUTHORITY_NAMESPACE";
  const KEY_FD: &str = "TENET_CONTROLLER_AUTHORITY_KEY_FD";

  if std::env::var_os(CHILD).is_some() {
    let repository = tempfile::tempdir().expect("temporary repository");
    let mut bootstrap = tenet_runtime::authority::AuthorityBootstrap::from_environment()
      .expect("select default authority provider");
    let error = bootstrap
      .install(repository.path())
      .expect_err("missing authority identity must fail closed");
    assert!(error.to_string().contains("tenet init"));
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

#[test]
fn partial_inherited_authority_configuration_fails_and_is_removed() {
  const CHILD: &str = "TENET_TEST_PARTIAL_AUTHORITY_CHILD";
  const NAMESPACE: &str = "TENET_CONTROLLER_AUTHORITY_NAMESPACE";
  const KEY_FD: &str = "TENET_CONTROLLER_AUTHORITY_KEY_FD";

  if std::env::var_os(CHILD).is_some() {
    let error = tenet_runtime::authority::AuthorityBootstrap::from_environment()
      .err()
      .expect("partial inherited authority must fail");
    assert!(error.to_string().contains("requires both"));
    assert!(std::env::var_os(NAMESPACE).is_none());
    assert!(std::env::var_os(KEY_FD).is_none());
    return;
  }

  let status = Command::new(std::env::current_exe().expect("current test executable"))
    .args([
      "--exact",
      "partial_inherited_authority_configuration_fails_and_is_removed",
      "--nocapture",
    ])
    .env(CHILD, "1")
    .env(NAMESPACE, "incomplete-authority")
    .env_remove(KEY_FD)
    .status()
    .expect("run isolated partial authority test");
  assert!(status.success());
}
