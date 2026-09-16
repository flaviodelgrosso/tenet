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

/// Enforcement level the runner must provide for a verifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerifierProtection {
  /// Unenforced execution; assurance is `LOCAL_V1` (detection only).
  #[default]
  Local,
  /// Enforced read-only Candidate/Authority views with separate writable
  /// scratch and controlled output; assurance is `PROTECTED_V1`. The runner
  /// must fail closed with an infrastructure result when the platform cannot
  /// enforce the boundary; it must never downgrade to `LOCAL_V1`.
  Protected,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentSpec {
  #[serde(default)]
  pub inherit: Vec<String>,
  #[serde(default)]
  pub set: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum CommandArgument {
  Literal(String),
  CandidatePath(String),
  AuthorityPath(String),
  ScratchPath(String),
}

impl CommandArgument {
  pub fn value(&self) -> &str {
    match self {
      Self::Literal(value)
      | Self::CandidatePath(value)
      | Self::AuthorityPath(value)
      | Self::ScratchPath(value) => value,
    }
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum CommandCwd {
  Candidate(String),
  Authority(String),
  Scratch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExitCodePolicy {
  pub pass: BTreeSet<i32>,
  pub fail: BTreeSet<i32>,
  pub inconclusive: BTreeSet<i32>,
}

impl Default for ExitCodePolicy {
  fn default() -> Self {
    Self {
      pass: BTreeSet::from([0]),
      fail: BTreeSet::from([1]),
      inconclusive: BTreeSet::from([125, 126]),
    }
  }
}

impl ExitCodePolicy {
  pub fn interpret(
    &self,
    exit_code: Option<i32>,
    timed_out: bool,
    infrastructure_failure: bool,
  ) -> crate::algebra::EvidenceResult {
    if timed_out || infrastructure_failure {
      return crate::algebra::EvidenceResult::InfrastructureError;
    }
    let Some(code) = exit_code else {
      return crate::algebra::EvidenceResult::InfrastructureError;
    };
    if self.pass.contains(&code) {
      crate::algebra::EvidenceResult::Pass
    } else if self.fail.contains(&code) {
      crate::algebra::EvidenceResult::Fail
    } else if self.inconclusive.contains(&code) {
      crate::algebra::EvidenceResult::Inconclusive
    } else {
      crate::algebra::EvidenceResult::InfrastructureError
    }
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandSpec {
  #[schemars(description = "Typed argv passed directly to the operating system process launcher.")]
  pub argv: Vec<CommandArgument>,
  pub cwd: CommandCwd,
  #[serde(default)]
  pub env: EnvironmentSpec,
  #[serde(default = "default_timeout_ms")]
  pub timeout_ms: u64,
  #[serde(default)]
  pub result: ExitCodePolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateCapturePolicy {
  #[serde(default = "default_candidate_root")]
  #[schemars(
    description = "Safe project-relative root under which candidate selectors are resolved."
  )]
  pub root: String,
  #[serde(default)]
  #[schemars(
    description = "Explicit positive Candidate Snapshot R surface. Selectors are exact paths, `path/to/directory/**`, or the explicit root selector `**`; an empty list means the surface is not configured."
  )]
  pub include: Vec<String>,
  #[serde(default)]
  #[schemars(
    description = "Selectors removed from the positive Candidate Snapshot R surface. These refine `include` and use the same exact-path or trailing-`/**` selector language."
  )]
  pub exclude: Vec<String>,
}

impl Default for CandidateCapturePolicy {
  fn default() -> Self {
    Self {
      root: default_candidate_root(),
      include: Vec::new(),
      exclude: Vec::new(),
    }
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
  pub command: CommandSpec,
  #[serde(default = "default_output_limit")]
  pub max_output_bytes: usize,
  #[schemars(
    description = "project executes from Candidate R. authority_snapshot executes from Authority A and receives R through typed paths and reserved runtime variables."
  )]
  pub authority: VerifierAuthority,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  #[schemars(
    description = "Required only for authority_snapshot: safe project-relative directory sealed as the authority-owned oracle bundle."
  )]
  pub oracle_path: Option<String>,
  #[serde(default)]
  #[schemars(
    description = "local executes without an enforcement boundary (LOCAL_V1). protected requires the runner to enforce read-only Candidate/Authority views, separate writable scratch, and controlled output (PROTECTED_V1), or fail closed."
  )]
  pub protection: VerifierProtection,
}

impl VerifierSpec {
  pub fn oracle_executable_path(&self) -> Option<std::path::PathBuf> {
    let CommandArgument::AuthorityPath(path) = self.command.argv.first()? else {
      return None;
    };
    Some(std::path::PathBuf::from(path))
  }
}

pub type VerificationPolicy = ProjectConfig;
fn default_candidate_root() -> String {
  ".".into()
}

fn default_timeout_ms() -> u64 {
  300_000
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
  #[error("candidate surface include must not be empty")]
  CandidateSurfaceUnconfigured,
  #[error("candidate include rule `{0}` is invalid")]
  InvalidCandidateInclusion(String),
  #[error("candidate exclude rule `{0}` is invalid")]
  InvalidCandidateExclusion(String),
  #[error("verifier identifier must not be blank")]
  BlankVerifierId,
  #[error("duplicate verifier identifier `{0}`")]
  DuplicateVerifier(String),
  #[error("verifier `{0}` has no executable argv")]
  EmptyArgv(String),
  #[error("verifier `{0}` has an invalid typed path")]
  InvalidCommandPath(String),
  #[error("verifier `{0}` has an invalid working directory")]
  InvalidCwd(String),
  #[error("verifier `{0}` timeout must be positive")]
  InvalidTimeout(String),
  #[error("verifier `{0}` output limit must be positive")]
  InvalidOutputLimit(String),
  #[error("verifier `{0}` exit-code sets must be disjoint")]
  OverlappingExitCodes(String),
  #[error("verifier `{0}` has an invalid environment variable name")]
  InvalidEnvironmentName(String),
  #[error("verifier `{0}` cannot override a reserved runtime variable")]
  ReservedEnvironmentName(String),
  #[error("project verifier `{0}` must not configure oracle_path")]
  UnexpectedOraclePath(String),
  #[error("authority_snapshot verifier `{0}` must configure a project-relative oracle_path")]
  InvalidOraclePath(String),
  #[error("authority_snapshot verifier `{0}` executable must use AuthorityPath inside oracle_path")]
  InvalidExecutable(String),
}
