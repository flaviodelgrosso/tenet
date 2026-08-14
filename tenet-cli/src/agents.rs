use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tenet_acp::{
  acp::{AcpAuthMechanism, AcpReadiness, AcpRuntime},
  registry::{LaunchMetadata, RegistryClient, RegistryIndex},
};

use crate::cli::AgentCommand;

struct AcpLaunch {
  registry: Option<LaunchMetadata>,
  custom: Option<tenet_domain::config::CustomAgentConfig>,
}

impl AcpLaunch {
  fn registry(launch: LaunchMetadata) -> Self {
    Self {
      registry: Some(launch),
      custom: None,
    }
  }

  fn custom(custom: tenet_domain::config::CustomAgentConfig) -> Self {
    Self {
      registry: None,
      custom: Some(custom),
    }
  }

  async fn readiness(&self) -> Result<AcpReadiness> {
    AcpRuntime
      .readiness(self.registry.clone(), self.custom.clone())
      .await
  }

  async fn login(self, method_id: &str) -> Result<()> {
    AcpRuntime
      .login(self.registry, self.custom, method_id)
      .await
      .map(|_| ())
  }
}

pub(crate) async fn handle(cwd: &Path, command: Option<AgentCommand>) -> Result<bool> {
  match command.unwrap_or(AgentCommand::List) {
    AgentCommand::List => list_registry_agents(cwd, None).await?,
    AgentCommand::Search { query } => list_registry_agents(cwd, Some(&query)).await?,
    AgentCommand::Select { id } => select_registry_agent(cwd, &id).await?,
    AgentCommand::Setup { id, yes } => setup_registry_agent(cwd, id.as_deref(), yes).await?,
    AgentCommand::Doctor => return doctor(cwd).await,
    AgentCommand::Login { method } => login(cwd, method.as_deref()).await?,
  }
  Ok(true)
}

fn registry_cache_dir(cwd: &Path) -> PathBuf {
  cwd.join(".tenet").join("registry")
}

async fn load_registry_index(cwd: &Path) -> Result<RegistryIndex> {
  let cache = registry_cache_dir(cwd);
  RegistryClient::default().load_index(&cache).await.with_context(|| {
    format!(
      "unable to load ACP Registry (cache: {}); next action: restore Registry access, retry, or configure agent.custom",
      cache.display()
    )
  })
}

async fn list_registry_agents(cwd: &Path, query: Option<&str>) -> Result<()> {
  let index = load_registry_index(cwd).await?;
  let query = query
    .map(str::trim)
    .filter(|query| !query.is_empty())
    .map(str::to_lowercase);
  let mut matched = 0usize;
  for agent in index.agents {
    if query.as_ref().is_none_or(|query| {
      agent.name.to_lowercase().contains(query) || agent.id.to_lowercase().contains(query)
    }) {
      println!("{} — {}", agent.name, agent.id);
      matched += 1;
    }
  }
  if matched == 0 {
    if let Some(query) = query {
      println!("No ACP Registry agents match {query:?}.");
      println!("Next: run `tenet agents list` or choose an ID from a refreshed Registry.");
    } else {
      println!("The ACP Registry returned no agents.");
      println!("Next: retry after refreshing Registry access or configure agent.custom.");
    }
  }
  Ok(())
}

async fn select_registry_agent(cwd: &Path, id: &str) -> Result<()> {
  let id = id.trim();
  if id.is_empty() {
    anyhow::bail!("Registry agent ID cannot be empty; next action: run `tenet agents list`");
  }

  let index = load_registry_index(cwd).await?;
  let selected = index.agents.iter().find(|agent| agent.id == id).ok_or_else(|| {
    anyhow::anyhow!("unknown Registry agent {id:?}; next action: run `tenet agents list` or `tenet agents search <query>`")
  })?;

  let path = tenet_domain::config::config_path(cwd);
  if !path.exists() {
    tenet_domain::config::ensure_config(cwd).await?;
  }

  let text = tokio::fs::read_to_string(&path)
    .await
    .with_context(|| format!("read {}", path.display()))?;

  let mut document: toml::Value =
    toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;

  let agent = document
    .as_table_mut()
    .and_then(|config| config.get_mut("agent"))
    .and_then(toml::Value::as_table_mut)
    .ok_or_else(|| anyhow::anyhow!("invalid tenet.toml: [agent] must be a table"))?;

  let configured_id = match agent.get("id") {
    None => None,
    Some(toml::Value::String(value)) if value.trim().is_empty() => None,
    Some(toml::Value::String(_)) => Some(()),
    Some(_) => anyhow::bail!("invalid tenet.toml: agent.id must be a non-empty string"),
  };

  if agent.contains_key("custom") {
    if configured_id.is_some() {
      anyhow::bail!(
        "ambiguous ACP launch source: choose either agent.id or [agent.custom], not both"
      );
    }
    anyhow::bail!("cannot select a Registry agent while [agent.custom] is configured; remove agent.custom first so exactly one ACP launch source remains");
  }

  agent.insert("id".into(), toml::Value::String(id.to_owned()));
  let body = toml::to_string_pretty(&document).context("serialize tenet.toml")?;

  tokio::fs::write(
    &path,
    format!(
      "#:schema {}\n\n{body}",
      tenet_domain::config::CONFIG_SCHEMA_URL
    ),
  )
  .await
  .with_context(|| format!("write {}", path.display()))?;

  println!(
    "Selected ACP Registry agent: {} — {}",
    selected.name, selected.id
  );
  println!("Updated {}", path.display());
  println!("Next: run `tenet agents doctor` to check launch readiness.");
  Ok(())
}

