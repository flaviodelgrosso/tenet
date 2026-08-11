use std::path::{Path, PathBuf};

use crate::model::WorkerRole;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::fs;

pub const LOOPS_DIR: &str = ".loops";
pub const CONFIG_FILE: &str = "loops.toml";
pub const CONFIG_SCHEMA_URL: &str =
  "https://raw.githubusercontent.com/flaviodelgrosso/loops/main/schemas/config.schema.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
  pub version: u32,
  pub spec_file: String,
  pub max_cycles: u32,
  pub max_repair_attempts: u32,
  pub stagnation_limit: u32,
  pub agent: AgentConfig,
  pub verification: VerificationConfig,
  pub git: GitConfig,
  pub protected_paths: Vec<String>,
  #[serde(default)]
  pub skills: SkillsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
  pub command: String,
  pub model: Option<String>,
  pub thinking: String,
  #[serde(default, skip_serializing_if = "AgentRoleOverrides::is_empty")]
  pub roles: AgentRoleOverrides,
  pub auto_approve: bool,
  pub turn_timeout_secs: u64,
  pub read_only_tools: Vec<String>,
  pub coding_tools: Vec<String>,
  pub extra_args: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentRoleConfig {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub model: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub thinking: Option<String>,
}

impl AgentRoleConfig {
  fn is_empty(&self) -> bool {
    self.model.is_none() && self.thinking.is_none()
  }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentRoleOverrides {
  #[serde(skip_serializing_if = "AgentRoleConfig::is_empty")]
  pub architect: AgentRoleConfig,
  #[serde(skip_serializing_if = "AgentRoleConfig::is_empty")]
  pub reconcile: AgentRoleConfig,
  #[serde(skip_serializing_if = "AgentRoleConfig::is_empty")]
  pub implement: AgentRoleConfig,
  #[serde(skip_serializing_if = "AgentRoleConfig::is_empty")]
  pub repair: AgentRoleConfig,
  #[serde(skip_serializing_if = "AgentRoleConfig::is_empty")]
  pub assess: AgentRoleConfig,
}

impl AgentRoleOverrides {
  fn is_empty(&self) -> bool {
    self.architect.is_empty()
      && self.reconcile.is_empty()
      && self.implement.is_empty()
      && self.repair.is_empty()
      && self.assess.is_empty()
  }

  fn from_agent_defaults(model: Option<&str>, thinking: &str) -> Self {
    let role = || AgentRoleConfig {
      model: model.map(str::to_owned),
      thinking: Some(thinking.to_owned()),
    };
    Self {
      architect: role(),
      reconcile: role(),
      implement: role(),
      repair: role(),
      assess: role(),
    }
  }
}

impl AgentConfig {
  pub fn role(&self, role: WorkerRole) -> &AgentRoleConfig {
    match role {
      WorkerRole::Architect => &self.roles.architect,
      WorkerRole::Reconcile => &self.roles.reconcile,
      WorkerRole::Implement => &self.roles.implement,
      WorkerRole::Repair => &self.roles.repair,
      WorkerRole::Assess => &self.roles.assess,
    }
  }

  pub fn model_for(&self, role: WorkerRole) -> Option<&str> {
    self.role(role).model.as_deref().or(self.model.as_deref())
  }

  pub fn thinking_for(&self, role: WorkerRole) -> &str {
    self
      .role(role)
      .thinking
      .as_deref()
      .unwrap_or(&self.thinking)
  }

