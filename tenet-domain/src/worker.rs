use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
  error::DomainValidationError,
  evidence::{AcceptanceCriterion, ImplementationState, VerificationObligation},
  ids::{CriterionId, ObligationId, RequirementId, WorkUnitId},
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
  pub id: RequirementId,
  pub title: String,
  pub description: String,
  #[serde(default = "default_true")]
  pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequirementCatalog {
  #[serde(rename = "specHash")]
  pub spec_hash: String,
  pub requirements: Vec<Requirement>,
  #[serde(rename = "acceptanceCriteria")]
  pub acceptance_criteria: Vec<AcceptanceCriterion>,
  #[serde(rename = "verificationObligations")]
  pub verification_obligations: Vec<VerificationObligation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchitectOutput {
  pub requirements: Vec<Requirement>,
  #[serde(rename = "acceptanceCriteria")]
  pub acceptance_criteria: Vec<AcceptanceCriterion>,
  #[serde(rename = "verificationObligations")]
  pub verification_obligations: Vec<VerificationObligation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequirementAssessment {
  #[serde(rename = "requirementId")]
  pub requirement_id: RequirementId,
  #[serde(rename = "implementationState")]
  pub implementation_state: ImplementationState,
  pub observations: Vec<String>,
  #[serde(rename = "missingImplementation")]
  pub missing_implementation: Vec<String>,
  #[serde(rename = "missingEvidence")]
  pub missing_evidence: Vec<ObligationId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkScope {
  pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CandidateCheck {
  #[serde(rename = "obligationId")]
  pub obligation_id: ObligationId,
  pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkUnit {
  pub id: String,
  pub title: String,
  pub objective: String,
  #[serde(rename = "requirementIds")]
  pub requirement_ids: Vec<RequirementId>,
  #[serde(rename = "criterionIds")]
  pub criterion_ids: Vec<CriterionId>,
  #[serde(rename = "verificationObligationIds")]
  pub verification_obligation_ids: Vec<ObligationId>,
  #[serde(rename = "suggestedChecks")]
  pub suggested_checks: Vec<CandidateCheck>,
  #[serde(rename = "dependsOn")]
  pub depends_on: Vec<String>,
  pub scope: WorkScope,
}

impl WorkUnit {
  pub fn validate(
    &self,
    known_requirements: &BTreeSet<RequirementId>,
    known_criteria: &BTreeSet<CriterionId>,
    known_obligations: &BTreeSet<ObligationId>,
  ) -> Result<(), DomainValidationError> {
    if self.id.trim().is_empty() || self.title.trim().is_empty() || self.objective.trim().is_empty()
    {
      return Err(DomainValidationError::MissingWorkUnitFields);
    }
    if !self
      .id
      .chars()
      .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
      || matches!(self.id.as_str(), "." | "..")
    {
      return Err(DomainValidationError::UnsafeWorkUnitId(self.id.clone()));
    }
    if self.requirement_ids.is_empty() {
      return Err(DomainValidationError::WorkUnitWithoutRequirements(
        self.id.clone(),
      ));
    }
    if self.criterion_ids.is_empty() {
      return Err(DomainValidationError::WorkUnitWithoutAcceptanceCriteria(
        self.id.clone(),
      ));
    }
    if self.verification_obligation_ids.is_empty() {
      return Err(DomainValidationError::WorkUnitWithoutVerificationObligations(self.id.clone()));
    }
    if self.scope.paths.is_empty() || self.scope.paths.iter().any(|path| path.trim().is_empty()) {
      return Err(DomainValidationError::EmptyWorkScope(self.id.clone()));
    }
    for check in &self.suggested_checks {
      if check.command.trim().is_empty() || check.command.contains(['\r', '\n', '`']) {
        return Err(DomainValidationError::InvalidSuggestedCheck {
          work_unit_id: self.id.clone(),
          check: check.command.clone(),
        });
      }
      if !known_obligations.contains(&check.obligation_id) {
        return Err(DomainValidationError::UnknownObligation {
          work_unit_id: self.id.clone(),
          obligation_id: check.obligation_id.to_string(),
        });
      }
    }
    for requirement_id in &self.requirement_ids {
      if !known_requirements.contains(requirement_id) {
        return Err(DomainValidationError::UnknownRequirement {
          work_unit_id: self.id.clone(),
          requirement_id: requirement_id.to_string(),
        });
      }
    }
    for criterion_id in &self.criterion_ids {
      if !known_criteria.contains(criterion_id) {
        return Err(DomainValidationError::UnknownCriterion {
          work_unit_id: self.id.clone(),
          criterion_id: criterion_id.to_string(),
        });
      }
    }
    for obligation_id in &self.verification_obligation_ids {
      if !known_obligations.contains(obligation_id) {
        return Err(DomainValidationError::UnknownObligation {
          work_unit_id: self.id.clone(),
          obligation_id: obligation_id.to_string(),
        });
      }
    }
    Ok(())
  }

  pub fn suggested_commands(&self) -> impl Iterator<Item = &str> {
    self
      .suggested_checks
      .iter()
      .map(|check| check.command.as_str())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn work_unit_validation_returns_typed_unknown_requirement_error() {
    let unit = WorkUnit {
      id: "WU-001".into(),
      title: "Implement requirement".into(),
      objective: "Make behavior observable".into(),
      requirement_ids: vec![RequirementId::from("REQ-002")],
      criterion_ids: vec![CriterionId::from("REQ-002/AC-01")],
      verification_obligation_ids: vec![ObligationId::from("REQ-002/AC-01/VO-01")],
      suggested_checks: Vec::new(),
      depends_on: Vec::new(),
      scope: WorkScope {
        paths: vec!["src/**".into()],
      },
    };
    let known_requirements = BTreeSet::from([RequirementId::from("REQ-001")]);
    let known_criteria = BTreeSet::from([CriterionId::from("REQ-002/AC-01")]);
    let known_obligations = BTreeSet::from([ObligationId::from("REQ-002/AC-01/VO-01")]);

    assert_eq!(
      unit.validate(&known_requirements, &known_criteria, &known_obligations),
      Err(DomainValidationError::UnknownRequirement {
        work_unit_id: "WU-001".into(),
        requirement_id: "REQ-002".into(),
      })
    );
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReconcileResult {
  pub summary: String,
  pub requirements: Vec<RequirementAssessment>,
  #[serde(rename = "workUnits")]
  pub work_units: Vec<WorkUnit>,
}

fn default_true() -> bool {
  true
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "snake_case")]
pub enum Discovery {
  Dependency {
    #[serde(rename = "workUnitId")]
    work_unit_id: WorkUnitId,
    #[serde(rename = "dependsOn")]
    depends_on: WorkUnitId,
    reason: String,
  },
  Blocker {
    description: String,
  },
  ScopeExpansion {
    paths: Vec<String>,
    reason: String,
  },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerDiscovery {
  pub discovery: Discovery,
  pub role: WorkerRole,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryStatus {
  Active,
  Consumed,
  Invalidated,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryRecord {
  pub fingerprint: String,
  pub discovery: Discovery,
  #[serde(rename = "catalogHash")]
  pub catalog_hash: String,
  #[serde(rename = "repositoryRevision")]
  pub repository_revision: String,
  #[serde(rename = "workUnitId")]
  pub work_unit_id: String,
  pub role: WorkerRole,
  pub cycle: u32,
  pub status: DiscoveryStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerSummary {
  pub summary: String,
  #[serde(rename = "changedFiles")]
  pub changed_files: Vec<String>,
  #[serde(rename = "testsRun")]
  pub tests_run: Vec<String>,
  pub notes: Vec<String>,
  #[serde(default)]
  pub decisions: Vec<String>,
  #[serde(default)]
  pub discoveries: Vec<Discovery>,
  #[serde(default)]
  pub risks: Vec<String>,
  #[serde(default, rename = "followUps")]
  pub follow_ups: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRole {
  Architect,
  Reconcile,
  Implement,
  Repair,
  Assess,
}

impl WorkerRole {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Architect => "architect",
      Self::Reconcile => "reconcile",
      Self::Implement => "implement",
      Self::Repair => "repair",
      Self::Assess => "assess",
    }
  }

  pub fn is_read_only(self) -> bool {
    matches!(self, Self::Architect | Self::Reconcile | Self::Assess)
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerEvent {
  Start {
    role: WorkerRole,
    #[serde(rename = "workerId")]
    worker_id: String,
    #[serde(rename = "leaseId")]
    lease_id: Option<String>,
    #[serde(rename = "workUnitId")]
    work_unit_id: Option<String>,
    at: String,
  },
  Text {
    role: WorkerRole,
    #[serde(rename = "workerId")]
    worker_id: String,
    #[serde(rename = "leaseId")]
    lease_id: Option<String>,
    #[serde(rename = "workUnitId")]
    work_unit_id: Option<String>,
    at: String,
    delta: String,
  },
  ToolStart {
    role: WorkerRole,
    #[serde(rename = "workerId")]
    worker_id: String,
    #[serde(rename = "leaseId")]
    lease_id: Option<String>,
    #[serde(rename = "workUnitId")]
    work_unit_id: Option<String>,
    at: String,
    tool_name: String,
    args: serde_json::Value,
  },
  ToolEnd {
    role: WorkerRole,
    #[serde(rename = "workerId")]
    worker_id: String,
    #[serde(rename = "leaseId")]
    lease_id: Option<String>,
    #[serde(rename = "workUnitId")]
    work_unit_id: Option<String>,
    at: String,
    tool_name: String,
    is_error: bool,
    output: Option<String>,
  },
  End {
    role: WorkerRole,
    #[serde(rename = "workerId")]
    worker_id: String,
    #[serde(rename = "leaseId")]
    lease_id: Option<String>,
    #[serde(rename = "workUnitId")]
    work_unit_id: Option<String>,
    at: String,
    ok: bool,
    message: Option<String>,
  },
}
