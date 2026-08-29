use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerifierAuthority {
  Project,
  AuthoritySnapshot,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentMode {
  #[default]
  Ambient,
  Declared,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateCapturePolicy {
  #[serde(default = "default_candidate_root")]
  #[schemars(description = "Safe project-relative root whose contents form Candidate Snapshot R.")]
  pub root: String,
  #[serde(default = "default_candidate_exclusions")]
  #[schemars(
    description = "Deterministic project-relative exclusion rules. A trailing `/**` excludes a directory and all descendants."
  )]
  pub exclude: Vec<String>,
}

impl Default for CandidateCapturePolicy {
  fn default() -> Self {
    Self {
      root: default_candidate_root(),
      exclude: default_candidate_exclusions(),
    }
  }
}

impl CandidateCapturePolicy {
  pub fn excludes(&self, path: &str) -> bool {
    let path = path.replace('\\', "/");
    path == ".tenet"
      || path.starts_with(".tenet/")
      || self.exclude.iter().any(|rule| {
        rule.strip_suffix("/**").map_or(rule == &path, |prefix| {
          path == prefix
            || (path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'/'))
        })
      })
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
  pub version: u32,
  #[schemars(description = "Safe project-relative path to the authority specification.")]
  pub spec_path: String,
  #[serde(default)]
  #[schemars(description = "Authority-defined Candidate Snapshot capture boundary.")]
  pub candidate: CandidateCapturePolicy,
  #[serde(default)]
  #[schemars(description = "Verifier definitions sealed into an Authority Capsule.")]
  pub verifiers: Vec<VerifierSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifierSpec {
  pub id: String,
  #[schemars(
    description = "Structured argv. For authority_snapshot, argv[0] directly names an executable inside oracle_path; Tenet never invokes a host interpreter implicitly."
  )]
  pub argv: Vec<String>,
  #[serde(default = "default_cwd")]
  #[schemars(
    description = "Safe relative working directory. For authority_snapshot it is relative to the sealed oracle bundle."
  )]
  pub cwd: String,
  #[serde(default = "default_timeout")]
  pub timeout_seconds: u64,
  #[serde(default = "default_output_limit")]
  pub max_output_bytes: usize,
  #[serde(default)]
  pub env: BTreeMap<String, String>,
  #[serde(default)]
  pub environment_mode: EnvironmentMode,
  #[schemars(
    description = "project executes in Candidate Snapshot R. authority_snapshot executes from a sealed Authority Capsule A bundle and receives R only through TENET_CANDIDATE_ROOT."
  )]
  pub authority: VerifierAuthority,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(
    description = "Required only for authority_snapshot: safe project-relative directory to seal as the authority-owned oracle bundle."
  )]
  pub oracle_path: Option<String>,
}
impl VerifierSpec {
  pub fn oracle_executable_path(&self) -> Option<std::path::PathBuf> {
    let normalize = |value: &str| {
      std::path::Path::new(value)
        .components()
        .filter(|component| *component != std::path::Component::CurDir)
        .collect::<std::path::PathBuf>()
    };
    Some(normalize(self.oracle_path.as_deref()?).join(normalize(self.argv.first()?)))
  }
}

pub type VerificationPolicy = ProjectConfig;
fn default_candidate_root() -> String {
  ".".into()
}

fn default_candidate_exclusions() -> Vec<String> {
  vec![
    ".tenet/**".into(),
    ".mcp.json".into(),
    ".agents/**".into(),
    ".git/**".into(),
    ".hg/**".into(),
    ".svn/**".into(),
  ]
}

fn valid_candidate_rule(value: &str) -> bool {
  let base = value.strip_suffix("/**").unwrap_or(value);
  !(base == "."
    || base.contains('*')
    || base.contains("//")
    || base.starts_with("./")
    || base.contains("/./")
    || base.ends_with("/.")
    || base.ends_with('/')
    || !is_safe_relative_path(base))
}

fn valid_project_executable(value: &str) -> bool {
  !value.trim().is_empty() && !value.contains('\0')
}

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
  #[error("unsupported project configuration version {0}")]
  UnsupportedVersion(u32),
  #[error("spec_path must name a project-relative path")]
  InvalidSpecPath,
  #[error("candidate root must name a safe project-relative directory")]
  InvalidCandidateRoot,
  #[error("candidate exclusion rule `{0}` is invalid")]
  InvalidCandidateExclusion(String),
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
  #[error("project verifier `{0}` must not configure oracle_path")]
  UnexpectedOraclePath(String),
  #[error("authority_snapshot verifier `{0}` must configure a project-relative oracle_path")]
  InvalidOraclePath(String),
  #[error("verifier `{0}` executable must be a safe relative path")]
  InvalidExecutable(String),
}

