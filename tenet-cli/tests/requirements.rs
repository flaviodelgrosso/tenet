use std::{
  path::PathBuf,
  process::Command,
  sync::atomic::{AtomicU64, Ordering},
  time::{SystemTime, UNIX_EPOCH},
};

use chrono::Utc;
use tenet_domain::{
  evidence::{AcceptanceCriterion, VerificationObligation},
  ids::{CriterionId, ObligationId, RequirementId, SpecFragmentId},
  model::{Requirement, RequirementCatalog},
  worker::{CatalogCoverage, SpecFragment, SpecReference},
};
use tenet_storage::Storage;

static NEXT_PROJECT: AtomicU64 = AtomicU64::new(0);

struct TempProject(PathBuf);

impl TempProject {
  fn new() -> Self {
    let nonce = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .expect("system clock")
      .as_nanos();
    let sequence = NEXT_PROJECT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
      "tenet-cli-requirements-{}-{nonce}-{sequence}",
      std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create project");
    Self(path)
  }
}

impl Drop for TempProject {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn catalog(description: &str) -> RequirementCatalog {
  let fragment = SpecFragment {
    id: SpecFragmentId::from("SPEC-0001-abcdef"),
    section: Some("Requirements".into()),
    text: "The system must persist state.".into(),
    text_hash: "abcdef".into(),
  };
  RequirementCatalog {
    spec_hash: "same-spec-hash".into(),
    requirements: vec![Requirement {
      id: RequirementId::from("REQ-001"),
      title: "Persist state".into(),
      description: description.into(),
      required: true,
      source_refs: vec![SpecReference {
        section: fragment.section.clone(),
        fragment_id: fragment.id.clone(),
        text_hash: fragment.text_hash.clone(),
      }],
    }],
    acceptance_criteria: vec![AcceptanceCriterion {
      id: CriterionId::from("REQ-001/AC-01"),
      requirement_id: RequirementId::from("REQ-001"),
      description: "State survives restart".into(),
      mandatory: true,
    }],
    verification_obligations: vec![VerificationObligation {
      id: ObligationId::from("REQ-001/AC-01/VO-01"),
      criterion_id: CriterionId::from("REQ-001/AC-01"),
      description: "Round trip is equal".into(),
      required: true,
      evidence_contract: Default::default(),
    }],
    coverage: CatalogCoverage {
      normative_fragments: vec![fragment],
      uncovered_fragment_ids: Vec::new(),
    },
  }
}

#[tokio::test]
async fn requirements_approve_approves_only_the_current_persisted_catalog() {
  let project = TempProject::new();
  let storage = Storage::open(&project.0).await.expect("open storage");
  let obsolete = catalog("Obsolete interpretation");
  storage
    .persist_catalog("spec.md", Utc::now(), &obsolete)
    .await
    .expect("persist obsolete catalog");
  let current = catalog("Current interpretation");
  storage
    .persist_catalog("spec.md", Utc::now(), &current)
    .await
    .expect("persist current catalog");

  let output = Command::new(env!("CARGO_BIN_EXE_tenet"))
    .args(["requirements", "approve", "--cwd"])
    .arg(&project.0)
    .output()
    .expect("approve requirements");
  let stdout = String::from_utf8_lossy(&output.stdout);
  let approval = storage
    .load_catalog_approval()
    .await
    .expect("load approval")
    .expect("persisted approval");

  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(approval.spec_hash, current.spec_hash);
  assert_eq!(
    approval.catalog_hash,
    current.catalog_hash().expect("hash current")
  );
  assert_ne!(
    approval.catalog_hash,
    obsolete.catalog_hash().expect("hash obsolete")
  );
  assert!(stdout.contains(&approval.catalog_hash), "{stdout}");
}

#[test]
fn requirements_approve_fails_without_an_active_catalog() {
  let project = TempProject::new();

  let output = Command::new(env!("CARGO_BIN_EXE_tenet"))
    .args(["requirements", "approve", "--cwd"])
    .arg(&project.0)
    .output()
    .expect("approve missing requirements");

  assert!(!output.status.success());
  assert!(
    String::from_utf8_lossy(&output.stderr).contains("no active requirement catalog to approve")
  );
}
