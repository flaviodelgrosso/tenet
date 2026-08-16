use std::{fmt, ops::Deref};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Stable work-unit identity used in serialized domain relationships.
#[derive(
  Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(transparent)]
pub struct WorkUnitId(String);

impl WorkUnitId {
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl Deref for WorkUnitId {
  type Target = str;

  fn deref(&self) -> &Self::Target {
    self.as_str()
  }
}

impl AsRef<str> for WorkUnitId {
  fn as_ref(&self) -> &str {
    self.as_str()
  }
}

impl fmt::Display for WorkUnitId {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str(self.as_str())
  }
}

impl From<String> for WorkUnitId {
  fn from(value: String) -> Self {
    Self(value)
  }
}

impl From<&str> for WorkUnitId {
  fn from(value: &str) -> Self {
    Self(value.to_owned())
  }
}

#[cfg(test)]
mod tests {
  use proptest::prelude::*;

  use super::*;

  proptest! {
    #[test]
    fn work_unit_id_json_round_trip_preserves_wire_value(value in "[A-Za-z0-9._-]{1,64}") {
      let id = WorkUnitId::from(value.clone());
      let json = serde_json::to_string(&id).expect("serialize id");
      let decoded: WorkUnitId = serde_json::from_str(&json).expect("deserialize id");

      prop_assert_eq!(decoded.as_str(), value);
    }
  }
}