pub fn validate_policy(policy: &VerificationPolicy) -> Result<(), PolicyError> {
  if policy.version != 1 {
    return Err(PolicyError::UnsupportedVersion(policy.version));
  }
  if !is_safe_relative_path(&policy.spec_path) {
    return Err(PolicyError::InvalidSpecPath);
  }
  if !is_safe_relative_path(&policy.candidate.root)
    || policy.candidate.root == ".tenet"
    || policy.candidate.root.starts_with(".tenet/")
  {
    return Err(PolicyError::InvalidCandidateRoot);
  }
  if policy
    .candidate
    .exclude
    .iter()
    .any(|rule| !valid_candidate_rule(rule))
  {
    let rule = policy
      .candidate
      .exclude
      .iter()
      .find(|rule| !valid_candidate_rule(rule))
      .map(String::as_str)
      .unwrap_or("<invalid>");
    return Err(PolicyError::InvalidCandidateExclusion(rule.into()));
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
      .is_none_or(|item| !valid_project_executable(item))
    {
      return Err(PolicyError::EmptyArgv(verifier.id.clone()));
    }
    if verifier.authority == VerifierAuthority::AuthoritySnapshot
      && !is_safe_relative_path(&verifier.argv[0])
    {
      return Err(PolicyError::InvalidExecutable(verifier.id.clone()));
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
    match verifier.authority {
      VerifierAuthority::Project if verifier.oracle_path.is_some() => {
        return Err(PolicyError::UnexpectedOraclePath(verifier.id.clone()));
      }
      VerifierAuthority::Project => {}
      VerifierAuthority::AuthoritySnapshot => {
        let valid_oracle_path = verifier.oracle_path.as_deref().is_some_and(|path| {
          path != "."
            && path != ".tenet"
            && !path.starts_with(".tenet/store")
            && is_safe_relative_path(path)
        });
        if !valid_oracle_path {
          return Err(PolicyError::InvalidOraclePath(verifier.id.clone()));
        }
      }
    }
  }
  Ok(())
}

fn is_safe_relative_path(value: &str) -> bool {
  let path = std::path::Path::new(value);
  !value.trim().is_empty()
    && !value.contains('\\')
    && !path.is_absolute()
    && path
      .components()
      .all(|component| !matches!(component, std::path::Component::ParentDir))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn policy(candidate: CandidateCapturePolicy) -> ProjectConfig {
    ProjectConfig {
      version: 1,
      spec_path: "SPEC.md".into(),
      candidate,
      verifiers: Vec::new(),
    }
  }

  #[test]
  fn candidate_exclusions_match_only_declared_relative_rules() {
    let candidate = CandidateCapturePolicy {
      root: ".".into(),
      exclude: vec!["build/**".into(), "notes.txt".into()],
    };
    assert!(candidate.excludes(".tenet/state.json"));
    assert!(candidate.excludes("build/cache/result"));
    assert!(candidate.excludes("notes.txt"));
    assert!(!candidate.excludes("build-output/result"));
    assert!(!candidate.excludes("notes.txt.bak"));
  }

  #[test]
  fn candidate_boundary_rejects_unsafe_paths_and_rules() {
    assert_eq!(
      validate_policy(&policy(CandidateCapturePolicy {
        root: ".tenet".into(),
        ..CandidateCapturePolicy::default()
      })),
      Err(PolicyError::InvalidCandidateRoot)
    );
    assert_eq!(
      validate_policy(&policy(CandidateCapturePolicy {
        exclude: vec!["../outside/**".into()],
        ..CandidateCapturePolicy::default()
      })),
      Err(PolicyError::InvalidCandidateExclusion(
        "../outside/**".into()
      ))
    );
    assert_eq!(
      validate_policy(&policy(CandidateCapturePolicy {
        exclude: vec!["./build/**".into()],
        ..CandidateCapturePolicy::default()
      })),
      Err(PolicyError::InvalidCandidateExclusion("./build/**".into()))
    );
    assert_eq!(
      validate_policy(&policy(CandidateCapturePolicy {
        exclude: vec!["build//cache".into()],
        ..CandidateCapturePolicy::default()
      })),
      Err(PolicyError::InvalidCandidateExclusion(
        "build//cache".into()
      ))
    );
    assert_eq!(
      validate_policy(&policy(CandidateCapturePolicy {
        exclude: vec![".".into()],
        ..CandidateCapturePolicy::default()
      })),
      Err(PolicyError::InvalidCandidateExclusion(".".into()))
    );
  }
}
