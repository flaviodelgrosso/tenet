use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::fs;

pub const LOOPS_DIR: &str = ".loops";
pub const CONFIG_FILE: &str = "config.toml";

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub command: String,
    pub model: Option<String>,
    pub thinking: String,
    pub auto_approve: bool,
    pub turn_timeout_secs: u64,
    pub read_only_tools: Vec<String>,
    pub coding_tools: Vec<String>,
    pub extra_args: Vec<String>,
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
                ".loops/config.toml",
                ".loops/state.json",
                ".loops/requirements.json",
                ".loops/roadmap.json",
                ".loops/STOP",
                ".loops/run.lock",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        }
    }
}

pub fn config_path(cwd: &Path) -> PathBuf {
    cwd.join(LOOPS_DIR).join(CONFIG_FILE)
}

pub async fn ensure_config(cwd: &Path) -> Result<Config> {
    fs::create_dir_all(cwd.join(LOOPS_DIR)).await?;
    let path = config_path(cwd);
    if !path.exists() {
        let config = Config::default();
        let text = toml::to_string_pretty(&config)?;
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

    use super::{config_path, ensure_config};

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
    }
}
