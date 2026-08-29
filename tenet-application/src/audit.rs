use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tenet_domain::evidence::EvidenceArtifact;

use crate::{
  project::{self, ExpectedEntry, atomic_write},
  response::GateResult,
};

const AUDIT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditState {
  pub schema_version: u32,
  pub evidence: Vec<EvidenceArtifact>,
  pub gates: Vec<GateResult>,
}

impl AuditState {
  pub fn load(root: &Path) -> Result<Self> {
    let path = match project::resolve_relative_path(root, project::STATE_PATH, ExpectedEntry::File)
    {
      Ok(path) => path,
      Err(project::PathResolutionError::Missing { .. }) => return Ok(Self::empty()),
      Err(error) => return Err(anyhow::Error::new(error)),
    };
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let value: serde_json::Value =
      serde_json::from_slice(&bytes).context("parse local Tenet audit state")?;
    if value
      .get("schemaVersion")
      .and_then(serde_json::Value::as_u64)
      != Some(u64::from(AUDIT_SCHEMA_VERSION))
    {
      return Ok(Self::empty());
    }
    serde_json::from_value(value).context("parse current local Tenet audit state")
  }

  fn empty() -> Self {
    Self {
      schema_version: AUDIT_SCHEMA_VERSION,
      ..Self::default()
    }
  }

  pub fn save(&self, root: &Path) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(self)?;
    bytes.push(b'\n');
    atomic_write(&root.join(crate::project::STATE_PATH), &bytes)
  }
}
