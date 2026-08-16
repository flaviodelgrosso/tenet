use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainValidationError {
  #[error("work unit is missing id, title, or objective")]
  MissingWorkUnitFields,
  #[error("work unit id contains unsafe path characters: {0}")]
  UnsafeWorkUnitId(String),
  #[error("{0} targets no requirements")]
  WorkUnitWithoutRequirements(String),
  #[error("{0} has no acceptance criteria")]
  WorkUnitWithoutAcceptanceCriteria(String),
  #[error("{0} has an empty declared scope")]
  EmptyWorkScope(String),
  #[error("{work_unit_id} has an invalid suggested check; expected one executable shell command without prose or Markdown backticks: {check}")]
  InvalidSuggestedCheck { work_unit_id: String, check: String },
  #[error("{work_unit_id} targets unknown requirement {requirement_id}")]
  UnknownRequirement {
    work_unit_id: String,
    requirement_id: String,
  },
}
