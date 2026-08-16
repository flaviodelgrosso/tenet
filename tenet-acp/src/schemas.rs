use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StructuredOutputError {
  #[error("could not serialize generated JSON Schema: {0}")]
  SchemaSerialization(#[source] serde_json::Error),
  #[error("generated JSON Schema is invalid: {0}")]
  InvalidSchema(String),
  #[error("structured output failed JSON Schema validation: {0}")]
  SchemaValidation(String),
  #[error("structured output failed typed deserialization: {0}")]
  Deserialization(#[source] serde_json::Error),
}

pub fn schema_for<T: JsonSchema>() -> Result<Value, StructuredOutputError> {
  serde_json::to_value(schemars::schema_for!(T)).map_err(StructuredOutputError::SchemaSerialization)
}

pub fn validate_structured_output<T>(
  value: &Value,
  schema: &Value,
) -> Result<T, StructuredOutputError>
where
  T: DeserializeOwned,
{
  let validator = jsonschema::validator_for(schema)
    .map_err(|error| StructuredOutputError::InvalidSchema(error.to_string()))?;
  let errors: Vec<_> = validator
    .iter_errors(value)
    .map(|error| error.to_string())
    .collect();
  if !errors.is_empty() {
    return Err(StructuredOutputError::SchemaValidation(errors.join("; ")));
  }

  serde_json::from_value(value.clone()).map_err(StructuredOutputError::Deserialization)
}
