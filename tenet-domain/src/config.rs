use std::{
  collections::{BTreeMap, BTreeSet},
  fmt::Write as _,
  path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{de, ser::SerializeStruct, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs;

use crate::{
  model::WorkerRole, trusted_verifier::TrustedVerificationSpec, verification::VerificationSpec,
};

pub const TENET_DIR: &str = ".tenet";
pub const CONFIG_FILE: &str = "tenet.toml";
pub const CONFIG_SCHEMA_FILE: &str = "config.schema.json";
pub const CONFIG_SCHEMA_DIRECTIVE: &str = "./.tenet/config.schema.json";
pub const SUPPORTED_CONFIG_VERSION: u32 = 1;

const DEFAULT_COMPLETION_RETRIES: u32 = 2;
const DEFAULT_TURN_TIMEOUT_SECS: u64 = 900;
const DEFAULT_VERIFICATION_TIMEOUT_SECS: u64 = 300;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const DEFAULT_STAGNATION_LIMIT: u32 = 3;
const DEFAULT_MAX_PARALLEL_WORKERS: usize = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
  pub version: u32,
  pub spec_file: String,
  pub max_cycles: u32,
  pub max_repair_attempts: u32,
  #[serde(
    default = "default_stagnation_limit",
    skip_serializing_if = "is_default_stagnation_limit"
  )]
  pub stagnation_limit: u32,
  pub agent: AgentConfig,
  #[serde(default)]
  pub verification: VerificationConfig,
  #[serde(default, skip_serializing_if = "ExecutionConfig::is_default")]
  pub execution: ExecutionConfig,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub additional_protected_paths: Vec<String>,
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
  DEFAULT_COMPLETION_RETRIES
}

fn default_turn_timeout_secs() -> u64 {
  DEFAULT_TURN_TIMEOUT_SECS
}

fn default_verification_timeout_secs() -> u64 {
  DEFAULT_VERIFICATION_TIMEOUT_SECS
}

fn default_max_output_bytes() -> usize {
  DEFAULT_MAX_OUTPUT_BYTES
}

fn default_stagnation_limit() -> u32 {
  DEFAULT_STAGNATION_LIMIT
}

fn is_default_stagnation_limit(value: &u32) -> bool {
  *value == DEFAULT_STAGNATION_LIMIT
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
    let mut state = serializer.serialize_struct("AgentConfig", 5)?;
    if let Some(id) = &self.id {
      state.serialize_field("id", id)?;
    }
    if let Some(custom) = &self.custom {
      state.serialize_field("custom", custom)?;
    }
    if self.completion_retries != DEFAULT_COMPLETION_RETRIES {
      state.serialize_field("completion_retries", &self.completion_retries)?;
    }
    if self.preferences != AgentPreferences::default() {
      state.serialize_field("preferences", &self.preferences)?;
    }
    if self.turn_timeout_secs != DEFAULT_TURN_TIMEOUT_SECS {
      state.serialize_field("turn_timeout_secs", &self.turn_timeout_secs)?;
    }
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
  #[serde(default = "default_turn_timeout_secs")]
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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VerificationConfig {
  pub checks: Vec<ProjectVerificationCheck>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub trusted_checks: Vec<TrustedVerificationSpec>,
  #[serde(default = "default_verification_timeout_secs")]
  pub timeout_secs: u64,
  #[serde(default = "default_max_output_bytes")]
  pub max_output_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectVerificationCheck {
  pub name: String,
  pub command: Vec<String>,
  #[serde(
    default = "default_working_directory",
    skip_serializing_if = "is_default_working_directory"
  )]
  pub working_directory: String,
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  pub environment: BTreeMap<String, String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub timeout_secs: Option<u64>,
}

impl ProjectVerificationCheck {
  pub fn verification_spec(&self) -> Result<VerificationSpec> {
    let (program, args) = self
      .command
      .split_first()
      .context("project verification check command must contain an executable")?;
    Ok(VerificationSpec {
      program: program.clone(),
      args: args.to_vec(),
      working_directory: self.working_directory.clone(),
      environment: self.environment.clone(),
    })
  }