  fn populate_role_defaults(&mut self) {
    self.roles = AgentRoleOverrides::from_agent_defaults(self.model.as_deref(), &self.thinking);
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationConfig {
  pub auto_detect: bool,
  pub require_project_gate: bool,
  pub commands: Vec<String>,
  pub timeout_secs: u64,
  pub max_output_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitConfig {
  pub init: bool,
  pub auto_commit: bool,
  pub require_clean_tree: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
  pub shared: Vec<String>,
  pub roles: std::collections::BTreeMap<String, Vec<String>>,
}

impl SkillsConfig {
  pub fn role_paths(&self, role: crate::model::WorkerRole) -> impl Iterator<Item = &String> {
    self.roles.get(role.as_str()).into_iter().flatten()
  }
}

impl Default for Config {
  fn default() -> Self {
    Self {
      version: 1,
      spec_file: "spec.md".into(),
      max_cycles: 25,
      max_repair_attempts: 3,
      stagnation_limit: 3,
      agent: AgentConfig {
        command: "omp".into(),
        model: None,
        thinking: "high".into(),
        roles: AgentRoleOverrides::default(),
        auto_approve: true,
        turn_timeout_secs: 900,
        read_only_tools: vec!["read", "grep", "glob"]
          .into_iter()
          .map(str::to_owned)
          .collect(),
        coding_tools: vec!["read", "grep", "glob", "edit", "write", "bash"]
          .into_iter()
          .map(str::to_owned)
          .collect(),
        extra_args: Vec::new(),
      },
      verification: VerificationConfig {
        auto_detect: true,
        require_project_gate: true,
        commands: Vec::new(),
        timeout_secs: 120,
        max_output_bytes: 64 * 1024,
      },
      git: GitConfig {
        init: true,
        auto_commit: false,
        require_clean_tree: false,
      },
      protected_paths: vec![
        "spec.md",
        "AGENTS.md",
        "loops.toml",
        ".loops/state.json",
        ".loops/requirements.json",
        ".loops/roadmap.json",
        ".loops/STOP",
        ".loops/run.lock",
      ]
      .into_iter()
      .map(str::to_owned)
      .collect(),
      skills: SkillsConfig::default(),
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
  toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

#[cfg(test)]
mod tests {
  use tempfile::tempdir;

  use super::{config_path, ensure_config, read_config, Config, CONFIG_SCHEMA_URL, LOOPS_DIR};
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
    configure(&mut config);
    let mut text = toml::to_string_pretty(&config).unwrap();
    text.push_str(role_overrides);
    text
  }

  fn config_schema() -> serde_json::Value {
    serde_json::from_str(include_str!("../schemas/config.schema.json")).unwrap()
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
      "git",
      "protected_paths",
      "skills",
    ];
    expected_root.sort_unstable();

    let mut role_properties = schema["$defs"]["agentRoleOverrides"]["properties"]
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
      schema["$defs"]["agentRoleOverride"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>(),
      ["model", "thinking"].into_iter().collect(),
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
    assert_eq!(
      schema["$defs"]["agentConfig"]["properties"]["command"]["default"],
      config.agent.command
    );
    assert_eq!(
      schema["$defs"]["agentConfig"]["properties"]["thinking"]["default"],
      config.agent.thinking
    );
    assert_eq!(
      schema["$defs"]["agentConfig"]["properties"]["auto_approve"]["default"],
      config.agent.auto_approve
    );
    assert_eq!(
      schema["$defs"]["agentConfig"]["properties"]["turn_timeout_secs"]["default"],
      config.agent.turn_timeout_secs
    );
    assert_eq!(
      schema["$defs"]["agentConfig"]["properties"]["read_only_tools"]["default"],
      serde_json::to_value(config.agent.read_only_tools).unwrap()
    );
    assert_eq!(
      schema["$defs"]["agentConfig"]["properties"]["coding_tools"]["default"],
      serde_json::to_value(config.agent.coding_tools).unwrap()
    );
    assert_eq!(
      schema["$defs"]["agentConfig"]["properties"]["extra_args"]["default"],
      serde_json::to_value(config.agent.extra_args).unwrap()
    );
    assert_eq!(
      schema["$defs"]["verificationConfig"]["properties"]["auto_detect"]["default"],
      config.verification.auto_detect
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
      schema["$defs"]["gitConfig"]["properties"]["init"]["default"],
      config.git.init
    );
    assert_eq!(
      schema["$defs"]["gitConfig"]["properties"]["auto_commit"]["default"],
      config.git.auto_commit
    );
    assert_eq!(
      schema["$defs"]["gitConfig"]["properties"]["require_clean_tree"]["default"],
      config.git.require_clean_tree
    );
    assert_eq!(
      schema["properties"]["skills"]["default"],
      serde_json::json!({"shared": config.skills.shared, "roles": config.skills.roles})
    );
  }

  #[tokio::test]
  async fn generated_config_is_created_at_the_project_root_and_remains_parseable() {
    let project = tempdir().unwrap();

    ensure_config(project.path()).await.unwrap();
    let path = config_path(project.path());
    let generated = tokio::fs::read_to_string(&path).await.unwrap();
    let loaded = read_config(project.path()).await.unwrap();

    assert_eq!(path, project.path().join("loops.toml"));
    assert!(!project.path().join(LOOPS_DIR).join("config.toml").exists());
    assert!(generated.starts_with(&format!("#:schema {CONFIG_SCHEMA_URL}\n\n")));
    assert_eq!(loaded.version, Config::default().version);
  }

  #[tokio::test]
  async fn existing_config_without_schema_directive_is_not_rewritten() {
    let project = tempdir().unwrap();
    ensure_config(project.path()).await.unwrap();
    let legacy = toml::to_string_pretty(&Config::default()).unwrap();
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
        "\n[agent.roles.architect]\nmodel = \"architect-model\"\n",
        "[agent.roles.reconcile]\nthinking = \"medium\"\n",
        "[agent.roles.implement]\nmodel = \"implement-model\"\nthinking = \"high\"\n",
        "[agent.roles.repair]\nthinking = \"low\"\n",
        "[agent.roles.assess]\nmodel = \"assess-model\"\n",
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
        schema["$defs"]["agentRoleOverrides"]["properties"]
          .get(role.as_str())
          .is_some(),
        "schema is missing the {} role",
        role.as_str()
      );
    }
  }

  #[tokio::test]
  async fn ensure_config_writes_supported_omp_tools() {
    let project = tempdir().unwrap();

    let config = ensure_config(project.path()).await.unwrap();
    let generated = tokio::fs::read_to_string(config_path(project.path()))
      .await
      .unwrap();

    assert_eq!(config.agent.read_only_tools, ["read", "grep", "glob"]);
    assert_eq!(
      config.agent.coding_tools,
      ["read", "grep", "glob", "edit", "write", "bash"]
    );
    assert!(generated.contains("glob"));
    assert!(!generated.contains("\"find\""));
    assert!(!generated.contains("\"ls\""));
    for role in ROLES {
      assert_eq!(config.agent.model_for(role), None);
      assert_eq!(config.agent.thinking_for(role), "high");
      assert!(generated.contains(&format!("[agent.roles.{}]", role.as_str())));
    }
  }

  #[test]
  fn legacy_agent_config_applies_global_values_to_every_role() {
    let text = serialized_config(
      |config| {
        config.agent.model = Some("legacy-model".into());
        config.agent.thinking = "medium".into();
      },
      "",
    );

    let loaded: Config = toml::from_str(&text).unwrap();

    for role in ROLES {
      assert_eq!(loaded.agent.model_for(role), Some("legacy-model"));
      assert_eq!(loaded.agent.thinking_for(role), "medium");
    }
  }

  #[test]
  fn architect_can_override_model_and_thinking() {
    let text = serialized_config(
      |_| {},
      "\n[agent.roles.architect]\nmodel = \"architect-model\"\nthinking = \"xhigh\"\n",
    );

    let loaded: Config = toml::from_str(&text).unwrap();

    assert_eq!(
      (
        loaded.agent.model_for(WorkerRole::Architect),
        loaded.agent.thinking_for(WorkerRole::Architect),
      ),
      (Some("architect-model"), "xhigh"),
    );
  }

  #[test]
  fn thinking_only_override_inherits_global_model() {
    let text = serialized_config(
      |config| {
        config.agent.model = Some("global-model".into());
        config.agent.thinking = "medium".into();
      },
      "\n[agent.roles.implement]\nthinking = \"low\"\n",
    );

    let loaded: Config = toml::from_str(&text).unwrap();

    assert_eq!(
      (
        loaded.agent.model_for(WorkerRole::Implement),
        loaded.agent.thinking_for(WorkerRole::Implement),
      ),
      (Some("global-model"), "low"),
    );
  }

  #[test]
  fn model_only_override_inherits_global_thinking() {
    let text = serialized_config(
      |config| config.agent.thinking = "medium".into(),
      "\n[agent.roles.assess]\nmodel = \"assessment-model\"\n",
    );

    let loaded: Config = toml::from_str(&text).unwrap();

    assert_eq!(
      (
        loaded.agent.model_for(WorkerRole::Assess),
        loaded.agent.thinking_for(WorkerRole::Assess),
      ),
      (Some("assessment-model"), "medium"),
    );
  }

  #[test]
  fn architect_override_does_not_affect_other_roles() {
    let text = serialized_config(
      |config| {
        config.agent.model = Some("global-model".into());
        config.agent.thinking = "medium".into();
      },
      "\n[agent.roles.architect]\nmodel = \"architect-model\"\nthinking = \"xhigh\"\n",
    );

    let loaded: Config = toml::from_str(&text).unwrap();

    for role in [
      WorkerRole::Reconcile,
      WorkerRole::Implement,
      WorkerRole::Repair,
      WorkerRole::Assess,
    ] {
      assert_eq!(loaded.agent.model_for(role), Some("global-model"));
      assert_eq!(loaded.agent.thinking_for(role), "medium");
    }
  }

  #[test]
  fn unknown_worker_role_is_rejected() {
    let text = serialized_config(|_| {}, "\n[agent.roles.implment]\nthinking = \"high\"\n");

    let error = toml::from_str::<Config>(&text).unwrap_err().to_string();

    assert!(error.contains("implment"), "unexpected error: {error}");
  }

  #[test]
  fn missing_global_model_remains_none_except_for_overridden_role() {
    let text = serialized_config(
      |_| {},
      "\n[agent.roles.implement]\nmodel = \"implementation-model\"\n",
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
  async fn missing_skills_section_uses_reproducible_defaults() {
    let project = tempdir().unwrap();
    let config = ensure_config(project.path()).await.unwrap();
    let generated = tokio::fs::read_to_string(config_path(project.path()))
      .await
      .unwrap();
    let legacy = generated.split("\n[skills]\n").next().unwrap();
    tokio::fs::write(config_path(project.path()), legacy)
      .await
      .unwrap();

    let loaded = super::read_config(project.path()).await.unwrap();
    assert!(loaded.skills.shared.is_empty());
    assert!(loaded.skills.roles.is_empty());
    assert_eq!(loaded.spec_file, config.spec_file);
  }
}