async fn setup_registry_agent(cwd: &Path, requested_id: Option<&str>, yes: bool) -> Result<()> {
  let id = match requested_id {
    Some(id) if id.trim().is_empty() => anyhow::bail!("Registry agent ID must not be blank"),
    Some(id) => id.trim().to_owned(),
    None => selected_registry_id(cwd).await?,
  };

  if !yes {
    println!("Registry ID: {id}");
    println!("Confirmation required: run `tenet agents setup {id} --yes` to install its binary distribution.");
    return Ok(());
  }

  let installed = RegistryClient::default()
    .setup_binary(&registry_cache_dir(cwd), &id)
    .await
    .with_context(|| format!("Registry binary setup failed for {id:?}"))?;

  println!("name: {}", installed.display_name);
  println!("id: {}", installed.id);
  println!("provenance: {}", installed.launch.provenance);
  Ok(())
}

async fn selected_registry_id(cwd: &Path) -> Result<String> {
  let config = tenet_domain::config::read_config(cwd).await?;
  match (config.agent.id.as_deref().filter(|id| !id.trim().is_empty()), config.agent.custom.as_ref()) {
    (Some(id), None) => Ok(id.to_owned()),
    (Some(_), Some(_)) => anyhow::bail!("ambiguous ACP launch source: choose either agent.id or [agent.custom], not both"),
    (None, Some(_)) => anyhow::bail!("no Registry agent is selected; pass an ID to `tenet agents setup <id> --yes`"),
    (None, None) => anyhow::bail!("no Registry agent is selected; run `tenet agents select <id>` or pass an ID to `tenet agents setup <id> --yes`"),
  }
}

async fn doctor(cwd: &Path) -> Result<bool> {
  let path = tenet_domain::config::config_path(cwd);
  if !path.exists() {
    println!(
      "configured source: none ({} does not exist)",
      path.display()
    );
    println!("ACP preflight: NOT READY");
    println!(
      "Next: run `tenet init`, then `tenet agents select <id>` or configure [agent.custom]."
    );
    return Ok(false);
  }

  let config = match tenet_domain::config::read_config(cwd).await {
    Ok(config) => config,
    Err(error) => {
      println!("configured source: invalid");
      println!("configuration error: {error:#}");
      println!("ACP preflight: NOT READY");
      println!(
        "Next: configure exactly one of agent.id or [agent.custom] in {}.",
        path.display()
      );
      return Ok(false);
    }
  };

  print_preferences(&config.agent.preferences);

  match (
    config
      .agent
      .id
      .as_deref()
      .filter(|id| !id.trim().is_empty()),
    config.agent.custom.as_ref(),
  ) {
    (Some(_), Some(_)) => {
      println!("configured source: ambiguous (Registry and custom ACP)");
      println!("ACP preflight: NOT READY");
      println!("Next: configure exactly one of agent.id or [agent.custom].");
      Ok(false)
    }
    (None, None) => {
      println!("configured source: none");
      println!("ACP preflight: NOT READY");
      println!("Next: run `tenet agents select <id>` or configure [agent.custom].");
      Ok(false)
    }
    (Some(id), None) => doctor_registry_agent(cwd, id).await,
    (None, Some(custom)) => {
      println!("configured source: custom ACP command");
      println!("install provenance: tenet.toml [agent.custom]");
      if !print_command_preflight(&custom.command) {
        return Ok(false);
      }
      doctor_acp(AcpLaunch::custom(custom.clone())).await
    }
  }
}

fn print_preferences(preferences: &tenet_domain::config::AgentPreferences) {
  let configured = |preference: &tenet_domain::config::RolePreference| {
    let mut values = Vec::new();
    if preference.model.is_some() {
      values.push("model");
    }
    if preference.thought_level.is_some() {
      values.push("thought_level");
    }
    if preference.mode.is_some() {
      values.push("mode");
    }
    values.join(",")
  };

  println!("preferences.default: {}", configured(&preferences.default));
  for (role, preference) in &preferences.roles {
    println!("preferences.role.{role}: {}", configured(preference));
  }
}

