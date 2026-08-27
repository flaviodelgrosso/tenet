use chrono::Utc;
use tenet_domain::{
  evidence::{
    AcceptanceCriterion, ObligationAssessmentResult, SemanticAssessmentReport,
    VerificationObligation,
  },
  ids::{CriterionId, ObligationId, RequirementId, SpecFragmentId},
  model::{
    Discovery, DiscoveryRecord, DiscoveryStatus, MutationWorkerContext, Phase, ReconcileResult,
    Requirement, RequirementCatalog, RunStatus, State, WorkScope, WorkUnit, WorkerRole,
  },
  proof::{AssessmentJudgment, GapKind},
  verification::VerificationReport,
  worker::{CatalogCoverage, SpecFragment, SpecReference},
};
use tenet_storage::Storage;

fn synthetic_catalog() -> RequirementCatalog {
  let mut requirements = Vec::new();
  let mut criteria = Vec::new();
  let mut obligations = Vec::new();
  let mut fragments = Vec::new();
  for requirement_index in 0..30 {
    let requirement_id = RequirementId::from(format!("REQ-{requirement_index:03}"));
    let fragment_id = SpecFragmentId::from(format!("SPEC-{requirement_index:04}"));
    fragments.push(SpecFragment {
      id: fragment_id.clone(),
      section: Some(format!("Requirement {requirement_index}")),
      text: "Normative specification content repeated for a representative fixture.".repeat(3),
      text_hash: format!("hash-{requirement_index}"),
    });
    requirements.push(Requirement {
      id: requirement_id.clone(),
      title: format!("Requirement {requirement_index}"),
      description: "Detailed implementation requirement used to measure prompt context.".repeat(2),
      required: true,
      source_refs: vec![SpecReference {
        section: Some(format!("Requirement {requirement_index}")),
        fragment_id,
        text_hash: format!("hash-{requirement_index}"),
      }],
    });
    for criterion_index in 0..2 {
      let criterion_id = CriterionId::from(format!(
        "REQ-{requirement_index:03}/AC-{criterion_index:02}"
      ));
      criteria.push(AcceptanceCriterion {
        id: criterion_id.clone(),
        requirement_id: requirement_id.clone(),
        description: "Observable acceptance behavior".repeat(2),
        mandatory: true,
      });
      let obligation_count = if requirement_index * 2 + criterion_index < 40 {
        2
      } else {
        1
      };
      for obligation_index in 0..obligation_count {
        obligations.push(VerificationObligation {
          id: ObligationId::from(format!(
            "REQ-{requirement_index:03}/AC-{criterion_index:02}/VO-{obligation_index:02}"
          )),
          criterion_id: criterion_id.clone(),
          description: "Controller-bound verification obligation".repeat(2),
          required: true,
          evidence_contract: Default::default(),
        });
      }
    }
  }
  RequirementCatalog {
    spec_hash: "synthetic-spec".into(),
    requirements,
    acceptance_criteria: criteria,
    verification_obligations: obligations,
    coverage: CatalogCoverage {
      normative_fragments: fragments,
      uncovered_fragment_ids: Vec::new(),
    },
  }
}

fn work_units(prefix: &str) -> Vec<WorkUnit> {
  (0..10)
    .map(|index| WorkUnit {
      id: format!("{prefix}-{index:02}"),
      title: format!("Work {index}"),
      objective: "Implement one bounded requirement".into(),
      requirement_ids: vec![RequirementId::from(format!("REQ-{index:03}"))],
      criterion_ids: vec![CriterionId::from(format!("REQ-{index:03}/AC-00"))],
      verification_obligation_ids: vec![ObligationId::from(format!("REQ-{index:03}/AC-00/VO-00"))],
      suggested_checks: Vec::new(),
      depends_on: Vec::new(),
      scope: WorkScope {
        paths: vec![format!("src/{index}/**")],
      },
    })
    .collect()
}