  pub fn effective_timeout_secs(&self, default: u64) -> u64 {
    self.timeout_secs.unwrap_or(default)
  }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerificationConfigWire {
  #[serde(default)]
  checks: Vec<ProjectVerificationCheck>,
  #[serde(default)]
  trusted_checks: Vec<TrustedVerificationSpec>,
  #[serde(default = "default_verification_timeout_secs")]
  timeout_secs: u64,
  #[serde(default = "default_max_output_bytes")]
  max_output_bytes: usize,
}

impl<'de> Deserialize<'de> for VerificationConfig {
  fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
  where
    D: de::Deserializer<'de>,
  {
    let wire = VerificationConfigWire::deserialize(deserializer)?;
    let config = Self {
      checks: wire.checks,
      trusted_checks: wire.trusted_checks,
      timeout_secs: wire.timeout_secs,
      max_output_bytes: wire.max_output_bytes,
    };
    config.validate().map_err(de::Error::custom)?;
    Ok(config)
  }
}

impl Default for VerificationConfig {
  fn default() -> Self {
    Self {
      checks: Vec::new(),
      trusted_checks: Vec::new(),
      timeout_secs: DEFAULT_VERIFICATION_TIMEOUT_SECS,
      max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
    }
  }
}

impl VerificationConfig {
  fn validate(&self) -> Result<()> {
    if self.timeout_secs == 0 {
      anyhow::bail!("verification.timeout_secs must be at least 1");
    }
    let mut trusted_names = BTreeSet::new();
    for check in &self.trusted_checks {
      check
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid trusted verifier {:?}: {error}", check.name))?;
      if !trusted_names.insert(check.name.as_str()) {
        anyhow::bail!("duplicate trusted verifier name {:?}", check.name);
      }
    }
    for check in &self.checks {
      if check.name.trim().is_empty() {
        anyhow::bail!("project verification check name must not be blank");
      }
      let Some(program) = check.command.first() else {
        anyhow::bail!("project verification check command must contain an executable");
      };
      if program.trim().is_empty() {
        anyhow::bail!("project verification check executable must not be blank");
      }
      if check.working_directory.trim().is_empty() {
        anyhow::bail!("project verification check working_directory must not be blank");
      }
      if check.timeout_secs == Some(0) {
        anyhow::bail!("project verification check timeout_secs must be at least 1");
      }
    }
    Ok(())
  }

  pub fn suite_hash(&self) -> Result<String> {
    let checks = self
      .checks
      .iter()
      .map(|check| {
        (
          &check.command,
          &check.working_directory,
          &check.environment,
          check.effective_timeout_secs(self.timeout_secs),
        )
      })
      .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&(
      "tenet-project-verification-suite-v1",
      checks,
      self.max_output_bytes,
    ))?;
    let digest = Sha256::digest(bytes);
    let mut hash = String::with_capacity(digest.len() * 2);
    for byte in digest {
      write!(hash, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(hash)
  }
}

fn default_working_directory() -> String {
  ".".into()
}

fn is_default_working_directory(value: &String) -> bool {
  value == "."
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ExecutionConfig {
  #[serde(skip_serializing_if = "ExecutionConfig::has_default_worker_count")]
  pub max_parallel_workers: usize,
}

impl Default for ExecutionConfig {
  fn default() -> Self {
    Self {
      max_parallel_workers: DEFAULT_MAX_PARALLEL_WORKERS,
    }
  }
}

impl ExecutionConfig {
  fn is_default(&self) -> bool {
    self == &Self::default()
  }

  fn has_default_worker_count(value: &usize) -> bool {
    *value == DEFAULT_MAX_PARALLEL_WORKERS
  }
}

impl Default for Config {
  fn default() -> Self {
    Self {
      version: SUPPORTED_CONFIG_VERSION,
      spec_file: "spec.md".into(),
      max_cycles: 25,
      max_repair_attempts: 3,
      stagnation_limit: DEFAULT_STAGNATION_LIMIT,
      agent: AgentConfig {
        id: None,
        custom: None,
        completion_retries: DEFAULT_COMPLETION_RETRIES,
        preferences: AgentPreferences::default(),
        turn_timeout_secs: DEFAULT_TURN_TIMEOUT_SECS,
      },
      verification: VerificationConfig::default(),
      execution: ExecutionConfig::default(),
      additional_protected_paths: Vec::new(),
    }
  }
}

pub fn config_path(cwd: &Path) -> PathBuf {
  cwd.join(CONFIG_FILE)
}

pub fn config_schema_path(cwd: &Path) -> PathBuf {
  cwd.join(TENET_DIR).join(CONFIG_SCHEMA_FILE)
}

async fn ensure_config_schema(cwd: &Path) -> Result<()> {
  let path = config_schema_path(cwd);
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent).await?;
  }
  fs::write(&path, include_str!("../../schemas/config.schema.json"))
    .await
    .with_context(|| format!("write {}", path.display()))?;
  Ok(())
}

pub async fn ensure_config(cwd: &Path) -> Result<Config> {
  ensure_config_schema(cwd).await?;
  let path = config_path(cwd);
  if !path.exists() {
    let config = Config::default();
    let body = toml::to_string_pretty(&config)?;
    let text = format!("#:schema {CONFIG_SCHEMA_DIRECTIVE}\n\n{body}");
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
  if config.version != SUPPORTED_CONFIG_VERSION {
    anyhow::bail!(
      "unsupported configuration version {}; supported version is {}",
      config.version,
      SUPPORTED_CONFIG_VERSION
    );
  }
  normalize_protected_path(&config.spec_file).map_err(|error| {
    anyhow::anyhow!(
      "invalid spec_file protected path {:?}: {error}",
      config.spec_file
    )
  })?;
  for path in &config.additional_protected_paths {
    normalize_protected_path(path).map_err(|error| {
      anyhow::anyhow!("invalid additional_protected_paths entry {path:?}: {error}")
    })?;
  }
  if config.stagnation_limit == 0 {
    anyhow::bail!("stagnation_limit must be at least 1");
  }
  if config.execution.max_parallel_workers == 0 {
    anyhow::bail!("execution.max_parallel_workers must be at least 1");
  }
  Ok(config)
}

pub fn normalize_protected_path(value: &str) -> Result<PathBuf> {
  let path = Path::new(value);
  if value.trim().is_empty() || path.is_absolute() {
    anyhow::bail!("protected path must be a nonblank repository-relative path: {value:?}");
  }
  let mut normalized = PathBuf::new();
  for component in path.components() {
    match component {
      Component::Normal(value) => normalized.push(value),
      Component::CurDir => {}
      Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
        anyhow::bail!("protected path escapes the repository: {value:?}")
      }
    }
  }
  if normalized.as_os_str().is_empty() {
    anyhow::bail!("protected path resolves to the repository root: {value:?}");
  }
  Ok(normalized)
}

#[cfg(test)]
mod tests {
  use tempfile::tempdir;

