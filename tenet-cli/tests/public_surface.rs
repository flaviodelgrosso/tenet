use std::{
  fs,
  io::Write,
  path::Path,
  process::{Command, Stdio},
};

use schemars::schema_for;
use tenet_domain::policy::ProjectConfig;

fn mcp_request(root: &Path, method: &str, params: serde_json::Value) -> serde_json::Value {
  let mut child = Command::new(env!("CARGO_BIN_EXE_tenet"))
    .arg("--cwd")
    .arg(root)
    .arg("mcp")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .spawn()
    .expect("spawn MCP");
  let request = serde_json::json!({
    "jsonrpc": "2.0",
    "id": 2,
    "method": method,
    "params": params,
  });
  let input = format!(
    concat!(
      "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{{}},\"clientInfo\":{{\"name\":\"test\",\"version\":\"1\"}}}}}}\n",
      "{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}}\n",
      "{request}\n",
    ),
    request = request,
  );
  child
    .stdin
    .take()
    .expect("MCP stdin")
    .write_all(input.as_bytes())
    .expect("write MCP request");
  let output = child.wait_with_output().expect("MCP response");
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  String::from_utf8(output.stdout)
    .expect("MCP UTF-8")
    .lines()
    .filter_map(|line| serde_json::from_str(line).ok())
    .find(|message: &serde_json::Value| message.get("id") == Some(&serde_json::json!(2)))
    .expect("MCP response for request")
}

fn mcp_tool(root: &Path, name: &str, arguments: serde_json::Value) -> serde_json::Value {
  let response = mcp_request(
    root,
    "tools/call",
    serde_json::json!({ "name": name, "arguments": arguments }),
  );
  let text = response
    .pointer("/result/content/0/text")
    .and_then(serde_json::Value::as_str)
    .expect("JSON tool result");
  serde_json::from_str(text).expect("tool result JSON")
}
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
    "unique across the Contract",
    "authoritative only under an admitted Authority",
    "never run tenet authority grant",
    "not even as a fail-closed probe",
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