#[tokio::test]
async fn implement_and_repair_contexts_are_at_least_thirty_percent_smaller_without_losing_links() {
  let project = tempfile::tempdir().expect("temporary project");
  let storage = Storage::open(project.path()).await.expect("storage");
  let catalog = synthetic_catalog();
  storage
    .persist_catalog("spec.md", Utc::now(), &catalog)
    .await
    .expect("catalog");
  storage.create_run("run-1").await.expect("run");
  storage
    .persist_reconcile_round(
      "run-1",
      1,
      "revision-1",
      &catalog.spec_hash,
      &ReconcileResult {
        summary: "historical round".repeat(10),
        requirements: Vec::new(),
        work_units: work_units("OLD"),
      },
    )
    .await
    .expect("historical round");
  let latest = ReconcileResult {
    summary: "current unresolved work".repeat(10),
    requirements: Vec::new(),
    work_units: work_units("WU"),
  };
  storage
    .persist_reconcile_round("run-1", 2, "revision-2", &catalog.spec_hash, &latest)
    .await
    .expect("latest round");
  let stale_assessments = catalog
    .verification_obligations
    .iter()
    .map(|obligation| ObligationAssessmentResult {
      obligation_id: obligation.id.clone(),
      assessment: AssessmentJudgment::Insufficient {
        reason: "Evidence for an earlier repository revision".into(),
        proposals: Vec::new(),
        gap_kind: GapKind::Evidence,
      },
    })
    .collect();
  storage
    .record_semantic_assessment(
      "run-1",
      "revision-1",
      Utc::now(),
      "assessor",
      &SemanticAssessmentReport {
        summary: "historical evidence".into(),
        assessments: stale_assessments,
      },
    )
    .await
    .expect("historical evidence");
  let current_assessments = catalog
    .verification_obligations
    .iter()
    .map(|obligation| ObligationAssessmentResult {
      obligation_id: obligation.id.clone(),
      assessment: AssessmentJudgment::Insufficient {
        reason: "Current revision evidence is absent".into(),
        proposals: Vec::new(),
        gap_kind: GapKind::Evidence,
      },
    })
    .collect();
  storage
    .record_semantic_assessment(
      "run-1",
      "revision-2",
      Utc::now(),
      "assessor",
      &SemanticAssessmentReport {
        summary: "current evidence".into(),
        assessments: current_assessments,
      },
    )
    .await
    .expect("current evidence");
  let mut state = State::fresh();
  state.run_id = Some("run-1".into());
  state.status = RunStatus::Running;
  state.phase = Phase::Implementing;
  state.discoveries = (0..10)
    .map(|index| DiscoveryRecord {
      fingerprint: format!("discovery-{index}"),
      discovery: Discovery::Blocker {
        description: format!("Relevant context for work unit {index}"),
      },
      catalog_hash: catalog.spec_hash.clone(),
      repository_revision: "revision-2".into(),
      work_unit_id: format!("WU-{index:02}"),
      role: WorkerRole::Implement,
      cycle: 2,
      status: DiscoveryStatus::Active,
    })
    .collect();
  storage
    .persist_state(&state)
    .await
    .expect("discovery state");

  let targeted = storage
    .load_current_work_unit_context("WU-00")
    .await
    .expect("targeted context");
  let report = VerificationReport {
    passed: false,
    started_at: Utc::now(),
    finished_at: Utc::now(),
    commands: Vec::new(),
    executions: Vec::new(),
    warnings: vec!["bounded verification failure".into()],
  };
  assert_eq!(catalog.requirements.len(), 30);
  assert_eq!(catalog.acceptance_criteria.len(), 60);
  assert_eq!(catalog.verification_obligations.len(), 100);
  assert_eq!(latest.work_units.len(), 10);
  let broad_discoveries: Vec<_> = state
    .discoveries
    .iter()
    .map(|record| record.discovery.clone())
    .collect();
  let targeted_discoveries: Vec<_> = targeted
    .discoveries
    .iter()
    .map(|record| record.discovery.clone())
    .collect();
  let broad_implement = MutationWorkerContext {
    work_unit: &targeted.work_unit,
    catalog: &catalog,
    discoveries: &broad_discoveries,
    previous_verification: None,
  };
  let targeted_implement = MutationWorkerContext {
    work_unit: &targeted.work_unit,
    catalog: &targeted.catalog,
    discoveries: &targeted_discoveries,
    previous_verification: None,
  };
  let broad_repair = MutationWorkerContext {
    previous_verification: Some(&report),
    ..broad_implement
  };
  let targeted_repair = MutationWorkerContext {
    previous_verification: Some(&report),
    ..targeted_implement
  };
  let broad_implement_bytes = serde_json::to_string_pretty(&broad_implement)
    .expect("broad implement context")
    .len();
  let targeted_implement_bytes = serde_json::to_string_pretty(&targeted_implement)
    .expect("targeted implement context")
    .len();
  let broad_repair_bytes = serde_json::to_string_pretty(&broad_repair)
    .expect("broad repair context")
    .len();
  let targeted_repair_bytes = serde_json::to_string_pretty(&targeted_repair)
    .expect("targeted repair context")
    .len();

  assert!(
    targeted_implement_bytes * 100 <= broad_implement_bytes * 70,
    "targeted={targeted_implement_bytes}, broad={broad_implement_bytes}"
  );
  assert!(
    targeted_repair_bytes * 100 <= broad_repair_bytes * 70,
    "targeted={targeted_repair_bytes}, broad={broad_repair_bytes}"
  );
  assert_eq!(targeted.discoveries.len(), 1);
  assert_eq!(targeted.discoveries[0].work_unit_id, "WU-00");
  assert_eq!(targeted.catalog.requirements.len(), 1);
  assert_eq!(targeted.catalog.acceptance_criteria.len(), 1);
  assert_eq!(targeted.catalog.verification_obligations.len(), 1);
  assert_eq!(
    targeted.work_unit.requirement_ids,
    vec![targeted.catalog.requirements[0].id.clone()]
  );
}
