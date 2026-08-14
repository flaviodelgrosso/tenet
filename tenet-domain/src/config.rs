use std::path::{Path, PathBuf};

use crate::model::WorkerRole;
use anyhow::{Context, Result};
use serde::{de, ser::SerializeStruct, Deserialize, Serialize};
use tokio::fs;

pub const TENET_DIR: &str = ".tenet";
pub const CONFIG_FILE: &str = "tenet.toml";
pub const CONFIG_SCHEMA_URL: &str =
  "https://raw.githubusercontent.com/flaviodelgrosso/tenet/main/schemas/config.schema.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
  pub version: u32,
  pub spec_file: String,
  pub max_cycles: u32,
  pub max_repair_attempts: u32,
  pub stagnation_limit: u32,
  pub agent: AgentConfig,
  pub verification: VerificationConfig,
  pub execution: ExecutionConfig,
  pub integration: IntegrationConfig,
  pub protected_paths: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RolePreference {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub model: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub thought_level: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub mode: Option<String>,
}

fn default_completion_retries() -> u32 {
  2
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
  pub id: Option<String>,
  pub custom: Option<CustomAgentConfig>,
  pub completion_retries: u32,
  pub preferences: AgentPreferences,
  pub turn_timeout_secs: u64,
}

impl Serialize for AgentConfig {
  fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
  where
    S: serde::Serializer,
  {
    let preferences = &self.preferences;

    let mut state = serializer.serialize_struct("AgentConfig", 5)?;
    if let Some(id) = &self.id {
      state.serialize_field("id", id)?;
    }
    if let Some(custom) = &self.custom {
      state.serialize_field("custom", custom)?;
    }
    state.serialize_field("completion_retries", &self.completion_retries)?;
    state.serialize_field("preferences", &preferences)?;
    state.serialize_field("turn_timeout_secs", &self.turn_timeout_secs)?;
    state.end()
  }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentConfigWire {
  id: Option<String>,
  custom: Option<CustomAgentConfig>,
  #[serde(default = "default_completion_retries")]
  completion_retries: u32,
  #[serde(default)]
  preferences: AgentPreferences,
  turn_timeout_secs: u64,
}

impl<'de> Deserialize<'de> for AgentConfig {
  fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
  where
    D: de::Deserializer<'de>,
  {
    let wire = AgentConfigWire::deserialize(deserializer)?;
    let config = Self {
      id: wire.id,
      custom: wire.custom,
      completion_retries: wire.completion_retries,
      preferences: wire.preferences,
      turn_timeout_secs: wire.turn_timeout_secs,
    };
    config.validate_launch_source().map_err(de::Error::custom)?;
    Ok(config)
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomAgentConfig {
  pub command: String,
  #[serde(default)]
  pub args: Vec<String>,
  #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
  pub env: std::collections::BTreeMap<String, String>,
}

impl AgentConfig {
  pub fn has_launch_source(&self) -> bool {
    self.id.as_deref().is_some_and(|id| !id.trim().is_empty())
      || self
        .custom
        .as_ref()
        .is_some_and(|custom| !custom.command.trim().is_empty())
  }

  pub fn validate_launch_source(&self) -> Result<()> {
    if self.id.as_deref().is_some_and(|id| id.trim().is_empty()) {
      anyhow::bail!("agent.id must not be blank");
    }
    if self
      .custom
      .as_ref()
      .is_some_and(|custom| custom.command.trim().is_empty())
    {
      anyhow::bail!("agent.custom.command must not be blank");
    }
    match (&self.id, &self.custom) {
      (Some(_), Some(_)) => anyhow::bail!("ambiguous ACP launch source: choose either agent.id or agent.custom.command, not both"),
      (None, None) => anyhow::bail!("no ACP launch source configured: select a Registry agent with agent.id or configure agent.custom.command"),
      _ => Ok(()),
    }
  }
  pub fn model_for(&self, role: WorkerRole) -> Option<&str> {
    self
      .preferences
      .roles
      .get(role.as_str())
      .and_then(|p| p.model.as_deref())
      .or(self.preferences.default.model.as_deref())
  }

  pub fn thinking_for(&self, role: WorkerRole) -> &str {
    self
      .preferences
      .roles
      .get(role.as_str())
      .and_then(|p| p.thought_level.as_deref())
      .or(self.preferences.default.thought_level.as_deref())
      .unwrap_or("high")
  }

  pub fn preferences_for(&self, role: WorkerRole) -> RolePreference {
    self.preferences.for_role(role)
  }

  fn populate_role_defaults(&mut self) {
    self
      .preferences
      .default
      .thought_level
      .get_or_insert_with(|| "high".into());
  }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentPreferences {
  pub default: RolePreference,
  pub roles: std::collections::BTreeMap<String, RolePreference>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AgentPreferencesWire {
  default: RolePreference,
  roles: std::collections::BTreeMap<String, RolePreference>,
}

impl<'de> Deserialize<'de> for AgentPreferences {
  fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
  where
    D: de::Deserializer<'de>,
  {
    let wire = AgentPreferencesWire::deserialize(deserializer)?;
    let preferences = Self {
      default: wire.default,
      roles: wire.roles,
    };
    preferences
      .validate_role_names()
      .map_err(de::Error::custom)?;
    Ok(preferences)
  }
}

impl Default for AgentPreferences {
  fn default() -> Self {
    Self {
      default: RolePreference {
        thought_level: Some("high".into()),
        ..RolePreference::default()
      },
      roles: std::collections::BTreeMap::new(),
    }
  }
}

impl AgentPreferences {
  pub fn for_role(&self, role: WorkerRole) -> RolePreference {
    let mut preference = self.default.clone();
    if let Some(override_) = self.roles.get(role.as_str()) {
      if override_.model.is_some() {
        preference.model = override_.model.clone();
      }
      if override_.thought_level.is_some() {
        preference.thought_level = override_.thought_level.clone();
      }
      if override_.mode.is_some() {
        preference.mode = override_.mode.clone();
      }
    }
    preference
  }

  fn validate_role_names(&self) -> Result<()> {
    for role in self.roles.keys() {
      if !matches!(
        role.as_str(),
        "architect" | "reconcile" | "implement" | "repair" | "assess"
      ) {
        anyhow::bail!("unknown agent worker role {role:?}");
      }
    }
    Ok(())
  }
}

#[derive(Debug, Clone, Serialize)]
pub struct VerificationConfig {
  pub require_project_gate: bool,
  pub commands: Vec<String>,
  pub timeout_secs: u64,
  pub max_output_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerificationConfigWire {
  require_project_gate: bool,
  commands: Vec<String>,
  timeout_secs: u64,
  max_output_bytes: usize,
}

impl<'de> Deserialize<'de> for VerificationConfig {
  fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
  where
    D: de::Deserializer<'de>,
  {
    let wire = VerificationConfigWire::deserialize(deserializer)?;
    let config = Self {
      require_project_gate: wire.require_project_gate,
      commands: wire.commands,
      timeout_secs: wire.timeout_secs,
      max_output_bytes: wire.max_output_bytes,
    };
    config.validate().map_err(de::Error::custom)?;
    Ok(config)
  }
}

impl VerificationConfig {
  fn validate(&self) -> Result<()> {
    if self.require_project_gate && self.commands.is_empty() {
      anyhow::bail!("verification.commands must contain at least one command when verification.require_project_gate is enabled");
    }
    Ok(())
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionConfig {
  pub max_parallel_workers: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationStrategy {
  CherryPick,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationConfig {
  pub strategy: IntegrationStrategy,
  pub verify_each_candidate: bool,
}

impl Default for Config {
  fn default() -> Self {
    Self {
      version: 1,
      spec_file: ".tenet/spec.md".into(),
      max_cycles: 25,
      max_repair_attempts: 3,
      stagnation_limit: 3,
      agent: AgentConfig {
        id: None,
        custom: None,
        completion_retries: 2,
        preferences: AgentPreferences {
          default: RolePreference {
            thought_level: Some("high".into()),
            ..RolePreference::default()
          },
          roles: std::collections::BTreeMap::new(),
        },
        turn_timeout_secs: 900,
      },
      verification: VerificationConfig {
        require_project_gate: false,
        commands: Vec::new(),
        timeout_secs: 120,
        max_output_bytes: 64 * 1024,
      },
      execution: ExecutionConfig {
        max_parallel_workers: 1,
      },
      integration: IntegrationConfig {
        strategy: IntegrationStrategy::CherryPick,
        verify_each_candidate: true,
      },
      protected_paths: vec![
        ".tenet/spec.md",
        "AGENTS.md",
        "tenet.toml",
        ".tenet/state.json",
        ".tenet/requirements.json",
        ".tenet/roadmap.json",
        ".tenet/STOP",
        ".tenet/run.lock",
      ]
      .into_iter()
      .map(str::to_owned)
      .collect(),
    }
  }
}

pub fn config_path(cwd: &Path) -> PathBuf {
  cwd.join(CONFIG_FILE)
}

pub async fn ensure_config(cwd: &Path) -> Result<Config> {
  let path = config_path(cwd);
  if !path.exists() {
    let mut config = Config::default();
    config.agent.populate_role_defaults();
    let body = toml::to_string_pretty(&config)?;
    let text = format!("#:schema {CONFIG_SCHEMA_URL}\n\n{body}");
    fs::write(&path, text).await?;
    return Ok(config);
  }
  read_config(cwd).await
}

pub async fn read_config(cwd: &Path) -> Result<Config> {
  let path = config_path(cwd);
  let text = fs::read_to_string(&path)
    .await
    .with_context(|| format!("read {}", path.display()))?;
  let config: Config =
    toml::from_str(&text).map_err(|error| anyhow::anyhow!("parse {}: {error}", path.display()))?;
  config.agent.validate_launch_source()?;
  if config.execution.max_parallel_workers == 0 {
    anyhow::bail!("execution.max_parallel_workers must be at least 1");
  }
  Ok(config)
}

#[cfg(test)]
mod tests {
  use tempfile::tempdir;

  use super::{config_path, ensure_config, read_config, Config, CONFIG_SCHEMA_URL, TENET_DIR};
  use crate::model::WorkerRole;

  const ROLES: [WorkerRole; 5] = [
    WorkerRole::Architect,
    WorkerRole::Reconcile,
    WorkerRole::Implement,
    WorkerRole::Repair,
    WorkerRole::Assess,
  ];

  fn serialized_config(configure: impl FnOnce(&mut Config), role_overrides: &str) -> String {
    let mut config = Config::default();
    config.agent.id = Some("test-agent".into());
    configure(&mut config);
    let mut text = toml::to_string_pretty(&config).unwrap();
    text.push_str(role_overrides);
    text
  }

  async fn read_error(text: String) -> String {
    let project = tempdir().unwrap();
    tokio::fs::write(config_path(project.path()), text)
      .await
      .unwrap();
    read_config(project.path()).await.unwrap_err().to_string()
  }

  fn config_schema() -> serde_json::Value {
    serde_json::from_str(include_str!("../../schemas/config.schema.json")).unwrap()
  }

  #[test]
  fn config_schema_is_valid_json() {
    config_schema();
  }

  #[test]
  fn config_schema_root_and_role_properties_match_config() {
    let schema = config_schema();
    let mut root_properties = schema["properties"]
      .as_object()
      .unwrap()
      .keys()
      .map(String::as_str)
      .collect::<Vec<_>>();
    root_properties.sort_unstable();
    let mut expected_root = vec![
      "version",
      "spec_file",
      "max_cycles",
      "max_repair_attempts",
      "stagnation_limit",
      "agent",
      "verification",
      "execution",
      "integration",
      "protected_paths",
    ];
    expected_root.sort_unstable();

    let mut role_properties = schema["$defs"]["agentPreferenceRoles"]["properties"]
      .as_object()
      .unwrap()
      .keys()
      .map(String::as_str)
      .collect::<Vec<_>>();
    role_properties.sort_unstable();
    let mut expected_roles = ROLES.map(WorkerRole::as_str);
    expected_roles.sort_unstable();

    assert_eq!(root_properties, expected_root);
    assert_eq!(role_properties, expected_roles);
    assert_eq!(
      schema["$defs"]["rolePreference"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>(),
      ["model", "thought_level", "mode"].into_iter().collect(),
    );
    assert_eq!(
      schema["$defs"]["executionConfig"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>(),
      ["max_parallel_workers"].into_iter().collect(),
    );
  }

  #[test]
  fn config_schema_defaults_match_config_defaults() {
    let schema = config_schema();
    let config = Config::default();

    assert_eq!(schema["properties"]["version"]["default"], config.version);
    assert_eq!(
      schema["properties"]["spec_file"]["default"],
      config.spec_file
    );
    assert_eq!(
      schema["properties"]["max_cycles"]["default"],
      config.max_cycles
    );
    assert_eq!(
      schema["properties"]["max_repair_attempts"]["default"],
      config.max_repair_attempts
    );
    assert_eq!(
      schema["properties"]["stagnation_limit"]["default"],
      config.stagnation_limit
    );
    assert_eq!(
      schema["properties"]["protected_paths"]["default"],
      serde_json::to_value(config.protected_paths).unwrap()
    );
    assert!(schema["$defs"]["agentConfig"]["properties"]["id"].is_object());
    assert!(schema["$defs"]["agentConfig"]["properties"]["custom"].is_object());
    assert_eq!(
      schema["$defs"]["agentConfig"]["properties"]["completion_retries"]["default"],
      config.agent.completion_retries
    );
    assert_eq!(
      schema["$defs"]["agentConfig"]["properties"]["preferences"]["default"],
      serde_json::to_value(&config.agent.preferences).unwrap()
    );
    assert_eq!(
      schema["$defs"]["agentConfig"]["properties"]["turn_timeout_secs"]["default"],
      config.agent.turn_timeout_secs
    );
    assert_eq!(
      schema["$defs"]["verificationConfig"]["properties"]["require_project_gate"]["default"],
      config.verification.require_project_gate
    );
    assert_eq!(
      schema["$defs"]["verificationConfig"]["properties"]["commands"]["default"],
      serde_json::to_value(config.verification.commands).unwrap()
    );
    assert_eq!(
      schema["$defs"]["verificationConfig"]["properties"]["timeout_secs"]["default"],
      config.verification.timeout_secs
    );
    assert_eq!(
      schema["$defs"]["verificationConfig"]["properties"]["max_output_bytes"]["default"],
      config.verification.max_output_bytes
    );
    assert_eq!(
      schema["$defs"]["executionConfig"]["properties"]["max_parallel_workers"]["default"],
      config.execution.max_parallel_workers
    );
    assert_eq!(
      schema["$defs"]["integrationConfig"]["properties"]["strategy"]["default"],
      serde_json::to_value(config.integration.strategy).unwrap()
    );
    assert_eq!(
      schema["$defs"]["integrationConfig"]["properties"]["verify_each_candidate"]["default"],
      config.integration.verify_each_candidate
    );
  }

  #[test]
  fn config_schema_requires_commands_when_project_gate_is_enabled() {
    let schema = config_schema();

    assert_eq!(
      (
        &schema["$defs"]["verificationConfig"]["allOf"][0]["if"]["properties"]
          ["require_project_gate"]["const"],
        &schema["$defs"]["verificationConfig"]["allOf"][0]["then"]["properties"]["commands"]
          ["minItems"],
      ),
      (&serde_json::json!(true), &serde_json::json!(1)),
    );
  }

  #[tokio::test]
  async fn generated_config_is_created_at_the_project_root_and_requires_a_source() {
    let project = tempdir().unwrap();

    ensure_config(project.path()).await.unwrap();
    let path = config_path(project.path());
    let generated = tokio::fs::read_to_string(&path).await.unwrap();
    let error = read_config(project.path()).await.unwrap_err().to_string();

    assert_eq!(path, project.path().join("tenet.toml"));
    assert!(!project.path().join(TENET_DIR).join("config.toml").exists());
    assert!(generated.contains("spec_file = \".tenet/spec.md\""));
    assert!(!generated.contains("[git]"));
    assert!(!generated.contains("workspace"));
    assert!(!generated.contains("require_clean_base"));
    assert!(generated.starts_with(&format!("#:schema {CONFIG_SCHEMA_URL}\n\n")));
    assert!(error.contains("no ACP launch source configured"));
  }

  #[tokio::test]
  async fn existing_config_without_schema_directive_is_not_rewritten() {
    let project = tempdir().unwrap();
    ensure_config(project.path()).await.unwrap();
    let legacy = serialized_config(|_| {}, "");
    tokio::fs::write(config_path(project.path()), &legacy)
      .await
      .unwrap();

    let loaded = ensure_config(project.path()).await.unwrap();
    let persisted = tokio::fs::read_to_string(config_path(project.path()))
      .await
      .unwrap();

    assert_eq!(loaded.version, Config::default().version);
    assert_eq!(persisted, legacy);
  }

  #[test]
  fn representative_role_overrides_are_supported_by_config_and_schema() {
    let text = serialized_config(
      |_| {},
      concat!(
        "\n[agent.preferences.roles.architect]\nmodel = \"architect-model\"\n",
        "[agent.preferences.roles.reconcile]\nthought_level = \"medium\"\n",
        "[agent.preferences.roles.implement]\nmodel = \"implement-model\"\nthought_level = \"high\"\n",
        "[agent.preferences.roles.repair]\nthought_level = \"low\"\n",
        "[agent.preferences.roles.assess]\nmodel = \"assess-model\"\n",
      ),
    );

    let loaded: Config = toml::from_str(&text).unwrap();
    let schema = config_schema();

    assert_eq!(
      (
        loaded.agent.model_for(WorkerRole::Implement),
        loaded.agent.thinking_for(WorkerRole::Implement),
      ),
      (Some("implement-model"), "high"),
    );
    for role in ROLES {
      assert!(
        schema["$defs"]["agentPreferenceRoles"]["properties"]
          .get(role.as_str())
          .is_some(),
        "schema is missing the {} role",
        role.as_str()
      );
    }
  }
  #[test]
  fn launch_sources_are_exclusive_and_required() {
    let mut config = Config::default();
    assert!(config.agent.validate_launch_source().is_err());
    config.agent.id = Some("pi-acp".into());
    assert!(config.agent.validate_launch_source().is_ok());
    config.agent.custom = Some(super::CustomAgentConfig {
      command: "omp".into(),
      args: vec!["acp".into()],
      env: Default::default(),
    });
    let error = config
      .agent
      .validate_launch_source()
      .unwrap_err()
      .to_string();
    assert!(error.contains("ambiguous ACP launch source"));
  }

  #[tokio::test]
  async fn read_config_rejects_missing_launch_source() {
    let error = read_error(toml::to_string_pretty(&Config::default()).unwrap()).await;

    assert!(error.contains("no ACP launch source configured"));
  }

  #[tokio::test]
  async fn read_config_rejects_ambiguous_launch_sources() {
    let text = serialized_config(
      |config| {
        config.agent.custom = Some(super::CustomAgentConfig {
          command: "agent".into(),
          args: Vec::new(),
          env: Default::default(),
        });
      },
      "",
    );
    let error = read_error(text).await;

    assert!(error.contains("ambiguous ACP launch source"));
  }

  #[tokio::test]
  async fn read_config_rejects_empty_commands_when_project_gate_is_required() {
    let text = serialized_config(|config| config.verification.require_project_gate = true, "");
    let error = read_error(text).await;

    assert!(error.contains("verification.commands must contain at least one command"));
  }

  #[test]
  fn empty_commands_are_allowed_when_project_gate_is_disabled() {
    let text = serialized_config(|_| {}, "");

    assert!(toml::from_str::<Config>(&text).is_ok());
  }

  #[tokio::test]
  async fn read_config_rejects_blank_registry_identity() {
    let text = serialized_config(|config| config.agent.id = Some(" \t".into()), "");
    let error = read_error(text).await;

    assert!(error.contains("agent.id must not be blank"));
  }

  #[tokio::test]
  async fn read_config_rejects_blank_custom_command() {
    let text = serialized_config(
      |config| {
        config.agent.id = None;
        config.agent.custom = Some(super::CustomAgentConfig {
          command: " \t".into(),
          args: Vec::new(),
          env: Default::default(),
        });
      },
      "",
    );
    let error = read_error(text).await;

    assert!(error.contains("agent.custom.command must not be blank"));
  }

  #[tokio::test]
  async fn read_config_rejects_unknown_agent_fields() {
    let text = serialized_config(|_| {}, "").replacen(
      "turn_timeout_secs = 900\n",
      "turn_timeout_secs = 900\nauto_approve = true\n",
      1,
    );
    let error = read_error(text).await;

    assert!(error.contains("auto_approve"));
  }
  #[test]
  fn legacy_agent_preference_fields_are_rejected() {
    for (legacy_field, expected_error) in [
      ("model = \"legacy-model\"", "model"),
      ("thinking = \"medium\"", "thinking"),
      ("roles = {}", "roles"),
    ] {
      let text = serialized_config(|_| {}, "").replacen(
        "turn_timeout_secs = 900\n",
        &format!("turn_timeout_secs = 900\n{legacy_field}\n"),
        1,
      );

      let error = toml::from_str::<Config>(&text).unwrap_err().to_string();
      assert!(error.contains(expected_error), "unexpected error: {error}");
    }

    let text =
      serialized_config(|_| {}, "").replacen("thought_level = \"high\"", "thinking = \"high\"", 1);
    let error = toml::from_str::<Config>(&text).unwrap_err().to_string();
    assert!(error.contains("thinking"), "unexpected error: {error}");
  }

  #[test]
  fn required_role_preference_field_is_rejected() {
    let default_preference = serialized_config(|_| {}, "").replacen(
      "thought_level = \"high\"",
      "thought_level = \"high\"\nrequired = true",
      1,
    );
    let role_preference = serialized_config(
      |_| {},
      "\n[agent.preferences.roles.implement]\nrequired = true\n",
    );

    for text in [default_preference, role_preference] {
      let error = toml::from_str::<Config>(&text).unwrap_err().to_string();
      assert!(error.contains("required"), "unexpected error: {error}");
    }
  }

  #[test]
  fn unknown_worker_role_is_rejected() {
    let text = serialized_config(
      |_| {},
      "\n[agent.preferences.roles.implment]\nthought_level = \"high\"\n",
    );

    let error = toml::from_str::<Config>(&text).unwrap_err().to_string();

    assert!(error.contains("implment"), "unexpected error: {error}");
  }

  #[test]
  fn missing_global_model_remains_none_except_for_overridden_role() {
    let text = serialized_config(
      |_| {},
      "\n[agent.preferences.roles.implement]\nmodel = \"implementation-model\"\n",
    );

    let loaded: Config = toml::from_str(&text).unwrap();

    for role in [
      WorkerRole::Architect,
      WorkerRole::Reconcile,
      WorkerRole::Repair,
      WorkerRole::Assess,
    ] {
      assert_eq!(loaded.agent.model_for(role), None);
    }
    assert_eq!(
      loaded.agent.model_for(WorkerRole::Implement),
      Some("implementation-model"),
    );
  }

  #[tokio::test]
  async fn checked_in_sample_uses_custom_acp_and_retains_preferences() {
    let project = tempdir().unwrap();
    tokio::fs::write(
      config_path(project.path()),
      include_str!("../../tenet.toml"),
    )
    .await
    .unwrap();

    let loaded = read_config(project.path()).await.unwrap();
    let custom = loaded.agent.custom.as_ref().unwrap();

    assert_eq!(custom.command, "omp");
    assert_eq!(custom.args, vec!["acp".to_owned()]);
    assert_eq!(loaded.agent.thinking_for(WorkerRole::Architect), "xhigh");
    assert_eq!(loaded.agent.turn_timeout_secs, 900);
    assert_eq!(
      loaded.agent.model_for(WorkerRole::Architect),
      Some("openai-codex/gpt-5.6-sol"),
    );
  }
}