#[test]
fn mcp_authoring_discovery_reaches_admission_without_internal_protocol_knowledge() {
  let directory = tempfile::tempdir().expect("repository");
  let root = directory.path();
  fs::write(root.join("SPEC.md"), "# Specification\n").expect("specification");
  let init = Command::new(env!("CARGO_BIN_EXE_tenet"))
    .arg("--cwd")
    .arg(root)
    .args(["init", "--json"])
    .output()
    .expect("initialize");
  assert!(
    init.status.success(),
    "{}",
    String::from_utf8_lossy(&init.stderr)
  );

  let initial_context = mcp_tool(root, "tenet_context", serde_json::json!({}));
  assert_eq!(initial_context["phase"], "AUTHORITY_REQUIRED");
  assert_eq!(
    initial_context.pointer("/authoring/configPath"),
    Some(&serde_json::json!(".tenet/tenet.toml"))
  );
  assert_eq!(
    initial_context.pointer("/authoring/missingPrerequisites"),
    Some(&serde_json::json!([
      "candidate_surface_not_configured",
      "no_verifiers_configured"
    ]))
  );

  let tools = mcp_request(root, "tools/list", serde_json::json!({}));
  let authority_schema = tools
    .pointer("/result/tools")
    .and_then(serde_json::Value::as_array)
    .and_then(|tools| {
      tools
        .iter()
        .find(|tool| tool["name"] == "tenet_authority_submit")
    })
    .and_then(|tool| tool.get("inputSchema"))
    .expect("authority-submit schema")
    .to_string();
  for field in ["PROPOSAL", "requirements", "criteria", "verifiers"] {
    assert!(
      authority_schema.contains(field),
      "tool schema must expose {field}"
    );
  }

  let resources = mcp_request(root, "resources/list", serde_json::json!({}));
  assert!(
    resources
      .pointer("/result/resources")
      .and_then(serde_json::Value::as_array)
      .is_some_and(|resources| {
        resources
          .iter()
          .any(|resource| resource["uri"] == "tenet://authoring/configuration")
      })
  );
  let config_before_resource_read =
    fs::read(root.join(".tenet/tenet.toml")).expect("initial configuration");
  let metadata = mcp_request(
    root,
    "resources/read",
    serde_json::json!({ "uri": "tenet://authoring/configuration" }),
  );
  let configuration = serde_json::from_str::<serde_json::Value>(
    metadata
      .pointer("/result/contents/0/text")
      .and_then(serde_json::Value::as_str)
      .expect("authoring metadata"),
  )
  .expect("authoring metadata JSON");
  assert_eq!(configuration["schemaVersion"], 1);
  assert_eq!(
    configuration["configurationSemantics"],
    "tenet:authoring-configuration-resource:v1"
  );
  assert_eq!(configuration["configPath"], ".tenet/tenet.toml");
  assert_eq!(
    configuration["configurationSchema"],
    serde_json::to_value(schema_for!(ProjectConfig)).expect("ProjectConfig schema")
  );
  assert_eq!(
    fs::read(root.join(".tenet/tenet.toml")).expect("configuration after resource read"),
    config_before_resource_read
  );
  let configuration_schema = &configuration["configurationSchema"].to_string();
  for field in [
    "candidate",
    "include",
    "exclude",
    "argv",
    "cwd",
    "timeoutMs",
    "max_output_bytes",
    "authority_snapshot",
    "protection",
    "inconclusive",
  ] {
    assert!(
      configuration_schema.contains(field),
      "configuration schema must expose {field}"
    );
  }
  let unknown_resource = mcp_request(
    root,
    "resources/read",
    serde_json::json!({ "uri": "tenet://authoring/unknown" }),
  );
  assert!(
    unknown_resource.get("error").is_some(),
    "unknown resource must fail explicitly: {unknown_resource}"
  );
  let malformed_resource = mcp_request(root, "resources/read", serde_json::json!({}));
  assert!(
    malformed_resource.get("error").is_some(),
    "malformed resource request must fail explicitly: {malformed_resource}"
  );
  fs::write(root.join("candidate.txt"), "candidate\n").expect("candidate");
  fs::write(root.join("verify.sh"), "#!/bin/sh\nexit 0\n").expect("verifier");
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(root.join("verify.sh"), fs::Permissions::from_mode(0o755))
      .expect("make verifier executable");
  }
  fs::write(
    root.join(".tenet/tenet.toml"),
    r#"
version = 1
spec_path = "SPEC.md"

[candidate]
root = "."
include = ["candidate.txt", "verify.sh"]
exclude = []

[[verifiers]]
id = "V1"
max_output_bytes = 4096
authority = "project"
protection = "local"

[verifiers.command]
argv = [{ kind = "candidate_path", value = "verify.sh" }]
cwd = { kind = "candidate", value = "." }
timeoutMs = 5000

[verifiers.command.result]
pass = [0]
fail = [1]
inconclusive = []
"#,
  )
  .expect("configured project policy");

  let configured_context = mcp_tool(root, "tenet_context", serde_json::json!({}));
  assert_eq!(configured_context["phase"], "AUTHORITY_REQUIRED");
  assert_eq!(
    configured_context.pointer("/authoring/candidateConfigured"),
    Some(&serde_json::json!(true))
  );
  assert_eq!(
    configured_context.pointer("/authoring/configuredVerifierIds"),
    Some(&serde_json::json!(["V1"]))
  );
  assert_eq!(
    configured_context.pointer("/authoring/missingPrerequisites"),
    Some(&serde_json::json!([]))
  );

  let proposal = mcp_tool(
    root,
    "tenet_authority_submit",
    serde_json::json!({
      "submission": {
        "stage": "PROPOSAL",
        "contract": {
          "schemaVersion": 1,
          "policy": "tenet:completion-policy:v1",
          "requirements": [{
            "id": "R1",
            "statement": "the candidate behavior is complete",
            "criteria": [{
              "id": "C1",
              "proposition": "the verifier passes",
              "verifiers": [{ "id": "V1", "material": "candidate" }],
              "evidence": {
                "control": "candidate_controlled_permitted",
                "assurance": "local_or_stronger"
              }
            }]
          }]
        },
        "issues": []
      }
    }),
  );
  let proposal_id = proposal["proposalId"]
    .as_str()
    .expect("proposal ID")
    .to_owned();
  let reconciliation = mcp_tool(
    root,
    "tenet_authority_submit",
    serde_json::json!({
      "submission": {
        "stage": "RECONCILIATION",
        "proposalId": proposal_id,
        "findings": []
      }
    }),
  );
  assert!(reconciliation["reconciliationId"].is_string());

  let admission_context = mcp_tool(root, "tenet_context", serde_json::json!({}));
  assert_eq!(admission_context["phase"], "AUTHORITY_ADMISSION");
  // The MCP adapter derives the same structured approval-UX preview as the
  // CLI: a host agent needs no content-ID copying on either surface.
  let preview = &admission_context["admission"];
  assert_eq!(preview["proposalId"], proposal_id);
  assert_eq!(preview["summary"]["requirements"], 1);
  assert_eq!(preview["summary"]["criteria"], 1);
  assert_eq!(preview["summary"]["verifiers"], 1);
  assert_eq!(preview["summary"]["assurance"], "LOCAL_V1");
  assert_eq!(preview["summary"]["candidateSurface"][0], "candidate.txt");
  assert_eq!(preview["handoff"]["command"][2], "admit-prepared");
  assert_eq!(preview["handoff"]["requiresTrustedSecret"], true);
}