  use super::{
    config_path, config_schema_path, ensure_config, read_config, Config, CONFIG_SCHEMA_DIRECTIVE,
    CONFIG_SCHEMA_FILE, TENET_DIR,
  };
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
      "additional_protected_paths",
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
    assert_eq!(
      schema["$defs"]["verificationConfig"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>(),
      [
        "checks",
        "trusted_checks",
        "max_output_bytes",
        "timeout_secs"
      ]
      .into_iter()
      .collect(),
    );
    assert_eq!(
      schema["$defs"]["projectVerificationCheck"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>(),
      [
        "name",
        "command",
        "working_directory",
        "environment",
        "timeout_secs",
      ]
      .into_iter()
      .collect(),
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
      schema["properties"]["additional_protected_paths"]["default"],
      serde_json::json!([])
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
      schema["$defs"]["verificationConfig"]["properties"]["checks"]["default"],
      serde_json::json!([])
    );
    assert_eq!(
      schema["$defs"]["verificationConfig"]["properties"]["checks"]["minItems"],
      1
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
  }

  #[test]
  fn config_schema_excludes_controller_owned_policy_fields() {
    let schema = config_schema();

    assert!(schema["properties"].get("integration").is_none());
    assert!(schema["properties"].get("protected_paths").is_none());
    assert!(schema["$defs"]["verificationConfig"]["properties"]
      .get("require_project_gate")
      .is_none());
  }

  #[tokio::test]
  async fn generated_config_creates_its_referenced_local_schema() {
    let project = tempdir().unwrap();

    ensure_config(project.path()).await.unwrap();
    let path = config_path(project.path());
    let generated = tokio::fs::read_to_string(&path).await.unwrap();
    let schema: serde_json::Value = serde_json::from_str(
      &tokio::fs::read_to_string(config_schema_path(project.path()))
        .await
        .unwrap(),
    )
    .unwrap();
    let error = read_config(project.path()).await.unwrap_err().to_string();

    assert_eq!(path, project.path().join("tenet.toml"));
    assert!(!project.path().join(TENET_DIR).join("config.toml").exists());
    assert!(generated.starts_with(&format!("#:schema {CONFIG_SCHEMA_DIRECTIVE}\n\n")));
    assert_eq!(schema["$id"], CONFIG_SCHEMA_FILE);
    assert!(generated.contains("version = 1"));
    assert!(generated.contains("spec_file = \"spec.md\""));
    assert!(generated
      .contains("[verification]\nchecks = []\ntimeout_secs = 300\nmax_output_bytes = 65536"));
    for omitted in [
      "stagnation_limit",
      "completion_retries",
      "turn_timeout_secs",
      "max_parallel_workers",
      "integration",
      "protected_paths",
      "Configure at least one trusted project check",
      "[[verification.checks]]",
    ] {
      assert!(
        !generated.contains(omitted),
        "generated config contains {omitted}"
      );
    }
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

  #[test]
  fn omitted_advanced_settings_resolve_to_runtime_defaults() {
    let loaded: Config = toml::from_str(concat!(
      "version = 1\n",
      "spec_file = \"spec.md\"\n",
      "max_cycles = 25\n",
      "max_repair_attempts = 3\n",
      "[agent]\n",
      "id = \"test-agent\"\n",
    ))
    .unwrap();
    let defaults = Config::default();

    assert_eq!(loaded.stagnation_limit, defaults.stagnation_limit);
    assert_eq!(
      loaded.agent.completion_retries,
      defaults.agent.completion_retries
    );
    assert_eq!(
      loaded.agent.turn_timeout_secs,
      defaults.agent.turn_timeout_secs
    );
    assert_eq!(loaded.verification, defaults.verification);
    assert_eq!(loaded.execution, defaults.execution);
  }

  #[test]
  fn explicit_advanced_settings_round_trip() {
    let text = serialized_config(
      |config| {
        config.stagnation_limit = 7;
        config.agent.completion_retries = 4;
        config.agent.turn_timeout_secs = 30;
        config.verification.timeout_secs = 45;
        config.verification.max_output_bytes = 2048;
        config.execution.max_parallel_workers = 3;
        config.additional_protected_paths = vec!["secrets".into()];
      },
      "",
    );
    let loaded: Config = toml::from_str(&text).unwrap();

    assert_eq!(toml::to_string_pretty(&loaded).unwrap(), text);
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
    let text =
      serialized_config(|_| {}, "").replacen("[agent]\n", "[agent]\nauto_approve = true\n", 1);
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
        "[agent]\n",
        &format!("[agent]\n{legacy_field}\n"),
        1,
      );

      let error = toml::from_str::<Config>(&text).unwrap_err().to_string();
      assert!(error.contains(expected_error), "unexpected error: {error}");
    }

    let text = serialized_config(
      |_| {},
      "\n[agent.preferences.default]\nthinking = \"high\"\n",
    );
    let error = toml::from_str::<Config>(&text).unwrap_err().to_string();
    assert!(error.contains("thinking"), "unexpected error: {error}");
  }
  #[test]
  fn structured_project_checks_deserialize_without_catalog_ids() {
    let text = concat!(
      "version = 1\n",
      "spec_file = \"spec.md\"\n",
      "max_cycles = 25\n",
      "max_repair_attempts = 3\n",
      "[agent]\n",
      "id = \"test-agent\"\n",
      "[verification]\n",
      "timeout_secs = 300\n",
      "max_output_bytes = 65536\n",
      "[[verification.checks]]\n",
      "name = \"tests\"\n",
      "command = [\"./test\", \"--all\"]\n",
      "working_directory = \"project\"\n",
      "environment = { CI = \"true\" }\n",
      "timeout_secs = 600\n",
    );

    let config: Config = toml::from_str(text).expect("structured project check");
    let check = &config.verification.checks[0];

    assert_eq!(check.command, ["./test", "--all"]);
    assert_eq!(check.working_directory, "project");
    assert_eq!(check.environment["CI"], "true");
    assert_eq!(
      check.effective_timeout_secs(config.verification.timeout_secs),
      600
    );
  }
  #[test]
  fn structured_trusted_check_deserializes_with_enforced_defaults() {
    let image = format!("example/verifier@sha256:{}", "a".repeat(64));
    let text = format!(
      "version = 1\nspec_file = \"spec.md\"\nmax_cycles = 25\nmax_repair_attempts = 3\n[agent]\nid = \"test-agent\"\n[[verification.trusted_checks]]\nname = \"expiry-boundary\"\nbackend = \"docker\"\nimage = \"{image}\"\nprogram = \"verify\"\nargs = [\"--expiry\"]\n"
    );

    let config: Config = toml::from_str(&text).expect("structured trusted check");
    let check = &config.verification.trusted_checks[0];

    assert_eq!(check.name, "expiry-boundary");
    assert_eq!(check.args, ["--expiry"]);
    assert_eq!(check.isolation, Default::default());
  }

  #[test]
  fn duplicate_trusted_check_names_are_rejected() {
    let image = format!("example/verifier@sha256:{}", "a".repeat(64));
    let check = format!(
      "[[verification.trusted_checks]]\nname = \"expiry-boundary\"\nbackend = \"docker\"\nimage = \"{image}\"\nprogram = \"verify\"\n"
    );
    let text = format!(
      "version = 1\nspec_file = \"spec.md\"\nmax_cycles = 25\nmax_repair_attempts = 3\n[agent]\nid = \"test-agent\"\n{check}{check}"
    );

    let error = toml::from_str::<Config>(&text).expect_err("duplicate name");

    assert!(error
      .to_string()
      .contains("duplicate trusted verifier name"));
  }
  #[test]
  fn unknown_trusted_verifier_backend_is_rejected() {
    let image = format!("example/verifier@sha256:{}", "a".repeat(64));
    let text = format!(
      "version = 1\nspec_file = \"spec.md\"\nmax_cycles = 25\nmax_repair_attempts = 3\n[agent]\nid = \"test-agent\"\n[[verification.trusted_checks]]\nname = \"expiry-boundary\"\nbackend = \"host\"\nimage = \"{image}\"\nprogram = \"verify\"\n"
    );

    let error = toml::from_str::<Config>(&text).expect_err("unknown backend");

    assert!(error.to_string().contains("unknown variant `host`"));
  }

  #[test]
  fn project_suite_hash_changes_with_effective_configuration() {
    let mut config = super::VerificationConfig::default();
    config.checks.push(super::ProjectVerificationCheck {
      name: "tests".into(),
      command: vec!["./test".into()],
      working_directory: ".".into(),
      environment: Default::default(),
      timeout_secs: None,
    });
    let original = config.suite_hash().expect("suite hash");
    config.checks[0]
      .environment
      .insert("CI".into(), "true".into());

    assert_ne!(config.suite_hash().expect("changed suite hash"), original);
  }

  #[test]
  fn project_check_name_does_not_change_suite_hash() {
    let mut config = super::VerificationConfig::default();
    config.checks.push(super::ProjectVerificationCheck {
      name: "tests".into(),
      command: vec!["./test".into()],
      working_directory: ".".into(),
      environment: Default::default(),
      timeout_secs: None,
    });
    let original = config.suite_hash().expect("suite hash");
    config.checks[0].name = "diagnostic rename".into();

    assert_eq!(config.suite_hash().expect("renamed suite hash"), original);
  }

  #[test]
  fn required_role_preference_field_is_rejected() {
    let default_preference =
      serialized_config(|_| {}, "\n[agent.preferences.default]\nrequired = true\n");
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
    assert!(!loaded.agent.thinking_for(WorkerRole::Architect).is_empty());
    assert_eq!(loaded.agent.turn_timeout_secs, 900);
    assert!(loaded.agent.model_for(WorkerRole::Architect).is_some());
  }

  #[tokio::test]
  async fn unsupported_configuration_version_is_rejected() {
    let project = tempdir().unwrap();
    let text = serialized_config(|config| config.version = 99, "");
    tokio::fs::write(config_path(project.path()), text)
      .await
      .unwrap();

    let error = read_config(project.path()).await.unwrap_err().to_string();

    assert!(error.contains("unsupported configuration version 99"));
    assert!(error.contains("supported version is 1"));
  }

  #[tokio::test]
  async fn unsafe_additional_protected_path_is_rejected() {
    let text = serialized_config(
      |config| config.additional_protected_paths = vec!["../outside".into()],
      "",
    );
    let error = read_error(text).await;

    assert!(error.contains("invalid additional_protected_paths entry"));
    assert!(error.contains("escapes the repository"));
  }

  #[tokio::test]
  async fn zero_stagnation_limit_is_rejected() {
    let project = tempdir().unwrap();
    let text = serialized_config(|config| config.stagnation_limit = 0, "");
    tokio::fs::write(config_path(project.path()), text)
      .await
      .unwrap();

    let error = read_config(project.path()).await.unwrap_err().to_string();

    assert!(error.contains("stagnation_limit must be at least 1"));
  }
}
