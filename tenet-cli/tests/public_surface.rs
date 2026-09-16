use std::{
  io::Write,
  process::{Command, Stdio},
};

#[test]
fn cli_exposes_the_canonical_lifecycle_commands() {
  let output = Command::new(env!("CARGO_BIN_EXE_tenet"))
    .arg("--help")
    .output()
    .unwrap();
  assert!(output.status.success());
  let help = String::from_utf8(output.stdout).unwrap();
  for command in [
    "init",
    "doctor",
    "status",
    "authority",
    "requirement",
    "verify",
    "blockers",
    "evidence",
    "receipt",
    "mcp",
    "version",
  ] {
    assert!(
      help
        .lines()
        .any(|line| line.trim_start().starts_with(command)),
      "missing `{command}` in {help}"
    );
  }
  for legacy in [
    "gate",
    "candidate",
    "contract",
    "propose",
    "approve",
    "seal",
    "select",
    "capture",
  ] {
    assert!(
      !help
        .lines()
        .any(|line| line.trim_start().starts_with(legacy))
    );
  }
}

#[test]
fn mcp_exposes_exactly_four_completion_operations() {
  let mut child = Command::new(env!("CARGO_BIN_EXE_tenet"))
    .arg("mcp")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .spawn()
    .unwrap();
  let input = concat!(
    "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n",
    "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
    "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n"
  );
  child
    .stdin
    .take()
    .unwrap()
    .write_all(input.as_bytes())
    .unwrap();
  let output = child.wait_with_output().unwrap();
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let response = String::from_utf8(output.stdout).unwrap();
  let tools = response
    .lines()
    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
    .find(|message| message.get("id") == Some(&serde_json::json!(2)))
    .and_then(|message| message.pointer("/result/tools").cloned())
    .and_then(|tools| tools.as_array().cloned())
    .expect("tools/list response");
  let authority_tool = tools
    .iter()
    .find(|tool| tool.get("name") == Some(&serde_json::json!("tenet_authority_submit")))
    .expect("authority tool");
  let authority_schema = authority_tool
    .get("inputSchema")
    .expect("authority input schema")
    .to_string();
  for stage in ["PROPOSAL", "RECONCILIATION", "CLARIFICATION", "ADMISSION"] {
    assert!(authority_schema.contains(stage), "missing stage {stage}");
  }
  let initialize = response
    .lines()
    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
    .find(|message| message.get("id") == Some(&serde_json::json!(1)))
    .expect("initialize response")
    .to_string();
  for boundary in [
    "same-user tamper resistance",
    "independent authorship",
    "sandboxing",
    "writer authentication",
    "cryptographic human identity",
    "verifier Pass is not task completion",
  ] {
    assert!(initialize.contains(boundary), "missing boundary {boundary}");
  }
  let mut names = tools
    .iter()
    .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
    .collect::<Vec<_>>();
  names.sort_unstable();
  assert_eq!(
    names,
    [
      "tenet_authority_submit",
      "tenet_context",
      "tenet_requirement_check",
      "tenet_verify",
    ]
  );
}
