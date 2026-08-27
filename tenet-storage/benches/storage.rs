use std::time::{Duration, Instant};

use chrono::Utc;
use tenet_domain::{
  evidence::{
    AcceptanceCriterion, ObligationAssessmentResult, SemanticAssessmentReport,
    VerificationObligation,
  },
  ids::{CriterionId, ObligationId, RequirementId, SpecFragmentId},
  model::{ReconcileResult, Requirement, RequirementCatalog, State, WorkScope, WorkUnit},
  proof::{AssessmentJudgment, GapKind},
  worker::{CatalogCoverage, SpecFragment, SpecReference},
};
use tenet_storage::Storage;

const ITERATIONS: u32 = 50;

fn catalog() -> RequirementCatalog {
  let requirement_id = RequirementId::from("REQ-001");
  let criterion_id = CriterionId::from("REQ-001/AC-01");
  let obligation_id = ObligationId::from("REQ-001/AC-01/VO-01");
  let fragment_id = SpecFragmentId::from("SPEC-0001");
  RequirementCatalog {
    spec_hash: "benchmark-spec".into(),
    requirements: vec![Requirement {
      id: requirement_id.clone(),
      title: "Benchmark requirement".into(),
      description: "Exercise relational storage operations".into(),
      required: true,
      source_refs: vec![SpecReference {
        section: Some("Benchmark".into()),
        fragment_id: fragment_id.clone(),
        text_hash: "fragment-hash".into(),
      }],
    }],
    acceptance_criteria: vec![AcceptanceCriterion {
      id: criterion_id.clone(),
      requirement_id,
      description: "Storage operation completes".into(),
      mandatory: true,
    }],
    verification_obligations: vec![VerificationObligation {
      id: obligation_id,
      criterion_id,
      description: "Observe storage result".into(),
      required: true,
      evidence_contract: Default::default(),
    }],
    coverage: CatalogCoverage {
      normative_fragments: vec![SpecFragment {
        id: fragment_id,
        section: Some("Benchmark".into()),
        text: "Storage benchmark specification".into(),
        text_hash: "fragment-hash".into(),
      }],
      uncovered_fragment_ids: Vec::new(),
    },
  }
}

fn reconciliation() -> ReconcileResult {
  ReconcileResult {
    summary: "Benchmark reconciliation".into(),
    requirements: Vec::new(),
    work_units: vec![WorkUnit {
      id: "WU-001".into(),
      title: "Benchmark work".into(),
      objective: "Exercise one targeted context query".into(),
      requirement_ids: vec![RequirementId::from("REQ-001")],
      criterion_ids: vec![CriterionId::from("REQ-001/AC-01")],
      verification_obligation_ids: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
      suggested_checks: Vec::new(),
      depends_on: Vec::new(),
      scope: WorkScope {
        paths: vec!["src/**".into()],
      },
    }],
  }
}

fn elapsed_per_iteration(start: Instant) -> Duration {
  start.elapsed() / ITERATIONS
}

fn main() {
  let runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
  runtime.block_on(async {
    let project = tempfile::tempdir().expect("temporary benchmark project");
    let storage = Storage::open(project.path()).await.expect("storage");
    let catalog = catalog();
    storage
      .persist_catalog("spec.md", Utc::now(), &catalog)
      .await
      .expect("catalog");
    storage.create_run("benchmark-run").await.expect("run");
    storage
      .persist_reconcile_round(
        "benchmark-run",
        1,
        "revision-1",
        &catalog.spec_hash,
        &reconciliation(),
      )
      .await
      .expect("reconciliation");
    let mut state = State::fresh();
    state.run_id = Some("benchmark-run".into());
    state.status = tenet_domain::model::RunStatus::Running;
    storage.persist_state(&state).await.expect("state");
    storage
      .record_semantic_assessment(
        "benchmark-run",
        "revision-1",
        Utc::now(),
        "benchmark-assessor",
        &SemanticAssessmentReport {
          summary: "Benchmark evidence".into(),
          assessments: vec![ObligationAssessmentResult {
            obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
            assessment: AssessmentJudgment::Insufficient {
              reason: "Benchmark evidence is advisory".into(),
              proposals: Vec::new(),
              gap_kind: GapKind::Evidence,
            },
          }],
        },
      )
      .await
      .expect("evidence");

    let start = Instant::now();
    for _ in 0..ITERATIONS {
      storage
        .load_current_state()
        .await
        .expect("load current run");
    }
    println!("load_current_run={:?}", elapsed_per_iteration(start));

    let start = Instant::now();
    for cycle in 0..ITERATIONS {
      state.cycle = cycle;
      storage
        .persist_state(&state)
        .await
        .expect("persist state transition");
    }
    println!(
      "persist_state_transition={:?}",
      elapsed_per_iteration(start)
    );

    let start = Instant::now();
    for _ in 0..ITERATIONS {
      storage.load_active_catalog().await.expect("load catalog");
    }
    println!("load_catalog={:?}", elapsed_per_iteration(start));

    let start = Instant::now();
    for _ in 0..ITERATIONS {
      storage
        .load_current_work_unit_context("WU-001")
        .await
        .expect("work context");
    }
    println!("fetch_work_unit_context={:?}", elapsed_per_iteration(start));

    let start = Instant::now();
    for _ in 0..ITERATIONS {
      storage
        .invalidate_evidence_for_revision("benchmark-run", "revision-2", Utc::now())
        .await
        .expect("invalidate evidence");
    }
    println!("invalidate_evidence={:?}", elapsed_per_iteration(start));

    let start = Instant::now();
    for _ in 0..ITERATIONS {
      storage
        .load_evidence_graph(&catalog, &[], &[])
        .await
        .expect("completion evidence facts");
    }
    println!(
      "load_completion_evidence={:?}",
      elapsed_per_iteration(start)
    );
  });
}