#[test]
fn mcp_context_reports_authoring_facts_only_for_valid_configuration() {
  let directory = tempfile::tempdir().expect("repository");
  let root = directory.path();
  fs::write(root.join("SPEC.md"), "# Specification\n").expect("specification");
  let init = Command::new(env!("CARGO_BIN_EXE_tenet"))
    .arg("--cwd")
    .arg(root)
    .args(["init", "--json"])
    .output()
    .expect("initialize");
  assert!(
    init.status.success(),
    "{}",
    String::from_utf8_lossy(&init.stderr)
  );

  let unconfigured = mcp_tool(root, "tenet_context", serde_json::json!({}));
  assert_eq!(
    unconfigured.pointer("/authoring/missingPrerequisites"),
    Some(&serde_json::json!([
      "candidate_surface_not_configured",
      "no_verifiers_configured"
    ]))
  );

  fs::write(
    root.join(".tenet/tenet.toml"),
    "version = 1\nspec_path = \"SPEC.md\"\n\n[candidate]\ninclude = [\"candidate.txt\"]\n",
  )
  .expect("candidate-only configuration");
  let candidate_only = mcp_tool(root, "tenet_context", serde_json::json!({}));
  assert_eq!(
    candidate_only.pointer("/authoring/candidateConfigured"),
    Some(&serde_json::json!(true))
  );
  assert_eq!(
    candidate_only.pointer("/authoring/configuredVerifierIds"),
    Some(&serde_json::json!([]))
  );
  assert_eq!(
    candidate_only.pointer("/authoring/missingPrerequisites"),
    Some(&serde_json::json!(["no_verifiers_configured"]))
  );

  fs::write(
    root.join(".tenet/tenet.toml"),
    r#"
version = 1
spec_path = "SPEC.md"

[candidate]
include = ["candidate.txt"]

[[verifiers]]
id = "V1"
authority = "project"

[verifiers.command]
argv = [{ kind = "literal", value = "true" }]
cwd = { kind = "scratch" }
"#,
  )
  .expect("one-verifier configuration");
  let one_verifier = mcp_tool(root, "tenet_context", serde_json::json!({}));
  assert_eq!(
    one_verifier.pointer("/authoring/configuredVerifierIds"),
    Some(&serde_json::json!(["V1"]))
  );
  assert_eq!(
    one_verifier.pointer("/authoring/missingPrerequisites"),
    Some(&serde_json::json!([]))
  );

  fs::write(
    root.join(".tenet/tenet.toml"),
    r#"
version = 1
spec_path = "SPEC.md"

[candidate]
include = ["candidate.txt"]

[[verifiers]]
id = "V1"
authority = "project"

[verifiers.command]
argv = [{ kind = "literal", value = "true" }]
cwd = { kind = "scratch" }

[[verifiers]]
id = "V2"
authority = "project"

[verifiers.command]
argv = [{ kind = "literal", value = "true" }]
cwd = { kind = "scratch" }
"#,
  )
  .expect("multiple-verifier configuration");
  let multiple_verifiers = mcp_tool(root, "tenet_context", serde_json::json!({}));
  assert_eq!(
    multiple_verifiers.pointer("/authoring/configuredVerifierIds"),
    Some(&serde_json::json!(["V1", "V2"]))
  );

  fs::write(
    root.join(".tenet/tenet.toml"),
    "version = 999\nspec_path = \"SPEC.md\"\n",
  )
  .expect("unsupported configuration");
  let incompatible = mcp_tool(root, "tenet_context", serde_json::json!({}));
  assert_ne!(incompatible["phase"], "AUTHORITY_REQUIRED");
  assert!(incompatible.get("authoring").is_none());
}
