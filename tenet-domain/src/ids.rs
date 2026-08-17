use std::{fmt, ops::Deref};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! semantic_id {
  ($name:ident, $doc:literal) => {
    #[doc = $doc]
    #[derive(
      Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
    )]
    #[serde(transparent)]
    pub struct $name(String);

    impl $name {
      pub fn as_str(&self) -> &str {
        &self.0
      }
    }

    impl Deref for $name {
      type Target = str;

      fn deref(&self) -> &Self::Target {
        self.as_str()
      }
    }

    impl AsRef<str> for $name {
      fn as_ref(&self) -> &str {
        self.as_str()
      }
    }

    impl fmt::Display for $name {
      fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
      }
    }

    impl From<String> for $name {
      fn from(value: String) -> Self {
        Self(value)
      }
    }

    impl From<&str> for $name {
      fn from(value: &str) -> Self {
        Self(value.to_owned())
      }
    }
  };
}

semantic_id!(RequirementId, "Stable semantic requirement identity.");
semantic_id!(
  CriterionId,
  "Stable semantic acceptance-criterion identity."
);
semantic_id!(
  ObligationId,
  "Stable semantic verification-obligation identity."
);
semantic_id!(
  SpecFragmentId,
  "Stable identity for one normative specification fragment."
);
semantic_id!(
  WorkUnitId,
  "Stable work-unit identity used in serialized domain relationships."
);

macro_rules! uuid_id {
  ($name:ident, $doc:literal) => {
    #[doc = $doc]
    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
    #[serde(transparent)]
    pub struct $name(Uuid);

    impl $name {
      pub fn new() -> Self {
        Self(Uuid::new_v4())
      }

      pub fn as_uuid(self) -> Uuid {
        self.0
      }
    }

    impl Default for $name {
      fn default() -> Self {
        Self::new()
      }
    }

    impl fmt::Display for $name {
      fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
      }
    }
  };
}

uuid_id!(EvidenceId, "UUID-backed identity for one evidence fact.");
uuid_id!(
  VerificationRunId,
  "UUID-backed identity for one controller verification run."
);

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

  #[test]
  fn evidence_ids_round_trip_as_uuid_values() {
    let id = EvidenceId::new();
    let json = serde_json::to_string(&id).expect("serialize evidence id");
    let decoded: EvidenceId = serde_json::from_str(&json).expect("deserialize evidence id");

    assert_eq!(decoded, id);
  }
}