async fn doctor_registry_agent(cwd: &Path, id: &str) -> Result<bool> {
  println!("configured source: ACP Registry");
  println!("registry ID: {id}");

  let cache = registry_cache_dir(cwd);
  let resolved = match RegistryClient::default().resolve(&cache, id).await {
    Ok(resolved) => resolved,
    Err(error) if binary_installation_required(&error) => {
      println!("installation: REQUIRED");
      println!("ACP preflight: NOT READY");
      println!("Next: run `tenet agents setup {id} --yes`, then rerun `tenet agents doctor`.");
      return Ok(false);
    }
    Err(error) => {
      println!("Registry resolution: unavailable ({error:#})");
      println!("ACP preflight: NOT READY");
      println!("Next: restore Registry access, retry with cached metadata, or select a compatible Registry agent.");
      return Ok(false);
    }
  };

  println!("name: {}", resolved.display_name);
  println!("id: {}", resolved.id);
  println!("provenance: {}", resolved.launch.provenance);

  if Path::new(&resolved.launch.command).is_absolute() {
    println!("installation: INSTALLED");
  }

  if !print_command_preflight(&resolved.launch.command) {
    return Ok(false);
  }

  doctor_acp(AcpLaunch::registry(resolved.launch)).await
}

async fn doctor_acp(launch: AcpLaunch) -> Result<bool> {
  match launch.readiness().await {
    Ok(readiness) => {
      print_acp_readiness(&readiness);
      println!("ACP handshake: READY");
      Ok(true)
    }
    Err(error) => {
      println!("ACP handshake: NOT READY ({error:#})");
      println!(
        "Next: fix the ACP agent launch or authentication setup, then rerun `tenet agents doctor`."
      );
      Ok(false)
    }
  }
}

fn print_acp_readiness(readiness: &AcpReadiness) {
  match (&readiness.agent_name, &readiness.agent_version) {
    (Some(name), Some(version)) => println!("agentInfo: {name} {version}"),
    (Some(name), None) => println!("agentInfo: {name}"),
    _ => println!("agentInfo: not advertised"),
  }

  if readiness.auth_methods.is_empty() {
    println!("auth methods: none advertised");
    return;
  }

  println!("auth methods:");
  for method in &readiness.auth_methods {
    println!(
      "  {}: {} ({})",
      method.id,
      method.name,
      method.mechanism.label()
    );
  }
}

async fn login(cwd: &Path, requested_method: Option<&str>) -> Result<()> {
  let launch = configured_acp_launch(cwd).await?;
  let readiness = launch.readiness().await?;
  print_acp_readiness(&readiness);

  let Some(method_id) = requested_method else {
    if readiness.auth_methods.is_empty() {
      println!("No authentication method is advertised by this agent.");
    } else {
      println!("Next: run `tenet agents login <method-id>` for an agent-owned method.");
    }
    return Ok(());
  };

  let method = readiness
    .auth_methods
    .iter()
    .find(|method| method.id == method_id)
    .ok_or_else(|| {
      anyhow::anyhow!(
        "authentication method {method_id:?} was not advertised; choose an ID listed above"
      )
    })?;

  if method.mechanism != AcpAuthMechanism::Agent {
    anyhow::bail!("authentication method {method_id:?} requires {}; Tenet does not handle credentials or interactive terminal authentication. Follow the agent's documented authentication flow.", method.mechanism.label());
  }

  launch.login(method_id).await?;
  println!("Authentication request accepted by the agent.");
  println!("Next: complete any agent-owned authentication flow, then rerun `tenet agents doctor`.");
  Ok(())
}

async fn configured_acp_launch(cwd: &Path) -> Result<AcpLaunch> {
  let config = tenet_domain::config::read_config(cwd).await?;

  match (
    config
      .agent
      .id
      .as_deref()
      .filter(|id| !id.trim().is_empty()),
    config.agent.custom,
  ) {
    (Some(_), Some(_)) => anyhow::bail!(
      "ambiguous ACP launch source: choose either agent.id or [agent.custom], not both"
    ),
    (None, None) => anyhow::bail!(
      "no ACP launch source configured; run `tenet agents select <id>` or configure [agent.custom]"
    ),
    (None, Some(custom)) => Ok(AcpLaunch::custom(custom)),
    (Some(id), None) => {
      let resolved = RegistryClient::default()
        .resolve(&registry_cache_dir(cwd), id)
        .await
        .map_err(|error| {
          anyhow::anyhow!(
            "could not resolve Registry agent {id:?}; run `tenet agents doctor`: {error:#}"
          )
        })?;
      Ok(AcpLaunch::registry(resolved.launch))
    }
  }
}

fn binary_installation_required(error: &anyhow::Error) -> bool {
  error.chain().any(|cause| {
    cause
      .to_string()
      .contains("is not installed and checksum-verified")
  })
}

fn print_command_preflight(command: &str) -> bool {
  if command_available(command) {
    println!("ACP preflight: READY (launch is available)");
    true
  } else {
    println!("ACP preflight: NOT READY (configured launch is not available)");
    println!("Next: install the configured source, then rerun `tenet agents doctor`.");
    false
  }
}

fn command_available(command: &str) -> bool {
  let path = Path::new(command);
  if path.components().count() > 1 || path.is_absolute() {
    return path.is_file();
  }
  std::env::var_os("PATH").is_some_and(|paths| {
    std::env::split_paths(&paths).any(|directory| directory.join(command).is_file())
  })
}
