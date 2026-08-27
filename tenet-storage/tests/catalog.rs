use chrono::{TimeZone, Utc};
use tenet_domain::{
  evidence::{AcceptanceCriterion, VerificationObligation},
  ids::{CriterionId, ObligationId, RequirementId, SpecFragmentId},
  model::{CatalogApproval, Requirement, RequirementCatalog},
  worker::{CatalogCoverage, SpecFragment, SpecReference},
};
use tenet_storage::Storage;

fn catalog() -> RequirementCatalog {
  let fragment = SpecFragment {
    id: SpecFragmentId::from("SPEC-0001-abcdef"),
    section: Some("Requirements".into()),
    text: "The system must persist state.".into(),
    text_hash: "abcdef".into(),
  };
  RequirementCatalog {
    spec_hash: "spec-hash".into(),
    requirements: vec![Requirement {
      id: RequirementId::from("REQ-001"),
      title: "Persist state".into(),
      description: "Persist controller state transactionally".into(),
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
async fn catalog_round_trip_preserves_domain_semantics() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let expected = catalog();

  storage
    .persist_catalog(
      "spec.md",
      Utc.with_ymd_and_hms(2026, 8, 20, 10, 0, 0).unwrap(),
      &expected,
    )
    .await
    .expect("persist catalog");

  assert_eq!(
    storage
      .load_catalog(&expected.spec_hash)
      .await
      .expect("load catalog"),
    Some(expected)
  );
}

#[tokio::test]
async fn catalog_write_rolls_back_when_criterion_references_unknown_requirement() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let mut invalid = catalog();
  invalid.acceptance_criteria[0].requirement_id = RequirementId::from("REQ-404");

  storage
    .persist_catalog("spec.md", Utc::now(), &invalid)
    .await
    .expect_err("foreign key mismatch must reject catalog");

  assert_eq!(
    storage
      .load_catalog(&invalid.spec_hash)
      .await
      .expect("load absent catalog"),
    None
  );
}

#[tokio::test]
async fn active_catalog_is_explicit_not_most_recent_by_timestamp() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let expected = catalog();

  storage
    .persist_catalog("spec.md", Utc::now(), &expected)
    .await
    .expect("persist catalog");

  assert_eq!(
    storage.load_active_catalog().await.expect("active catalog"),
    Some(expected)
  );
}

#[tokio::test]
async fn exact_catalog_approval_survives_reload() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let catalog = catalog();
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("persist catalog");
  let approval = CatalogApproval {
    spec_hash: catalog.spec_hash.clone(),
    catalog_hash: catalog.catalog_hash().expect("hash catalog"),
    approved_at: Utc.with_ymd_and_hms(2026, 8, 25, 10, 0, 0).unwrap(),
  };
  storage
    .persist_catalog_approval(&approval)
    .await
    .expect("approve catalog");
  drop(storage);

  let reloaded = Storage::open_existing(project.path())
    .await
    .expect("reopen storage");

  assert!(reloaded
    .catalog_is_approved(&catalog)
    .await
    .expect("check approval"));
}

#[tokio::test]
async fn replacing_catalog_under_same_spec_hash_invalidates_approval() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let original = catalog();
  storage
    .persist_catalog("spec.md", Utc::now(), &original)
    .await
    .expect("persist catalog");
  storage
    .persist_catalog_approval(&CatalogApproval {
      spec_hash: original.spec_hash.clone(),
      catalog_hash: original.catalog_hash().expect("hash catalog"),
      approved_at: Utc::now(),
    })
    .await
    .expect("approve catalog");
  let mut replacement = original.clone();
  replacement.requirements[0].description = "Different architect interpretation".into();

  storage
    .persist_catalog("spec.md", Utc::now(), &replacement)
    .await
    .expect("replace catalog");

  assert!(!storage
    .catalog_is_approved(&replacement)
    .await
    .expect("check approval"));
}

#[tokio::test]
async fn stale_approval_cannot_be_written_for_replaced_catalog() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("open storage");
  let original = catalog();
  storage
    .persist_catalog("spec.md", Utc::now(), &original)
    .await
    .expect("persist catalog");
  let stale = CatalogApproval {
    spec_hash: original.spec_hash.clone(),
    catalog_hash: original.catalog_hash().expect("hash original"),
    approved_at: Utc::now(),
  };
  let mut replacement = original;
  replacement.requirements[0].description = "Replacement interpretation".into();
  storage
    .persist_catalog("spec.md", Utc::now(), &replacement)
    .await
    .expect("replace catalog");

  storage
    .persist_catalog_approval(&stale)
    .await
    .expect_err("stale approval must fail closed");

  assert!(storage
    .load_catalog_approval()
    .await
    .expect("load approval")
    .is_none());
}
