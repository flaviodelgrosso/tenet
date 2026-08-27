use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
  ids::{ObligationId, VerificationRunId},
  proof::{ArtifactObservation, ExecutionObservation},
  trusted_verifier::{IsolationCapabilityReport, TrustedExecutionResult, TrustedVerificationSpec},
};

const DEFAULT_MAX_INPUT_BYTES: usize = 16 * 1024;
const MAX_INPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FalsifierProtocol {
  #[default]
  ExitCode,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StructuredFalsifierInputSpec {
  pub schema: Value,
  pub argument: String,
  #[serde(default = "default_max_input_bytes", rename = "maxBytes")]
  pub max_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FalsifierSpec {
  #[serde(flatten)]
  pub execution: TrustedVerificationSpec,
  #[serde(default, rename = "result_protocol")]
  pub protocol: FalsifierProtocol,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub input: Option<StructuredFalsifierInputSpec>,
}

impl FalsifierSpec {
  pub fn validate(&self) -> Result<(), FalsifierSpecError> {
    self
      .execution
      .validate()
      .map_err(|error| FalsifierSpecError::InvalidExecution(error.to_string()))?;
    if let Some(input) = &self.input {
      if !input.schema.is_object() {
        return Err(FalsifierSpecError::InvalidInputSchema);
      }
      if input.argument.trim().is_empty()
        || input
          .argument
          .bytes()
          .any(|byte| byte.is_ascii_whitespace())
      {
        return Err(FalsifierSpecError::InvalidInputArgument);
      }
      if !(1..=MAX_INPUT_BYTES).contains(&input.max_bytes) {
        return Err(FalsifierSpecError::InvalidInputLimit);
      }
    }
    Ok(())
  }

  pub fn name(&self) -> &str {
    &self.execution.name
  }

  pub fn fingerprint(&self) -> Result<String, serde_json::Error> {
    fingerprint("tenet-falsifier-spec-v1", self)
  }

  pub fn execution_spec(
    &self,
    admitted_input: Option<&Value>,
  ) -> Result<TrustedVerificationSpec, FalsifierSpecError> {
    self.validate()?;
    let mut execution = self.execution.clone();
    match (&self.input, admitted_input) {
      (None, None) => {}
      (Some(input_spec), Some(value)) => {
        let encoded = serde_json::to_string(value)
          .map_err(|error| FalsifierSpecError::InvalidInput(error.to_string()))?;
        if encoded.len() > input_spec.max_bytes {
          return Err(FalsifierSpecError::InputTooLarge);
        }
        execution.args.push(input_spec.argument.clone());
        execution.args.push(encoded);
      }
      (Some(_), None) => return Err(FalsifierSpecError::MissingInput),
      (None, Some(_)) => return Err(FalsifierSpecError::UnexpectedInput),
    }
    Ok(execution)
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FalsifierResult {
  CounterexampleFound,
  NoCounterexampleFound,
  InfrastructureFailure,
}

impl FalsifierResult {
  pub fn authoritative_observation(&self) -> Option<ArtifactObservation> {
    match self {
      Self::CounterexampleFound => Some(ArtifactObservation::Contradicts),
      Self::NoCounterexampleFound => Some(ArtifactObservation::Supports),
      Self::InfrastructureFailure => None,
    }
  }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FalsificationExecutionRecord {
  pub id: VerificationRunId,
  pub revision: String,
  #[serde(rename = "inputMaterializationHash")]
  pub input_materialization_hash: String,
  #[serde(rename = "falsifierName")]
  pub falsifier_name: String,
  #[serde(rename = "specHash")]
  pub spec_hash: String,
  #[serde(rename = "isolationPolicyHash")]
  pub isolation_policy_hash: String,
  #[serde(rename = "imageDigest")]
  pub image_digest: String,
  #[serde(rename = "admittedInput", skip_serializing_if = "Option::is_none")]
  pub admitted_input: Option<Value>,
  #[serde(rename = "admittedInputHash")]
  pub admitted_input_hash: String,
  #[serde(rename = "isolationReport")]
  pub isolation_report: IsolationCapabilityReport,
  #[serde(rename = "startedAt")]
  pub started_at: DateTime<Utc>,
  #[serde(rename = "finishedAt")]
  pub finished_at: DateTime<Utc>,
  pub result: FalsifierResult,
  pub observation: ExecutionObservation,
  #[serde(rename = "obligationIds")]
  pub obligation_ids: Vec<ObligationId>,
}

impl FalsificationExecutionRecord {
  pub fn from_trusted_execution(
    trusted: crate::trusted_verifier::TrustedExecutionRecord,
    spec: &FalsifierSpec,
    admitted_input: Option<Value>,
  ) -> Result<Self, FalsifierRecordError> {
    let execution = spec
      .execution_spec(admitted_input.as_ref())
      .map_err(|error| FalsifierRecordError::InvalidSpec(error.to_string()))?;
    if !trusted.can_issue_authority(&execution) {
      return Err(FalsifierRecordError::UntrustedExecution);
    }
    let isolation_report = trusted
      .isolation_report
      .ok_or(FalsifierRecordError::MissingIsolationReport)?;
    let image_digest = execution
      .image_digest()
      .map_err(|error| FalsifierRecordError::InvalidSpec(error.to_string()))?
      .as_str()
      .to_owned();
    Ok(Self {
      id: trusted.id,
      revision: trusted.revision,
      input_materialization_hash: trusted.input_materialization_hash,
      falsifier_name: spec.name().into(),
      spec_hash: spec
        .fingerprint()
        .map_err(|error| FalsifierRecordError::InvalidSpec(error.to_string()))?,
      isolation_policy_hash: execution
        .isolation_policy_hash()
        .map_err(|error| FalsifierRecordError::InvalidSpec(error.to_string()))?,
      image_digest,
      admitted_input_hash: admitted_input_hash(admitted_input.as_ref())
        .map_err(|error| FalsifierRecordError::InvalidSpec(error.to_string()))?,
      admitted_input,
      isolation_report,
      started_at: trusted.started_at,
      finished_at: trusted.finished_at,
      result: classify_falsifier_result(&trusted.result),
      observation: trusted.observation,
      obligation_ids: trusted.obligation_ids,
    })
  }
}

impl FalsificationExecutionRecord {
  pub fn record_hash(&self) -> Result<String, serde_json::Error> {
    fingerprint("tenet-falsification-execution-record-v1", self)
  }

  pub fn can_issue_authority(&self, spec: &FalsifierSpec) -> bool {
    let Ok(execution) = spec.execution_spec(self.admitted_input.as_ref()) else {
      return false;
    };
    !self.revision.trim().is_empty()
      && !self.input_materialization_hash.trim().is_empty()
      && self.falsifier_name == spec.name()
      && spec.fingerprint().ok().as_deref() == Some(self.spec_hash.as_str())
      && execution.isolation_policy_hash().ok().as_deref()
        == Some(self.isolation_policy_hash.as_str())
      && execution
        .image_digest()
        .ok()
        .as_ref()
        .map(|digest| digest.as_str())
        == Some(self.image_digest.as_str())
      && admitted_input_hash(self.admitted_input.as_ref())
        .ok()
        .as_deref()
        == Some(self.admitted_input_hash.as_str())
      && self.isolation_report.satisfies(&execution)
      && self.isolation_report.input_revision == self.revision
      && self.isolation_report.input_materialization_hash == self.input_materialization_hash
      && matches!(
        (
          &self.result,
          self.observation.exit_code,
          self.observation.timed_out,
        ),
        (FalsifierResult::NoCounterexampleFound, Some(0), false)
          | (FalsifierResult::CounterexampleFound, Some(1), false)
      )
      && !self.obligation_ids.is_empty()
  }
}
pub fn classify_falsifier_result(result: &TrustedExecutionResult) -> FalsifierResult {
  match result {
    TrustedExecutionResult::Supports => FalsifierResult::NoCounterexampleFound,
    TrustedExecutionResult::Contradicts { .. } => FalsifierResult::CounterexampleFound,
    TrustedExecutionResult::TimedOut | TrustedExecutionResult::InfrastructureFailure { .. } => {
      FalsifierResult::InfrastructureFailure
    }
  }
}

pub fn admitted_input_hash(value: Option<&Value>) -> Result<String, serde_json::Error> {
  let bytes = value
    .map(serde_json::to_vec)
    .transpose()?
    .unwrap_or_default();
  let mut digest = Sha256::new();
  digest.update(b"tenet-falsifier-input-v1");
  digest.update((bytes.len() as u64).to_be_bytes());
  digest.update(bytes);
  let digest = digest.finalize();
  let mut encoded = String::with_capacity(digest.len() * 2);
  for byte in digest {
    write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
  }
  Ok(encoded)
}

fn default_max_input_bytes() -> usize {
  DEFAULT_MAX_INPUT_BYTES
}

fn fingerprint<T: Serialize>(domain: &str, value: &T) -> Result<String, serde_json::Error> {
  let encoded = serde_json::to_vec(&(domain, value))?;
  let digest = Sha256::digest(encoded);
  let mut output = String::with_capacity(digest.len() * 2);
  for byte in digest {
    write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
  }
  Ok(output)
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FalsifierSpecError {
  #[error("invalid falsifier execution specification: {0}")]
  InvalidExecution(String),
  #[error("falsifier input schema must be a JSON object")]
  InvalidInputSchema,
  #[error("falsifier input argument must be one non-blank token")]
  InvalidInputArgument,
  #[error("falsifier input size limit is outside controller bounds")]
  InvalidInputLimit,
  #[error("configured falsifier requires structured input")]
  MissingInput,
  #[error("configured falsifier does not accept structured input")]
  UnexpectedInput,
  #[error("falsifier input exceeds configured size limit")]
  InputTooLarge,
  #[error("falsifier input could not be encoded: {0}")]
  InvalidInput(String),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FalsifierRecordError {
  #[error("invalid falsifier specification: {0}")]
  InvalidSpec(String),
  #[error("sandbox execution did not satisfy the configured falsifier boundary")]
  UntrustedExecution,
  #[error("sandbox execution omitted its isolation capability report")]
  MissingIsolationReport,
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use serde_json::json;

  use super::*;
  use crate::trusted_verifier::{TrustedExecutionBackend, TrustedVerifierProtocol};

  fn spec() -> FalsifierSpec {
    FalsifierSpec {
      execution: TrustedVerificationSpec {
        name: "boundary-search".into(),
        backend: TrustedExecutionBackend::Microsandbox,
        image: format!("example/falsifier@sha256:{}", "a".repeat(64)),
        program: "/bin/search".into(),
        args: vec!["--bounded".into()],
        working_directory: ".".into(),
        environment: BTreeMap::new(),
        timeout_secs: 30,
        isolation: Default::default(),
        resources: Default::default(),
        protocol: TrustedVerifierProtocol::ExitCode,
      },
      protocol: FalsifierProtocol::ExitCode,
      input: Some(StructuredFalsifierInputSpec {
        schema: json!({"type": "object", "required": ["seed"]}),
        argument: "--input-json".into(),
        max_bytes: 128,
      }),
    }
  }

  #[test]
  fn execution_program_and_image_remain_controller_configured() {
    let spec = spec();
    let execution = spec
      .execution_spec(Some(&json!({"seed": 7})))
      .expect("effective execution");

    assert_eq!(execution.program, "/bin/search");
    assert_eq!(execution.image, spec.execution.image);
    assert_eq!(
      execution.args.last().map(String::as_str),
      Some("{\"seed\":7}")
    );
  }

  #[test]
  fn unexpected_dynamic_input_is_rejected() {
    let mut spec = spec();
    spec.input = None;

    assert_eq!(
      spec.execution_spec(Some(&json!({"seed": 7}))).unwrap_err(),
      FalsifierSpecError::UnexpectedInput
    );
  }

  #[test]
  fn infrastructure_failure_has_no_authoritative_observation() {
    let result = classify_falsifier_result(&TrustedExecutionResult::InfrastructureFailure {
      message: "sandbox unavailable".into(),
    });

    assert_eq!(result.authoritative_observation(), None);
  }
}
