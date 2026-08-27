use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerifierAuthority {
  Project,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
  pub version: u32,
  pub spec_path: String,
  #[serde(default)]
  pub verifiers: Vec<VerifierSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifierSpec {
  pub id: String,
  pub argv: Vec<String>,
  #[serde(default = "default_cwd")]
  pub cwd: String,
  #[serde(default = "default_timeout")]
  pub timeout_seconds: u64,
  #[serde(default = "default_output_limit")]
  pub max_output_bytes: usize,
  #[serde(default)]
  pub env: BTreeMap<String, String>,
  pub authority: VerifierAuthority,
}

pub type VerificationPolicy = RepositoryConfig;

fn default_cwd() -> String {
  ".".into()
}

fn default_timeout() -> u64 {
  300
}

fn default_output_limit() -> usize {
  65_536
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
  #[error("unsupported repository configuration version {0}")]
  UnsupportedVersion(u32),
  #[error("spec_path must name a repository-relative path")]
  InvalidSpecPath,
  #[error("verifier identifier must not be blank")]
  BlankVerifierId,
  #[error("duplicate verifier identifier `{0}`")]
  DuplicateVerifier(String),
  #[error("verifier `{0}` has no executable argv")]
  EmptyArgv(String),
  #[error("verifier `{0}` has an invalid working directory")]
  InvalidCwd(String),
  #[error("verifier `{0}` timeout must be positive")]
  InvalidTimeout(String),
  #[error("verifier `{0}` output limit must be positive")]
  InvalidOutputLimit(String),
}

pub fn validate_policy(policy: &VerificationPolicy) -> Result<(), PolicyError> {
  if policy.version != 1 {
    return Err(PolicyError::UnsupportedVersion(policy.version));
  }
  if !is_safe_relative_path(&policy.spec_path) {
    return Err(PolicyError::InvalidSpecPath);
  }
  let mut ids = BTreeSet::new();
  for verifier in &policy.verifiers {
    if verifier.id.trim().is_empty() {
      return Err(PolicyError::BlankVerifierId);
    }
    if !ids.insert(verifier.id.as_str()) {
      return Err(PolicyError::DuplicateVerifier(verifier.id.clone()));
    }
    if verifier
      .argv
      .first()
      .is_none_or(|item| item.trim().is_empty())
    {
      return Err(PolicyError::EmptyArgv(verifier.id.clone()));
    }
    if !is_safe_relative_path(&verifier.cwd) {
      return Err(PolicyError::InvalidCwd(verifier.id.clone()));
    }
    if verifier.timeout_seconds == 0 {
      return Err(PolicyError::InvalidTimeout(verifier.id.clone()));
    }
    if verifier.max_output_bytes == 0 {
      return Err(PolicyError::InvalidOutputLimit(verifier.id.clone()));
    }
  }
  Ok(())
}

fn is_safe_relative_path(value: &str) -> bool {
  let path = std::path::Path::new(value);
  !value.trim().is_empty()
    && !path.is_absolute()
    && path
      .components()
      .all(|component| !matches!(component, std::path::Component::ParentDir))
}
