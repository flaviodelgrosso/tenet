use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tenet_domain::evidence::EvidenceArtifact;

use crate::{repository::atomic_write, response::GateResult};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditState {
  pub schema_version: u32,
  pub evidence: Vec<EvidenceArtifact>,
  pub gates: Vec<GateResult>,
}

impl AuditState {
  pub fn load(root: &Path) -> Result<Self> {
    let path = root.join(crate::repository::STATE_PATH);
    if !path.exists() {
      return Ok(Self {
        schema_version: 1,
        ..Self::default()
      });
    }
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).context("parse local Tenet audit state")
  }

  pub fn save(&self, root: &Path) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(self)?;
    bytes.push(b'\n');
    atomic_write(&root.join(crate::repository::STATE_PATH), &bytes)
  }
}
