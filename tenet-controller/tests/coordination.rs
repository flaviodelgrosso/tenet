use std::{
  collections::{BTreeMap, BTreeSet},
  path::{Path, PathBuf},
  process::Command,
  sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
  },
  time::Duration,
};

use anyhow::{bail, Result};
use async_trait::async_trait;
use chrono::Utc;
use tokio::{
  fs,
  sync::{mpsc, Notify},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use tenet_controller::ports::agent::{ReconciliationRequest, SemanticAssessmentRequest};
use tenet_controller::{
  controller::manual_verify, evidence as controller_evidence, AgentBackend, Controller,
};
use tenet_domain::{
  config::{read_config, Config, CustomAgentConfig, ProjectVerificationCheck},
  events::{EventSink, RunEvent},
  evidence::{
    AcceptanceCriterion, AgentObligationAssessment, ImplementationState,
    SemanticAssessmentProposal, VerificationObligation,
  },
  ids::{ArchitectSourceRef, CriterionId, ObligationId, RequirementId},
  model::{
    AgentReconciliationProposal, AgentRequirementAssessment, AgentWorkUnit, ArchitectOutput,
    ArchitectRequirement, CandidateCheck, CatalogApproval, CompletedWorkUnit, Discovery,
    DiscoveryRecord, DiscoveryStatus, IntegrationPhase, IntegrationTransaction, Phase,
    ReconcileResult, Requirement, RequirementCatalog, RunStatus, State, VerificationReport,
    WorkExecution, WorkLease, WorkScope, WorkUnit, WorkerRole, WorkerSummary,
  },
  proof::{AssessmentJudgment, EvidenceContract, EvidencePredicate, GapKind, ProofState},
  verification::ProjectVerificationRun,
  worker::{derive_normative_fragments, CatalogCoverage},
};
use tenet_runtime::{
  backend::{BackendContext, LaunchMetadata},
  git,
  integration::{IntegrationOutcome, Integrator},
  store,
  workspace::WorkspaceManager,
};

struct TempRepo(PathBuf);

impl TempRepo {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("tenet-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&path).expect("create temp repository");
    run_git(&path, &["init"]);
    run_git(&path, &["config", "user.name", "Tenet Test"]);
    run_git(&path, &["config", "user.email", "tenet-test@localhost"]);
    std::fs::write(path.join(".gitignore"), ".tenet/\n").expect("write gitignore");
    std::fs::write(path.join("README.txt"), "base\n").expect("write base file");
    run_git(&path, &["add", "-A"]);
    run_git(&path, &["commit", "-m", "base"]);
    Self(path)
  }

  fn path(&self) -> &Path {
    &self.0
  }
}

impl Drop for TempRepo {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn run_git(cwd: &Path, args: &[&str]) -> String {
  let output = Command::new("git")
    .args(args)
    .current_dir(cwd)
    .output()
    .expect("run git");
  assert!(
    output.status.success(),
    "git {} failed: {}",
    args.join(" "),
    String::from_utf8_lossy(&output.stderr)
  );
  String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

async fn reset_storage(repository: &Path) {
  for name in ["tenet.db", "tenet.db-wal", "tenet.db-shm"] {
    match fs::remove_file(repository.join(".tenet").join(name)).await {
      Ok(()) => {}
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(error) => panic!("remove {name}: {error}"),
    }
  }
}

fn annotated_references(spec: &str) -> Vec<ArchitectSourceRef> {
  spec
    .lines()
    .filter_map(|line| line.strip_prefix("[sourceRef="))
    .filter_map(|metadata| metadata.split([' ', ']']).next())
    .map(ArchitectSourceRef::from)
    .collect()
}

fn annotated_reference(spec: &str) -> ArchitectSourceRef {
  annotated_references(spec)
    .into_iter()
    .next()
    .expect("annotated source token")
}

fn requirement_for(spec: &str) -> Requirement {
  Requirement {
    id: RequirementId::from("REQ-001"),
    title: "Diamond".into(),
    description: "Complete the diamond graph".into(),
    required: true,
    source_refs: vec![derive_normative_fragments(spec)[0].reference()],
  }
}

fn requirement() -> Requirement {
  requirement_for("diamond")
}

fn architect_requirement() -> ArchitectRequirement {
  let requirement = requirement();
  ArchitectRequirement {
    id: requirement.id,
    title: requirement.title,
    description: requirement.description,
    required: requirement.required,
    source_refs: requirement
      .source_refs
      .into_iter()
      .enumerate()
      .map(|(index, _)| ArchitectSourceRef::from(format!("B0001-F{:02}", index + 1)))
      .collect(),
  }
}

fn criterion() -> AcceptanceCriterion {
  AcceptanceCriterion {
    id: CriterionId::from("REQ-001/AC-01"),
    requirement_id: RequirementId::from("REQ-001"),
    description: "A, B, C, and D exist".into(),
    mandatory: true,
  }
}

fn obligation() -> VerificationObligation {
  VerificationObligation {
    id: ObligationId::from("REQ-001/AC-01/VO-01"),
    criterion_id: CriterionId::from("REQ-001/AC-01"),
    description: "Verify every diamond output".into(),
    required: true,
    evidence_contract: EvidenceContract::Artifact {
      predicate: EvidencePredicate::NamedProjectCheck {
        name: "project verification".into(),
      },
    },
  }
}

fn unit(id: &str, dependencies: &[&str]) -> WorkUnit {
  WorkUnit {
    id: id.into(),
    title: format!("Implement {id}"),
    objective: format!("Create {id}"),
    requirement_ids: vec![RequirementId::from("REQ-001")],
    criterion_ids: vec![CriterionId::from("REQ-001/AC-01")],
    verification_obligation_ids: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
    suggested_checks: vec![CandidateCheck {
      obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
      command: format!("test -f {id}.txt"),
    }],
    depends_on: dependencies.iter().map(|value| (*value).into()).collect(),
    scope: WorkScope {
      paths: vec![format!("{id}.txt")],
    },
  }
}

fn proposed_unit(id: &str, dependencies: &[&str]) -> AgentWorkUnit {
  let unit = unit(id, dependencies);
  AgentWorkUnit {
    id: unit.id,
    title: unit.title,
    objective: unit.objective,
    verification_obligation_ids: unit.verification_obligation_ids,
    suggested_checks: unit.suggested_checks,
    depends_on: unit.depends_on,
    scope: unit.scope,
  }
}

fn graph_units() -> Vec<AgentWorkUnit> {
  vec![
    proposed_unit("A", &[]),
    proposed_unit("B", &["A"]),
    proposed_unit("C", &["A"]),
    proposed_unit("D", &["B", "C"]),
  ]
}

fn assessment(satisfied: bool) -> AgentRequirementAssessment {
  AgentRequirementAssessment {
    requirement_handle: "R001".into(),
    implementation_state: if satisfied {
      ImplementationState::Present
    } else {
      ImplementationState::Absent
    },
    observations: satisfied
      .then(|| "all files exist".into())
      .into_iter()
      .collect(),
    missing_implementation: (!satisfied)
      .then(|| "diamond incomplete".into())
      .into_iter()
      .collect(),
    missing_evidence: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
  }
}

fn summary(discoveries: Vec<Discovery>) -> WorkerSummary {
  WorkerSummary {
    summary: "implemented".into(),
    changed_files: Vec::new(),
    tests_run: Vec::new(),
    notes: Vec::new(),
    decisions: Vec::new(),
    discoveries,
    risks: Vec::new(),
    follow_ups: Vec::new(),
  }
}

#[derive(Clone, Copy)]
enum ScopeMutation {
  Modification,
  Addition,
  Deletion,
  Rename,
}

#[derive(Clone, Copy)]
enum BackendMode {
  Normal,
  LargeCatalog,
  EmptyImplementationThenRepair,
  EmptyThenNeverPassingVerification,
  RequireSpecification,
  IncompleteCatalogThenCorrect,
  AlwaysIncompleteCatalog,
  FailB,
  WorkerTimeout,
  ProtectedMutation,
  WaitForCancellation,
  RepairDiscovery,
  InvalidVerificationCheck,
  NeverPassingVerification,
  VerificationBlockerThenRepairFailure,
  IncompleteAssessment,
  SemanticGapThenRepair,
  CleanupFailure,
  InvalidReconcileThenCorrect,
  InvalidAssessmentThenCorrect,
  AlwaysInvalidReconcile,
  StructuralRetryThenInvalidReconcile,
  ContradictoryReconcileThenCorrect,
  AlwaysContradictoryReconcile,
  InvalidScopeThenCorrect,
  InvalidAssessmentScopeThenCorrect,
  RepairScopeMutation,
  UnapprovedScopeMutation,
  SemanticallyRepeatedWork,
  ScopeMutation(ScopeMutation),
  ReadOnlyMutation {
    role: tenet_domain::model::WorkerRole,
    commit: bool,
  },
}

struct FakeBackend {
  mode: BackendMode,
  workspaces: Mutex<Vec<(String, PathBuf)>>,
  canonical: Mutex<Option<PathBuf>>,
  waiting: Notify,
  active: AtomicUsize,
  max_active: AtomicUsize,
  discoveries_seen: AtomicUsize,
  repair_calls: AtomicUsize,
  reconcile_calls: AtomicUsize,
  architect_calls: AtomicUsize,
  semantic_feedback: Mutex<Vec<(WorkerRole, String)>>,
  assessment_calls: AtomicUsize,
}

impl FakeBackend {
  fn new(mode: BackendMode) -> Self {
    Self {
      mode,
      workspaces: Mutex::new(Vec::new()),
      canonical: Mutex::new(None),
      active: AtomicUsize::new(0),
      max_active: AtomicUsize::new(0),
      waiting: Notify::new(),
      discoveries_seen: AtomicUsize::new(0),
      repair_calls: AtomicUsize::new(0),
      reconcile_calls: AtomicUsize::new(0),
      architect_calls: AtomicUsize::new(0),
      semantic_feedback: Mutex::new(Vec::new()),
      assessment_calls: AtomicUsize::new(0),
    }
  }

  fn record_active(&self) -> ActiveGuard<'_> {
    let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
    self.max_active.fetch_max(active, Ordering::SeqCst);
    ActiveGuard(&self.active)
  }
  async fn require_specification(&self, ctx: &BackendContext) -> Result<()> {
    if matches!(self.mode, BackendMode::RequireSpecification) {
      let specification = fs::read_to_string(ctx.cwd.join(&ctx.config.spec_file)).await?;
      if specification != "diamond" {
        bail!("agent workspace contains the wrong authoritative specification");
      }
    }
    Ok(())
  }

  async fn mutate_read_only_workspace(
    &self,
    ctx: &BackendContext,
    role: tenet_domain::model::WorkerRole,
  ) -> Result<()> {
    let BackendMode::ReadOnlyMutation {
      role: target,
      commit,
    } = self.mode
    else {
      return Ok(());
    };
    if target != role {
      return Ok(());
    }
    fs::write(ctx.cwd.join("read-only-mutation.txt"), role.as_str()).await?;
    if commit {
      run_git(&ctx.cwd, &["add", "read-only-mutation.txt"]);
      run_git(&ctx.cwd, &["commit", "-m", "malicious read-only mutation"]);
    }
    Ok(())
  }
}

struct ActiveGuard<'a>(&'a AtomicUsize);

impl Drop for ActiveGuard<'_> {
  fn drop(&mut self) {
    self.0.fetch_sub(1, Ordering::SeqCst);
  }
}

#[async_trait]
impl AgentBackend for FakeBackend {
  async fn architect(&self, ctx: &BackendContext, spec: &str) -> Result<ArchitectOutput> {
    self.require_specification(ctx).await?;
    self
      .mutate_read_only_workspace(ctx, tenet_domain::model::WorkerRole::Architect)
      .await?;
    let mut requirement = architect_requirement();
    let attempt = self.architect_calls.fetch_add(1, Ordering::SeqCst);
    requirement.source_refs = if matches!(self.mode, BackendMode::LargeCatalog)
      || matches!(self.mode, BackendMode::IncompleteCatalogThenCorrect) && attempt > 0
    {
      if matches!(self.mode, BackendMode::IncompleteCatalogThenCorrect) {
        self
          .semantic_feedback
          .lock()
          .expect("semantic feedback lock")
          .push((WorkerRole::Architect, spec.to_owned()));
      }
      annotated_references(spec)
    } else {
      vec![annotated_reference(spec)]
    };
    Ok(ArchitectOutput {
      requirements: vec![requirement],
      acceptance_criteria: vec![criterion()],
      verification_obligations: vec![obligation()],
    })
  }

  async fn resolve_launch(&self, _cwd: &Path, _config: &Config) -> Result<Option<LaunchMetadata>> {
    Ok(None)
  }

  async fn reconcile(
    &self,
    ctx: &BackendContext,
    request: ReconciliationRequest<'_>,
    semantic_validation_feedback: Option<&str>,
  ) -> Result<AgentReconciliationProposal> {
    let catalog = request.catalog;
    let requirement_handles = request.requirement_handles;
    let discoveries = request.discoveries;
    self.require_specification(ctx).await?;
    self
      .mutate_read_only_workspace(ctx, tenet_domain::model::WorkerRole::Reconcile)
      .await?;
    if matches!(self.mode, BackendMode::StructuralRetryThenInvalidReconcile) {
      ctx
        .completion_budget
        .as_ref()
        .expect("read-only worker supplies a completion budget")
        .reserve()?;
    }
    let reconcile_call = self.reconcile_calls.fetch_add(1, Ordering::SeqCst);
    if let Some(feedback) = semantic_validation_feedback {
      self
        .semantic_feedback
        .lock()
        .expect("semantic feedback lock")
        .push((WorkerRole::Reconcile, feedback.into()));
    }
    if matches!(self.mode, BackendMode::InvalidReconcileThenCorrect) {
      let marker = ctx.cwd.join("semantic-reconcile-attempt.txt");
      if reconcile_call == 0 {
        fs::write(marker, "discard me").await?;
      } else if marker.exists() {
        bail!("reconciliation retry reused a dirty inspection workspace");
      }
    }
    self
      .discoveries_seen
      .fetch_add(discoveries.len(), Ordering::SeqCst);
    if matches!(self.mode, BackendMode::SemanticallyRepeatedWork) {
      let id = format!("repeat-{reconcile_call}");
      let mut work_unit = proposed_unit(&id, &[]);
      work_unit.title = format!("Planner prose {reconcile_call}");
      work_unit.objective = format!("Repeated objective {reconcile_call}");
      work_unit.suggested_checks.clear();
      work_unit.scope.paths = vec!["repeat/**".into()];
      return Ok(AgentReconciliationProposal {
        summary: format!("Repeated summary {reconcile_call}"),
        requirements: vec![assessment(false)],
        work_units: vec![work_unit],
      });
    }
    if matches!(self.mode, BackendMode::LargeCatalog) {
      let requirements = catalog
        .requirements
        .iter()
        .map(|requirement| {
          let criterion_ids: BTreeSet<_> = catalog
            .acceptance_criteria
            .iter()
            .filter(|criterion| criterion.requirement_id == requirement.id)
            .map(|criterion| criterion.id.clone())
            .collect();
          AgentRequirementAssessment {
            requirement_handle: requirement_handles
              .get(&requirement.id)
              .expect("controller supplies a handle for every requirement")
              .clone(),
            implementation_state: ImplementationState::Present,
            observations: vec!["large specification behavior is present".into()],
            missing_implementation: Vec::new(),
            missing_evidence: catalog
              .verification_obligations
              .iter()
              .filter(|obligation| criterion_ids.contains(&obligation.criterion_id))
              .map(|obligation| obligation.id.clone())
              .collect(),
          }
        })
        .collect();
      return Ok(AgentReconciliationProposal {
        summary: "large catalog implementation present; evidence pending".into(),
        requirements,
        work_units: Vec::new(),
      });
    }
    let missing: std::collections::BTreeSet<_> = ["A", "B", "C", "D"]
      .into_iter()
      .filter(|id| !ctx.cwd.join(format!("{id}.txt")).exists())
      .collect();
    let satisfied = missing.is_empty();
    let mut work_units: Vec<_> = graph_units()
      .into_iter()
      .filter(|unit| missing.contains(unit.id.as_str()))
      .collect();
    for unit in &mut work_units {
      unit
        .depends_on
        .retain(|dependency| missing.contains(dependency.as_str()));
    }
    if matches!(self.mode, BackendMode::SemanticGapThenRepair)
      && satisfied
      && discoveries
        .iter()
        .any(|discovery| matches!(discovery, Discovery::VerificationBlocker { .. }))
      && !ctx.cwd.join("semantic-fix.txt").exists()
    {
      work_units.push(proposed_unit("semantic-fix", &[]));
    }
    if matches!(
      self.mode,
      BackendMode::ScopeMutation(_) | BackendMode::RepairScopeMutation
    ) {
      if let Some(unit) = work_units.iter_mut().find(|unit| unit.id == "A") {
        let expanded: Vec<_> = discoveries
          .iter()
          .filter_map(|discovery| {
            let Discovery::ScopeExpansion { paths, .. } = discovery else {
              return None;
            };
            Some(paths.iter().cloned())
          })
          .flatten()
          .collect();
        if !expanded.is_empty() {
          unit.scope.paths = expanded;
          unit.scope.paths.sort();
          unit.scope.paths.dedup();
        }
      }
    }
    if matches!(
      self.mode,
      BackendMode::RepairDiscovery | BackendMode::RepairScopeMutation
    ) {
      if let Some(unit) = work_units.iter_mut().find(|unit| unit.id == "A") {
        unit.suggested_checks = vec![CandidateCheck {
          obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
          command: "grep -q repaired A.txt".into(),
        }];
      }
    }
    if matches!(
      self.mode,
      BackendMode::NeverPassingVerification
        | BackendMode::EmptyThenNeverPassingVerification
        | BackendMode::VerificationBlockerThenRepairFailure
    ) {
      if let Some(unit) = work_units.iter_mut().find(|unit| unit.id == "A") {
        unit.suggested_checks = vec![CandidateCheck {
          obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
          command: "false".into(),
        }];
      }
    }
    if matches!(self.mode, BackendMode::InvalidVerificationCheck) {
      if let Some(unit) = work_units.iter_mut().find(|unit| unit.id == "A") {
        unit.suggested_checks = vec![CandidateCheck {
          obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
          command: "false".into(),
        }];
      }
    }
    if matches!(
      self.mode,
      BackendMode::AlwaysInvalidReconcile | BackendMode::StructuralRetryThenInvalidReconcile
    ) || matches!(self.mode, BackendMode::InvalidReconcileThenCorrect) && reconcile_call == 0
    {
      work_units[0].verification_obligation_ids = vec![ObligationId::from("REQ-999/AC-01/VO-01")];
    }
    if matches!(self.mode, BackendMode::InvalidScopeThenCorrect) && reconcile_call == 0 {
      work_units[0].scope.paths = vec!["src/".into()];
    }
    let mut requirement_assessment = assessment(satisfied);
    if matches!(self.mode, BackendMode::AlwaysContradictoryReconcile)
      || matches!(self.mode, BackendMode::ContradictoryReconcileThenCorrect) && reconcile_call == 0
    {
      requirement_assessment.implementation_state = ImplementationState::Present;
      requirement_assessment.missing_implementation =
        vec!["required behavior is still missing".into()];
    }
    Ok(AgentReconciliationProposal {
      summary: if satisfied {
        "implementation present; evidence pending".into()
      } else {
        "work remains".into()
      },
      requirements: vec![requirement_assessment],
      work_units,
    })
  }

  async fn implement(
    &self,
    ctx: &BackendContext,
    _catalog: &RequirementCatalog,
    work_unit: &WorkUnit,
    _discoveries: &[Discovery],
  ) -> Result<WorkerSummary> {
    self.require_specification(ctx).await?;
    let _active = self.record_active();
    self
      .workspaces
      .lock()
      .expect("workspace lock")
      .push((work_unit.id.clone(), ctx.cwd.clone()));
    if matches!(self.mode, BackendMode::SemanticallyRepeatedWork) {
      fs::create_dir_all(ctx.cwd.join("repeat")).await?;
      fs::write(
        ctx.cwd.join("repeat").join(format!("{}.txt", work_unit.id)),
        work_unit.id.as_bytes(),
      )
      .await?;
      return Ok(summary(Vec::new()));
    }
    if matches!(self.mode, BackendMode::WorkerTimeout) {
      bail!("implement worker timed out after 1s");
    }
    if matches!(self.mode, BackendMode::FailB) && work_unit.id == "B" {
      bail!("synthetic worker failure");
    }
    if matches!(self.mode, BackendMode::WaitForCancellation) && work_unit.id != "A" {
      self.waiting.notify_one();
      ctx.cancel.cancelled().await;
      bail!("run cancelled");
    }
    if matches!(
      self.mode,
      BackendMode::EmptyImplementationThenRepair | BackendMode::EmptyThenNeverPassingVerification
    ) {
      return Ok(summary(Vec::new()));
    }
    if work_unit.id == "A" {
      match self.mode {
        BackendMode::ScopeMutation(ScopeMutation::Modification) => {
          fs::write(ctx.cwd.join("README.txt"), "unauthorized modification").await?;
        }
        BackendMode::ScopeMutation(ScopeMutation::Addition) => {
          fs::write(ctx.cwd.join("outside.txt"), "unauthorized addition").await?;
        }
        BackendMode::ScopeMutation(ScopeMutation::Deletion) => {
          fs::remove_file(ctx.cwd.join("README.txt")).await?;
        }
        BackendMode::ScopeMutation(ScopeMutation::Rename) => {
          fs::rename(ctx.cwd.join("README.txt"), ctx.cwd.join("renamed.txt")).await?;
        }
        BackendMode::UnapprovedScopeMutation => {
          fs::write(ctx.cwd.join("outside.txt"), "unauthorized addition").await?;
        }
        _ => {}
      }
    }
    if matches!(self.mode, BackendMode::ProtectedMutation) {
      fs::write(ctx.cwd.join("tenet.toml"), "modified").await?;
    }
    if matches!(work_unit.id.as_str(), "B" | "C") {
      tokio::time::sleep(Duration::from_millis(50)).await;
    }
    fs::write(
      ctx.cwd.join(format!("{}.txt", work_unit.id)),
      work_unit.id.as_bytes(),
    )
    .await?;
    let discoveries = (work_unit.id == "A")
      .then(|| Discovery::ScopeExpansion {
        paths: vec!["generated/**".into()],
        reason: "generated files observed".into(),
      })
      .into_iter()
      .collect();
    Ok(summary(discoveries))
  }

  async fn repair(
    &self,
    ctx: &BackendContext,
    _catalog: &RequirementCatalog,
    work_unit: &WorkUnit,
    _discoveries: &[Discovery],
    _report: &VerificationReport,
  ) -> Result<WorkerSummary> {
    let repair_number = self.repair_calls.fetch_add(1, Ordering::SeqCst) + 1;
    if matches!(self.mode, BackendMode::VerificationBlockerThenRepairFailure) && work_unit.id == "A"
    {
      if repair_number == 1 {
        return Ok(summary(vec![Discovery::VerificationBlocker {
          description: "defer the candidate once".into(),
        }]));
      }
      bail!("synthetic resumed repair failure");
    }
    if matches!(
      self.mode,
      BackendMode::EmptyImplementationThenRepair | BackendMode::EmptyThenNeverPassingVerification
    ) {
      fs::write(
        ctx.cwd.join(format!("{}.txt", work_unit.id)),
        work_unit.id.as_bytes(),
      )
      .await?;
      return Ok(summary(Vec::new()));
    }
    if matches!(
      self.mode,
      BackendMode::NeverPassingVerification | BackendMode::EmptyThenNeverPassingVerification
    ) && work_unit.id == "A"
    {
      fs::write(ctx.cwd.join("A.txt"), format!("repair-{repair_number}")).await?;
      return Ok(summary(Vec::new()));
    }
    if matches!(self.mode, BackendMode::RepairScopeMutation) && work_unit.id == "A" {
      fs::write(ctx.cwd.join("A.txt"), "repaired").await?;
      fs::write(ctx.cwd.join("outside.txt"), "repair expansion").await?;
      return Ok(summary(Vec::new()));
    }
    if matches!(self.mode, BackendMode::RepairDiscovery) && work_unit.id == "A" {
      fs::write(ctx.cwd.join("A.txt"), "repaired").await?;
      return Ok(summary(vec![Discovery::Blocker {
        description: "repair found a dependent concern".into(),
      }]));
    }
    if matches!(self.mode, BackendMode::InvalidVerificationCheck) && work_unit.id == "A" {
      return Ok(summary(vec![Discovery::VerificationBlocker {
        description: "verification command hides its required tool environment".into(),
      }]));
    }
    bail!("repair not expected")
  }

  async fn assess(
    &self,
    ctx: &BackendContext,
    request: SemanticAssessmentRequest<'_>,
    semantic_validation_feedback: Option<&str>,
  ) -> Result<SemanticAssessmentProposal> {
    let catalog = request.catalog;
    let obligation_handles = request.obligation_handles;
    self.require_specification(ctx).await?;
    self
      .mutate_read_only_workspace(ctx, tenet_domain::model::WorkerRole::Assess)
      .await?;
    let assessment_call = self.assessment_calls.fetch_add(1, Ordering::SeqCst);
    if let Some(feedback) = semantic_validation_feedback {
      self
        .semantic_feedback
        .lock()
        .expect("semantic feedback lock")
        .push((WorkerRole::Assess, feedback.into()));
    }
    if matches!(self.mode, BackendMode::InvalidAssessmentThenCorrect) {
      let marker = ctx.cwd.join("semantic-assessment-attempt.txt");
      if assessment_call == 0 {
        fs::write(marker, "discard me").await?;
      } else if marker.exists() {
        bail!("assessment retry reused a dirty inspection workspace");
      }
    }
    if matches!(self.mode, BackendMode::LargeCatalog) {
      return Ok(SemanticAssessmentProposal {
        summary: "large catalog independently assessed".into(),
        assessments: catalog
          .verification_obligations
          .iter()
          .map(|obligation| AgentObligationAssessment {
            obligation_handle: obligation_handles
              .get(&obligation.id)
              .expect("controller supplies a handle for every obligation")
              .clone(),
            judgment: AssessmentJudgment::Supported {
              artifact_ids: Vec::new(),
              rationale: "the immutable revision satisfies the batch requirement".into(),
            },
          })
          .collect(),
      });
    }
    let satisfied = ["A", "B", "C", "D"]
      .iter()
      .all(|id| ctx.cwd.join(format!("{id}.txt")).exists());
    if matches!(self.mode, BackendMode::CleanupFailure) && satisfied {
      let canonical = self
        .canonical
        .lock()
        .expect("canonical path lock")
        .clone()
        .expect("canonical path configured");
      let run_id = ctx
        .runtime_dir
        .parent()
        .and_then(Path::file_name)
        .expect("assessment run id");
      let obstacle = canonical.join(".tenet/integration").join(run_id);
      fs::create_dir_all(obstacle.parent().expect("integration parent")).await?;
      fs::write(obstacle, "cleanup obstacle").await?;
    }
    let invalid_first = matches!(
      self.mode,
      BackendMode::InvalidAssessmentThenCorrect | BackendMode::InvalidAssessmentScopeThenCorrect
    ) && assessment_call == 0;
    if invalid_first {
      return Ok(SemanticAssessmentProposal {
        summary: "incomplete semantic assessment".into(),
        assessments: Vec::new(),
      });
    }
    let semantic_repair_present = ctx.cwd.join("semantic-fix.txt").exists();
    let assessment = match self.mode {
      BackendMode::SemanticGapThenRepair if !semantic_repair_present => {
        AssessmentJudgment::Insufficient {
          reason: "semantic implementation is missing".into(),
          proposals: Vec::new(),
          gap_kind: GapKind::Implementation,
        }
      }
      BackendMode::SemanticGapThenRepair => AssessmentJudgment::Insufficient {
        reason: "implementation exists but authoritative evidence is unavailable".into(),
        proposals: Vec::new(),
        gap_kind: GapKind::Evidence,
      },
      mode if matches!(mode, BackendMode::IncompleteAssessment) || !satisfied => {
        AssessmentJudgment::Contradicted {
          artifact_ids: Vec::new(),
          rationale: "diamond implementation is incomplete".into(),
          proposals: Vec::new(),
        }
      }
      _ => AssessmentJudgment::Supported {
        artifact_ids: Vec::new(),
        rationale: "A, B, C, and D exist in the immutable revision".into(),
      },
    };
    Ok(SemanticAssessmentProposal {
      summary: "independent semantic assessment".into(),
      assessments: vec![AgentObligationAssessment {
        obligation_handle: obligation_handles
          .get(&ObligationId::from("REQ-001/AC-01/VO-01"))
          .expect("controller supplies the obligation handle")
          .clone(),
        judgment: assessment,
      }],
    })
  }
}

async fn approve_active_catalog(repository: &Path) -> CatalogApproval {
  let catalog = store::read_catalog(repository)
    .await
    .expect("load catalog")
    .expect("active catalog");
  let approval = CatalogApproval {
    spec_hash: catalog.spec_hash.clone(),
    catalog_hash: catalog.catalog_hash().expect("hash catalog"),
    approved_at: Utc::now(),
  };
  store::write_catalog_approval(repository, &approval)
    .await
    .expect("approve catalog");
  approval
}

async fn configured_controller(
  repository: &TempRepo,
  backend: Arc<FakeBackend>,
  max_parallel_workers: usize,
) -> (Controller, mpsc::UnboundedReceiver<RunEvent>) {
  *backend.canonical.lock().expect("canonical path lock") = Some(repository.path().to_path_buf());
  let mut config = Config::default();
  config.agent.custom = Some(CustomAgentConfig {
    command: "unused".into(),
    args: Vec::new(),
    env: BTreeMap::new(),
  });
  config.execution.max_parallel_workers = max_parallel_workers;
  config.verification.checks = vec![ProjectVerificationCheck {
    name: "project verification".into(),
    command: vec!["git".into(), "diff".into(), "--check".into()],
    working_directory: ".".into(),
    environment: BTreeMap::new(),
    timeout_secs: None,
  }];
  fs::write(
    repository.path().join("tenet.toml"),
    toml::to_string_pretty(&config).expect("serialize config"),
  )
  .await
  .expect("write config");
  fs::create_dir_all(repository.path().join(".tenet"))
    .await
    .expect("create tenet dir");
  fs::write(repository.path().join("spec.md"), "diamond")
    .await
    .expect("write spec");
  run_git(repository.path(), &["add", "tenet.toml", "spec.md"]);
  run_git(repository.path(), &["commit", "-m", "configure"]);
  let (_, spec_hash) = store::spec_text_and_hash(repository.path(), &config)
    .await
    .expect("hash spec");
  store::write_catalog(
    repository.path(),
    &RequirementCatalog {
      spec_hash,
      requirements: vec![requirement()],
      acceptance_criteria: vec![criterion()],
      verification_obligations: vec![obligation()],
      coverage: CatalogCoverage::derive("diamond", &[requirement()]),
    },
  )
  .await
  .expect("write catalog");
  approve_active_catalog(repository.path()).await;
  let (sender, receiver) = mpsc::unbounded_channel();
  (
    Controller::new(
      repository.path().to_path_buf(),
      backend,
      EventSink::new(Some(sender)),
    ),
    receiver,
  )
}
async fn run_after_approval_if_required(
  controller: &Controller,
  repository: &Path,
) -> Result<State> {
  let state = controller.run(CancellationToken::new()).await?;
  if state.status != RunStatus::ReviewRequired {
    return Ok(state);
  }
  approve_active_catalog(repository).await;
  controller.run(CancellationToken::new()).await
}

#[tokio::test]
async fn manual_verify_runs_project_suite_without_requirement_catalog() {
  let repository = TempRepo::new();
  let mut config = Config::default();
  config.agent.custom = Some(CustomAgentConfig {
    command: "unused".into(),
    args: Vec::new(),
    env: BTreeMap::new(),
  });
  config.verification.checks = vec![ProjectVerificationCheck {
    name: "tracked file".into(),
    command: vec!["sh".into(), "-c".into(), "test -f README.txt".into()],
    working_directory: ".".into(),
    environment: BTreeMap::new(),
    timeout_secs: None,
  }];
  fs::write(
    repository.path().join("tenet.toml"),
    toml::to_string_pretty(&config).expect("serialize config"),
  )
  .await
  .expect("write config");
  run_git(repository.path(), &["add", "tenet.toml"]);
  run_git(
    repository.path(),
    &["commit", "-m", "configure verification"],
  );

  let report = manual_verify(repository.path())
    .await
    .expect("manual project verification");

  assert!(report.passed);
  assert_eq!(report.checks[0].name, "tracked file");
  assert!(!repository.path().join(".tenet/requirements.json").exists());
}
#[tokio::test]
async fn newly_generated_catalog_requires_review_before_reconciliation() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, mut events) = configured_controller(&repository, backend.clone(), 1).await;
  reset_storage(repository.path()).await;

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("catalog generation stops for review");

  assert_eq!(state.status, RunStatus::ReviewRequired);
  assert_eq!(state.phase, Phase::ReviewingRequirements);
  assert!(state.blocked_reason.is_none());
  assert_eq!(backend.reconcile_calls.load(Ordering::SeqCst), 0);
  let messages = std::iter::from_fn(|| events.try_recv().ok())
    .filter_map(|event| match event {
      RunEvent::Message(message) => Some(message),
      _ => None,
    })
    .collect::<Vec<_>>();
  assert!(messages.iter().any(|message| {
    message.contains("Human approval is required")
      && message.contains("tenet requirements dump")
      && message.contains("tenet requirements approve")
  }));
}

#[tokio::test]
async fn unapproved_cached_catalog_does_not_invoke_architect_again() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  reset_storage(repository.path()).await;
  let first = controller
    .run(CancellationToken::new())
    .await
    .expect("first run requires review");
  let architect_calls = backend.architect_calls.load(Ordering::SeqCst);

  let second = controller
    .run(CancellationToken::new())
    .await
    .expect("cached catalog still requires review");

  assert_eq!(first.status, RunStatus::ReviewRequired);
  assert_eq!(second.status, RunStatus::ReviewRequired);
  assert_eq!(
    backend.architect_calls.load(Ordering::SeqCst),
    architect_calls
  );
  assert_eq!(backend.reconcile_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn exact_catalog_approval_allows_later_run_to_reconcile() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend.clone(), 2).await;
  reset_storage(repository.path()).await;
  let first = controller
    .run(CancellationToken::new())
    .await
    .expect("first run requires review");
  assert_eq!(first.status, RunStatus::ReviewRequired);
  approve_active_catalog(repository.path()).await;

  let second = controller
    .run(CancellationToken::new())
    .await
    .expect("approved run succeeds");

  assert_eq!(second.status, RunStatus::Done);
  assert!(backend.reconcile_calls.load(Ordering::SeqCst) > 0);
}

#[tokio::test]
async fn changed_specification_invalidates_approval_and_stops_before_reconciliation() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  fs::write(repository.path().join("spec.md"), "changed requirement")
    .await
    .expect("change specification");
  run_git(repository.path(), &["add", "spec.md"]);
  run_git(repository.path(), &["commit", "-m", "change specification"]);

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("changed catalog requires review");
  let catalog = store::read_catalog(repository.path())
    .await
    .expect("load catalog")
    .expect("active catalog");

  assert_eq!(state.status, RunStatus::ReviewRequired);
  assert_eq!(backend.reconcile_calls.load(Ordering::SeqCst), 0);
  assert!(!store::catalog_is_approved(repository.path(), &catalog)
    .await
    .expect("check approval"));
}

#[tokio::test]
async fn approved_unchanged_catalog_continues_without_human_interruption() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend.clone(), 2).await;

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("approved catalog proceeds");

  assert_eq!(state.status, RunStatus::Done);
  assert_eq!(backend.architect_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn missing_project_checks_block_before_reconciliation() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  let mut config = read_config(repository.path()).await.expect("read config");
  config.verification.checks.clear();
  fs::write(
    repository.path().join("tenet.toml"),
    toml::to_string_pretty(&config).expect("serialize config"),
  )
  .await
  .expect("write config");
  run_git(repository.path(), &["add", "tenet.toml"]);
  run_git(
    repository.path(),
    &["commit", "-m", "remove project checks"],
  );

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("missing checks produce blocked state");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Blocked);
  assert!(state
    .blocked_reason
    .as_deref()
    .is_some_and(|reason| reason.contains("[[verification.checks]]")));
  assert_eq!(backend.reconcile_calls.load(Ordering::SeqCst), 0);
}

async fn assert_read_only_role_is_isolated(role: tenet_domain::model::WorkerRole, commit: bool) {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::ReadOnlyMutation {
    role,
    commit,
  }));
  let (controller, _) = configured_controller(&repository, backend, 2).await;
  if role == tenet_domain::model::WorkerRole::Architect {
    reset_storage(repository.path()).await;
  }
  let head_before = git::head(repository.path()).await.expect("head before run");

  let state = run_after_approval_if_required(&controller, repository.path())
    .await
    .expect("isolated read-only mutation cannot affect the run");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert!(!repository.path().join("read-only-mutation.txt").exists());
  assert_ne!(
    git::head(repository.path()).await.expect("head after run"),
    head_before,
    "normal candidate integrations should still advance HEAD"
  );
  let history = run_git(repository.path(), &["log", "--format=%s"]);

  assert!(!history.contains("malicious read-only mutation"));
}
#[tokio::test]
async fn large_specification_is_architected_in_bounded_complete_batches() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::LargeCatalog));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  let specification = (1..=200)
    .map(|index| format!("Normative requirement {index}."))
    .collect::<Vec<_>>()
    .join("\n\n");
  fs::write(repository.path().join("spec.md"), &specification)
    .await
    .expect("write large specification");
  run_git(repository.path(), &["add", "spec.md"]);
  run_git(
    repository.path(),
    &["commit", "-m", "add large specification"],
  );

  let state = run_after_approval_if_required(&controller, repository.path())
    .await
    .expect("large catalog proceeds through the controller");
  let catalog = store::read_catalog(repository.path())
    .await
    .expect("read catalog")
    .expect("persisted catalog");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert_eq!(backend.architect_calls.load(Ordering::SeqCst), 4);
  assert_eq!(catalog.coverage.normative_fragments.len(), 200);
  assert!(catalog.coverage.is_complete());
  assert_eq!(
    catalog
      .requirements
      .iter()
      .flat_map(|requirement| &requirement.source_refs)
      .count(),
    200
  );
}

#[tokio::test]
async fn architect_uncommitted_mutation_is_discarded() {
  assert_read_only_role_is_isolated(tenet_domain::model::WorkerRole::Architect, false).await;
}

#[tokio::test]
async fn architect_committed_mutation_is_discarded() {
  assert_read_only_role_is_isolated(tenet_domain::model::WorkerRole::Architect, true).await;
}

#[tokio::test]
async fn reconcile_uncommitted_mutation_is_discarded() {
  assert_read_only_role_is_isolated(tenet_domain::model::WorkerRole::Reconcile, false).await;
}

#[tokio::test]
async fn reconcile_committed_mutation_is_discarded() {
  assert_read_only_role_is_isolated(tenet_domain::model::WorkerRole::Reconcile, true).await;
}

#[tokio::test]
async fn assess_uncommitted_mutation_is_discarded_after_final_verification() {
  assert_read_only_role_is_isolated(tenet_domain::model::WorkerRole::Assess, false).await;
}

#[tokio::test]
async fn assess_committed_mutation_is_discarded_after_final_verification() {
  assert_read_only_role_is_isolated(tenet_domain::model::WorkerRole::Assess, true).await;
}
#[tokio::test]
async fn ignored_specification_is_available_to_every_agent_workspace() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::RequireSpecification));
  let (controller, _) = configured_controller(&repository, backend, 1).await;
  fs::write(repository.path().join(".gitignore"), ".tenet/\nspec.md\n")
    .await
    .expect("ignore specification");
  run_git(repository.path(), &["add", ".gitignore"]);
  run_git(repository.path(), &["rm", "--cached", "spec.md"]);
  run_git(
    repository.path(),
    &["commit", "-m", "keep specification controller-owned"],
  );
  reset_storage(repository.path()).await;

  let state = run_after_approval_if_required(&controller, repository.path())
    .await
    .expect("agents can read the ignored authoritative specification");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
}

#[tokio::test]
async fn incomplete_catalog_is_retried_with_coverage_feedback() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::IncompleteCatalogThenCorrect));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  fs::write(
    repository.path().join("spec.md"),
    "First normative statement.\n\nSecond normative statement.",
  )
  .await
  .expect("write multi-fragment specification");
  run_git(repository.path(), &["add", "spec.md"]);
  run_git(repository.path(), &["commit", "-m", "expand specification"]);

  let state = run_after_approval_if_required(&controller, repository.path())
    .await
    .expect("corrected catalog proceeds");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert_eq!(backend.architect_calls.load(Ordering::SeqCst), 2);
  let feedback = backend
    .semantic_feedback
    .lock()
    .expect("semantic feedback lock");
  assert_eq!(feedback.len(), 1);
  assert_eq!(feedback[0].0, WorkerRole::Architect);
  assert!(feedback[0]
    .1
    .contains("catalog does not cover normative specification fragments"));
}

#[tokio::test]
async fn incomplete_catalog_exhausts_configured_retries_with_precise_error() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::AlwaysIncompleteCatalog));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  let specification = "First normative statement.\n\nSecond normative statement.";
  fs::write(repository.path().join("spec.md"), specification)
    .await
    .expect("write multi-fragment specification");
  run_git(repository.path(), &["add", "spec.md"]);
  run_git(repository.path(), &["commit", "-m", "expand specification"]);
  rewrite_config(&repository, |config| config.agent.completion_retries = 1).await;
  let uncovered = derive_normative_fragments(specification)[1].id.clone();

  let error = controller
    .run(CancellationToken::new())
    .await
    .expect_err("incomplete catalog exhausts retries");

  assert_eq!(
    error.to_string(),
    format!("catalog does not cover normative specification fragments: {uncovered}")
  );
  assert_eq!(backend.architect_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn directory_scope_is_retried_with_recursive_glob_feedback() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::InvalidScopeThenCorrect));
  let (controller, _) = configured_controller(&repository, backend.clone(), 2).await;

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("corrected recursive scope proceeds");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  let feedback = backend
    .semantic_feedback
    .lock()
    .expect("semantic feedback lock");
  assert_eq!(feedback.len(), 1);
  assert_eq!(feedback[0].0, WorkerRole::Reconcile);
  assert!(feedback[0].1.contains(
    "A scope path \"src/\" does not include descendants; use \"src/**\" or explicit files"
  ));
  assert!(!feedback[0]
    .1
    .contains("Do not guess, repair, or normalize identifiers"));
  assert!(!feedback[0].1.contains("correct the reported relationships"));
}

#[tokio::test]
async fn mechanical_proof_skips_assessment_scope_retry() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(
    BackendMode::InvalidAssessmentScopeThenCorrect,
  ));
  let (controller, _) = configured_controller(&repository, backend.clone(), 2).await;

  let state = controller.run(CancellationToken::new()).await.expect("run");
  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert_eq!(backend.assessment_calls.load(Ordering::SeqCst), 0);
  assert!(backend
    .semantic_feedback
    .lock()
    .expect("feedback lock")
    .is_empty());
}

#[tokio::test]
async fn malformed_reconciliation_is_retried_with_feedback_and_corrected() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::InvalidReconcileThenCorrect));
  let (controller, _) = configured_controller(&repository, backend.clone(), 2).await;

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("corrected reconciliation proceeds");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert!(!repository
    .path()
    .join("semantic-reconcile-attempt.txt")
    .exists());
  let feedback = backend
    .semantic_feedback
    .lock()
    .expect("semantic feedback lock");
  assert_eq!(feedback.len(), 1);
  assert_eq!(feedback[0].0, WorkerRole::Reconcile);
  assert!(feedback[0]
    .1
    .contains("A targets unknown verification obligation REQ-999/AC-01/VO-01"));
  assert!(feedback[0]
    .1
    .contains("correct this semantic validation failure"));
  assert!(!feedback[0].1.contains("criterion relationships"));
}

#[tokio::test]
async fn structural_and_semantic_retries_cannot_multiply_completion_budget() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(
    BackendMode::StructuralRetryThenInvalidReconcile,
  ));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  rewrite_config(&repository, |config| config.agent.completion_retries = 1).await;

  let error = controller
    .run(CancellationToken::new())
    .await
    .expect_err("shared completion budget must be exhausted");

  assert!(error
    .to_string()
    .contains("agent completion attempt budget exhausted after 2 completion(s)"));
  assert_eq!(backend.reconcile_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn contradictory_reconciliation_is_retried_with_targeted_feedback_and_corrected() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(
    BackendMode::ContradictoryReconcileThenCorrect,
  ));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("corrected reconciliation proceeds");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  let feedback = backend
    .semantic_feedback
    .lock()
    .expect("semantic feedback lock");
  assert_eq!(feedback.len(), 1);
  assert_eq!(feedback[0].0, WorkerRole::Reconcile);
  assert!(feedback[0].1.contains("REQ-001 is internally inconsistent"));
  assert!(feedback[0]
    .1
    .contains("implementationState=\"present\" means all required implementation exists"));
  assert!(feedback[0]
    .1
    .contains("choose \"partial\", \"absent\", or \"unknown\""));
  assert!(!feedback[0]
    .1
    .contains("Do not guess, repair, or normalize identifiers"));
}

#[tokio::test]
async fn mechanical_proof_does_not_require_assessment_retries() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::InvalidAssessmentThenCorrect));
  let (controller, _) = configured_controller(&repository, backend.clone(), 2).await;

  let state = controller.run(CancellationToken::new()).await.expect("run");
  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert_eq!(backend.assessment_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn exhausted_semantic_retries_return_precise_validation_error() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::AlwaysInvalidReconcile));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  rewrite_config(&repository, |config| config.agent.completion_retries = 1).await;

  let error = controller
    .run(CancellationToken::new())
    .await
    .expect_err("invalid reconciliation exhausts retries");

  assert_eq!(
    error.to_string(),
    "A targets unknown verification obligation REQ-999/AC-01/VO-01"
  );
  assert_eq!(backend.reconcile_calls.load(Ordering::SeqCst), 2);
  let feedback = backend
    .semantic_feedback
    .lock()
    .expect("semantic feedback lock");
  assert_eq!(feedback.len(), 1);
  assert!(feedback.iter().all(
    |(role, message)| *role == WorkerRole::Reconcile && message.contains("REQ-999/AC-01/VO-01")
  ));
}

#[tokio::test]
async fn contradictory_reconciliation_fails_after_retry_exhaustion() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::AlwaysContradictoryReconcile));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  rewrite_config(&repository, |config| config.agent.completion_retries = 1).await;

  let error = controller
    .run(CancellationToken::new())
    .await
    .expect_err("contradictory reconciliation exhausts retries");

  assert_eq!(
    error.to_string(),
    "present implementation REQ-001 cannot report implementation gaps"
  );
  assert_eq!(backend.reconcile_calls.load(Ordering::SeqCst), 2);
  let feedback = backend
    .semantic_feedback
    .lock()
    .expect("semantic feedback lock");
  assert_eq!(feedback.len(), 1);
  assert!(feedback[0].1.contains("REQ-001 is internally inconsistent"));
}

#[tokio::test]
async fn diamond_executes_independent_units_in_parallel_and_integrates_by_id() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, mut events) = configured_controller(&repository, backend.clone(), 2).await;

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("diamond run succeeds");

  assert_eq!(state.completed_work_units.len(), 4);
  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert!(backend.max_active.load(Ordering::SeqCst) >= 2);
  {
    let workspaces = backend.workspaces.lock().expect("workspace lock");
    let b = workspaces
      .iter()
      .find(|(id, _)| id == "B")
      .expect("B workspace");
    let c = workspaces
      .iter()
      .find(|(id, _)| id == "C")
      .expect("C workspace");
    assert_ne!(b.1, c.1);
  }
  assert!(backend.discoveries_seen.load(Ordering::SeqCst) > 0);

  let mut integrations = Vec::new();
  let mut worker_ids = std::collections::BTreeSet::new();
  let mut candidates = Vec::new();
  while let Ok(event) = events.try_recv() {
    match event {
      RunEvent::IntegrationAccepted { work_unit_id, .. } => integrations.push(work_unit_id),
      RunEvent::WorkerStarted { worker_id, .. } => {
        worker_ids.insert(worker_id);
      }
      RunEvent::CandidateProduced(candidate) => candidates.push(candidate),
      _ => {}
    }
  }
  assert_eq!(integrations, ["A", "B", "C", "D"]);
  assert_eq!(worker_ids.len(), 4);
  assert_eq!(candidates.len(), 4);
  let catalog = store::read_catalog(repository.path())
    .await
    .expect("read catalog")
    .expect("catalog");
  let graph = controller_evidence::load(repository.path(), &catalog)
    .await
    .expect("read evidence graph");
  let head = git::head(repository.path()).await.expect("head");
  let derivation = &graph.proof_derivations[&ObligationId::from("REQ-001/AC-01/VO-01")];
  assert_eq!(derivation.revision, head);
  assert_eq!(derivation.state, ProofState::Proven);
  let artifact_id = match &derivation.reason {
    tenet_domain::proof::ProofReason::Artifact { artifact_id, .. } => *artifact_id,
    reason => panic!("expected artifact proof, got {reason:?}"),
  };
  assert_eq!(graph.artifacts[&artifact_id].revision, head);

  let b_candidate = candidates
    .iter()
    .find(|item| item.lease.work_unit.id == "B")
    .expect("B candidate");
  let c_candidate = candidates
    .iter()
    .find(|item| item.lease.work_unit.id == "C")
    .expect("C candidate");
  assert_eq!(b_candidate.base_revision, c_candidate.base_revision);
  assert_ne!(
    b_candidate.candidate_revision,
    c_candidate.candidate_revision
  );
  assert!(candidates
    .iter()
    .all(|item| item.base_revision == item.lease.base_revision));
  assert!(candidates.iter().all(|item| !item.changed_paths.is_empty()));
  assert!(["A", "B", "C", "D"]
    .iter()
    .all(|id| repository.path().join(format!("{id}.txt")).exists()));
}
async fn assert_out_of_scope_change_is_preserved_and_reauthorized(mutation: ScopeMutation) {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::ScopeMutation(mutation)));
  let (controller, mut events) = configured_controller(&repository, backend.clone(), 1).await;
  let head_before = git::head(repository.path()).await.expect("head before run");

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("scope expansion is reconsidered by reconciliation");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert_ne!(
    git::head(repository.path()).await.expect("canonical head"),
    head_before
  );
  let expected_paths: Vec<String> = match mutation {
    ScopeMutation::Modification | ScopeMutation::Deletion => vec!["README.txt".into()],
    ScopeMutation::Addition => vec!["outside.txt".into()],
    ScopeMutation::Rename => vec!["renamed.txt".into()],
  };
  let a_attempts = backend
    .workspaces
    .lock()
    .expect("workspace lock")
    .iter()
    .filter(|(id, _)| id == "A")
    .count();
  assert_eq!(a_attempts, 1, "authorized candidate must be reused");
  let mut produced_a = 0;
  let mut observed_expansion = false;
  while let Ok(event) = events.try_recv() {
    match event {
      RunEvent::CandidateProduced(candidate) if candidate.lease.work_unit.id == "A" => {
        produced_a += 1;
      }
      RunEvent::State(snapshot) => {
        observed_expansion |= snapshot.discoveries.iter().any(|record| {
          matches!(
            &record.discovery,
            Discovery::ScopeExpansion { paths, reason }
              if paths == &expected_paths && reason.starts_with("Controller observed")
          )
        });
      }
      _ => {}
    }
  }
  assert!(observed_expansion);
  assert_eq!(
    produced_a, 1,
    "candidate must become integrable only after reauthorization"
  );
  assert!(state.deferred_candidates.is_empty());
  assert!(run_git(
    repository.path(),
    &[
      "for-each-ref",
      "--format=%(refname)",
      "refs/tenet/candidates"
    ]
  )
  .is_empty());
}

#[tokio::test]
async fn candidate_modification_outside_scope_is_replanned() {
  assert_out_of_scope_change_is_preserved_and_reauthorized(ScopeMutation::Modification).await;
}

#[tokio::test]
async fn candidate_addition_outside_scope_is_replanned() {
  assert_out_of_scope_change_is_preserved_and_reauthorized(ScopeMutation::Addition).await;
}

#[tokio::test]
async fn candidate_deletion_outside_scope_is_replanned() {
  assert_out_of_scope_change_is_preserved_and_reauthorized(ScopeMutation::Deletion).await;
}

#[tokio::test]
async fn candidate_rename_outside_scope_is_replanned() {
  assert_out_of_scope_change_is_preserved_and_reauthorized(ScopeMutation::Rename).await;
}

#[tokio::test]
async fn scope_expansion_candidate_survives_resume_without_reimplementation() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::ScopeMutation(
    ScopeMutation::Addition,
  )));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  rewrite_config(&repository, |config| config.max_cycles = 1).await;

  let first = controller
    .run(CancellationToken::new())
    .await
    .expect("first run preserves deferred candidate");
  assert_eq!(first.status, tenet_domain::model::RunStatus::Blocked);
  assert_eq!(first.deferred_candidates.len(), 1);
  assert!(!run_git(
    repository.path(),
    &[
      "for-each-ref",
      "--format=%(refname)",
      "refs/tenet/candidates"
    ]
  )
  .is_empty());

  let second = controller
    .run(CancellationToken::new())
    .await
    .expect("resumed run reuses deferred candidate");

  let a_attempts = backend
    .workspaces
    .lock()
    .expect("workspace lock")
    .iter()
    .filter(|(id, _)| id == "A")
    .count();
  assert_eq!(second.status, tenet_domain::model::RunStatus::Blocked);
  assert_eq!(a_attempts, 1, "resume must not rerun implementation A");
  assert!(second.deferred_candidates.is_empty());
  assert!(run_git(
    repository.path(),
    &[
      "for-each-ref",
      "--format=%(refname)",
      "refs/tenet/candidates"
    ]
  )
  .is_empty());
}

#[tokio::test]
async fn stale_deferred_candidate_is_rejected_and_its_ref_is_removed() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::ScopeMutation(
    ScopeMutation::Addition,
  )));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  rewrite_config(&repository, |config| config.max_cycles = 1).await;

  let first = controller
    .run(CancellationToken::new())
    .await
    .expect("first run preserves deferred candidate");
  let first_ref = first.deferred_candidates[0].git_ref.clone();
  std::fs::write(repository.path().join("user-change.txt"), "advance base")
    .expect("write user change");
  run_git(repository.path(), &["add", "user-change.txt"]);
  run_git(
    repository.path(),
    &["commit", "-m", "advance canonical base"],
  );

  let second = controller
    .run(CancellationToken::new())
    .await
    .expect("stale candidate is replanned");
  let references = run_git(
    repository.path(),
    &[
      "for-each-ref",
      "--format=%(refname)",
      "refs/tenet/candidates",
    ],
  );
  let a_attempts = backend
    .workspaces
    .lock()
    .expect("workspace lock")
    .iter()
    .filter(|(id, _)| id == "A")
    .count();

  assert_eq!(second.status, tenet_domain::model::RunStatus::Blocked);
  assert_eq!(a_attempts, 2, "stale candidate must not be reused");
  assert!(!references.contains(&first_ref));
}
#[tokio::test]
async fn repair_candidate_outside_scope_is_preserved_and_reauthorized() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::RepairScopeMutation));
  let (controller, mut events) = configured_controller(&repository, backend.clone(), 1).await;

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("repair scope expansion is reconsidered");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert_eq!(backend.repair_calls.load(Ordering::SeqCst), 1);
  assert!(repository.path().join("outside.txt").exists());
  let mut observed_expansion = false;
  while let Ok(event) = events.try_recv() {
    if let RunEvent::State(snapshot) = event {
      observed_expansion |= snapshot.discoveries.iter().any(|record| {
        record.role == WorkerRole::Repair
          && matches!(
            &record.discovery,
            Discovery::ScopeExpansion { paths, reason }
              if paths == &["outside.txt".to_owned()] && reason.starts_with("Controller observed")
          )
      });
    }
  }
  assert!(observed_expansion);
}

#[tokio::test]
async fn repeated_unapproved_scope_expansion_is_bounded_by_stagnation() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::UnapprovedScopeMutation));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  rewrite_config(&repository, |config| {
    config.max_cycles = 10;
    config.stagnation_limit = 1;
  })
  .await;
  let head_before = git::head(repository.path()).await.expect("head before run");

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("stagnation blocks repeated unapproved expansion");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Blocked);
  assert_eq!(state.cycle, 2);
  assert!(state
    .blocked_reason
    .as_deref()
    .is_some_and(|reason| reason.contains("Stagnation limit (1)")));
  assert_eq!(
    git::head(repository.path()).await.expect("canonical head"),
    head_before
  );
  assert!(!repository.path().join("A.txt").exists());
  assert!(!repository.path().join("outside.txt").exists());
  let a_attempts = backend
    .workspaces
    .lock()
    .expect("workspace lock")
    .iter()
    .filter(|(id, _)| id == "A")
    .count();
  assert_eq!(
    a_attempts, 1,
    "unapproved scope must not rerun implementation"
  );
  assert_eq!(state.deferred_candidates.len(), 1);
  assert!(!run_git(
    repository.path(),
    &[
      "for-each-ref",
      "--format=%(refname)",
      "refs/tenet/candidates"
    ]
  )
  .is_empty());
}

async fn run_with_historical_a(mut historical: WorkUnit) {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend, 2).await;
  historical.id = "A".into();
  let mut state = State::fresh();
  state.run_id = Some("historical-run".into());
  state.status = tenet_domain::model::RunStatus::Running;
  state.phase = tenet_domain::model::Phase::Reconciling;
  state.cycle = 1;
  store::write_state(repository.path(), &state)
    .await
    .expect("persist historical run");
  let revision = git::head(repository.path()).await.expect("historical head");
  let catalog = store::read_catalog(repository.path())
    .await
    .expect("read historical catalog")
    .expect("historical catalog");
  store::write_roadmap(
    repository.path(),
    "historical-run",
    1,
    &revision,
    &catalog.spec_hash,
    &ReconcileResult {
      summary: "historical".into(),
      requirements: Vec::new(),
      work_units: vec![historical.clone()],
    },
  )
  .await
  .expect("persist historical roadmap");
  let verification = passing_project_verification();
  store::record_manual_verification(repository.path(), "historical-run", &verification)
    .await
    .expect("persist historical verification");
  state.completed_work_units.push(CompletedWorkUnit {
    work_unit: historical,
    completed_at: "historical".into(),
    verification_run_id: verification.run_id,
  });
  store::write_state(repository.path(), &state)
    .await
    .expect("persist historical completion");

  let result = controller
    .run(CancellationToken::new())
    .await
    .expect("current reconciliation must outrank historical completion");

  assert_eq!(result.status, tenet_domain::model::RunStatus::Done);
  assert!(repository.path().join("A.txt").exists());
}

#[tokio::test]
async fn reused_work_id_with_changed_objective_is_not_suppressed() {
  let mut historical = unit("A", &[]);
  historical.objective = "obsolete objective".into();
  run_with_historical_a(historical).await;
}

#[tokio::test]
async fn reused_work_id_with_changed_scope_is_not_suppressed() {
  let mut historical = unit("A", &[]);
  historical.scope.paths = vec!["obsolete/**".into()];
  run_with_historical_a(historical).await;
}

#[tokio::test]
async fn manual_repository_regression_is_not_hidden_by_completion_history() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend, 2).await;
  controller
    .run(CancellationToken::new())
    .await
    .expect("first run completes");
  std::fs::remove_file(repository.path().join("A.txt")).expect("regress repository");
  run_git(repository.path(), &["add", "-A"]);
  run_git(repository.path(), &["commit", "-m", "manual regression"]);

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("second run repairs current repository reality");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert!(repository.path().join("A.txt").exists());
}

#[tokio::test]
async fn repository_rewind_is_not_hidden_by_completion_history() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend, 2).await;
  let before_work = git::head(repository.path())
    .await
    .expect("pre-work revision");
  controller
    .run(CancellationToken::new())
    .await
    .expect("first run completes");
  run_git(repository.path(), &["reset", "--hard", &before_work]);

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("rewound repository is reconciled again");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert!(repository.path().join("A.txt").exists());
}

#[tokio::test]
async fn specification_change_invalidates_completion_and_discovery_history() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend, 2).await;
  let mut first = controller
    .run(CancellationToken::new())
    .await
    .expect("first run completes");
  assert!(!first.completed_work_units.is_empty());
  let catalog = store::read_catalog(repository.path())
    .await
    .expect("read catalog")
    .expect("catalog exists");
  first.discoveries.push(DiscoveryRecord {
    fingerprint: "stale".into(),
    discovery: Discovery::Blocker {
      description: "old catalog blocker".into(),
    },
    catalog_hash: catalog.spec_hash,
    repository_revision: git::head(repository.path()).await.expect("current head"),
    work_unit_id: "A".into(),
    role: WorkerRole::Implement,
    cycle: first.cycle,
    status: DiscoveryStatus::Active,
  });
  store::write_state(repository.path(), &first)
    .await
    .expect("persist stale discovery");
  fs::write(repository.path().join("spec.md"), "changed specification")
    .await
    .expect("change specification");
  run_git(repository.path(), &["add", "spec.md"]);
  run_git(repository.path(), &["commit", "-m", "change specification"]);

  let second = run_after_approval_if_required(&controller, repository.path())
    .await
    .expect("changed specification starts a new catalog context");

  assert_eq!(second.status, tenet_domain::model::RunStatus::Done);
  assert!(second.completed_work_units.is_empty());
  assert!(second.discoveries.is_empty());
}
#[tokio::test]
async fn concurrency_one_preserves_sequential_execution() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;

  controller
    .run(CancellationToken::new())
    .await
    .expect("sequential run succeeds");

  assert_eq!(backend.max_active.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn repair_discovery_reaches_next_reconciliation_once() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::RepairDiscovery));
  let (controller, mut events) = configured_controller(&repository, backend.clone(), 2).await;

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("repair discovery run completes");

  assert_eq!(
    state.status,
    tenet_domain::model::RunStatus::Done,
    "blocked={:?} error={:?}",
    state.blocked_reason,
    state.last_error
  );
  assert_eq!(backend.discoveries_seen.load(Ordering::SeqCst), 2);
  let repair_state = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| {
    let RunEvent::State(state) = event else {
      return None;
    };
    (state.phase == tenet_domain::model::Phase::Repairing).then_some(state)
  });
  let repair = repair_state
    .and_then(|state| state.current_repair)
    .expect("repair phase includes current repair details");
  assert_eq!(repair.work_unit_id, "A");
  assert_eq!(repair.attempt, 1);
}

#[tokio::test]
async fn invalid_verification_discovery_returns_to_reconciliation_without_repair_loop() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::InvalidVerificationCheck));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  let mut config = read_config(repository.path()).await.expect("read config");
  config.max_cycles = 1;
  fs::write(
    repository.path().join("tenet.toml"),
    toml::to_string_pretty(&config).expect("serialize config"),
  )
  .await
  .expect("limit cycles");
  run_git(repository.path(), &["add", "tenet.toml"]);
  run_git(repository.path(), &["commit", "-m", "limit cycles"]);

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("invalid verification is deferred to reconciliation");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Blocked);
  assert_eq!(backend.repair_calls.load(Ordering::SeqCst), 1);
  assert!(state
    .discoveries
    .iter()
    .any(|record| matches!(record.discovery, Discovery::VerificationBlocker { .. })));
  assert_eq!(state.deferred_candidates.len(), 1);
  assert!(!run_git(
    repository.path(),
    &[
      "for-each-ref",
      "--format=%(refname)",
      "refs/tenet/candidates",
    ],
  )
  .is_empty());
  let list = run_git(repository.path(), &["worktree", "list", "--porcelain"]);
  assert_eq!(list.matches("worktree ").count(), 1);
}

#[tokio::test]
async fn deferred_candidate_cannot_reset_repair_budget() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::InvalidVerificationCheck));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  rewrite_config(&repository, |config| {
    config.max_cycles = 2;
    config.max_repair_attempts = 1;
  })
  .await;

  controller
    .run(CancellationToken::new())
    .await
    .expect_err("resumed candidate cannot receive a second repair budget");

  assert_eq!(backend.repair_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_resumed_repair_keeps_consumed_attempt_persisted() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(
    BackendMode::VerificationBlockerThenRepairFailure,
  ));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  rewrite_config(&repository, |config| {
    config.max_cycles = 3;
    config.max_repair_attempts = 2;
  })
  .await;

  controller
    .run(CancellationToken::new())
    .await
    .expect_err("resumed repair fails after consuming final attempt");
  let failed = store::read_state(repository.path())
    .await
    .expect("failed state retains deferred candidate");
  assert_eq!(failed.deferred_candidates[0].repair_attempts, 2);

  controller
    .run(CancellationToken::new())
    .await
    .expect_err("exhausted deferred candidate cannot repair again");
  assert_eq!(backend.repair_calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn dirty_canonical_tree_is_rejected_before_worktree_execution() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend, 1).await;
  std::fs::write(repository.path().join("dirty.txt"), "uncommitted\n")
    .expect("dirty canonical tree");

  let error = controller
    .run(CancellationToken::new())
    .await
    .expect_err("dirty canonical tree must be rejected");

  assert!(error
    .to_string()
    .contains("worktree execution requires a clean canonical working tree"));
}

#[tokio::test]
async fn empty_implementation_is_repaired_in_the_assigned_worktree() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::EmptyImplementationThenRepair));
  let (controller, _) = configured_controller(&repository, backend.clone(), 2).await;

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("empty implementation is repaired");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert_eq!(backend.repair_calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn worker_failure_fails_closed_and_cleans_worktrees() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::FailB));
  let (controller, _) = configured_controller(&repository, backend, 2).await;

  let error = controller
    .run(CancellationToken::new())
    .await
    .expect_err("worker failure propagates");

  assert!(error.to_string().contains("implementation worker"));
  let list = run_git(repository.path(), &["worktree", "list", "--porcelain"]);
  assert_eq!(list.matches("worktree ").count(), 1);
}

#[tokio::test]
async fn worker_timeout_fails_closed_and_cleans_worktrees() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::WorkerTimeout));
  let (controller, _) = configured_controller(&repository, backend, 2).await;

  let error = controller
    .run(CancellationToken::new())
    .await
    .expect_err("worker timeout propagates");

  assert!(format!("{error:#}").contains("timed out"));
  let list = run_git(repository.path(), &["worktree", "list", "--porcelain"]);
  assert_eq!(list.matches("worktree ").count(), 1);
}

#[tokio::test]
async fn protected_file_mutation_fails_closed() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::ProtectedMutation));
  let (controller, _) = configured_controller(&repository, backend, 1).await;

  let error = controller
    .run(CancellationToken::new())
    .await
    .expect_err("protected mutation rejected");

  assert!(error.to_string().contains("controller-protected"));
}

#[tokio::test]
async fn cancellation_after_partial_completion_cleans_all_worktrees() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::WaitForCancellation));
  let (controller, _) = configured_controller(&repository, backend.clone(), 2).await;
  let cancellation = CancellationToken::new();
  let trigger = cancellation.clone();
  tokio::spawn(async move {
    backend.waiting.notified().await;
    trigger.cancel();
  });

  let state = controller
    .run(cancellation)
    .await
    .expect("cancellation returns stopped state");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Stopped);
  assert!(repository.path().join("A.txt").exists());
  assert!(!repository.path().join("B.txt").exists());
  assert!(!repository.path().join("C.txt").exists());
  let list = run_git(repository.path(), &["worktree", "list", "--porcelain"]);
  assert_eq!(list.matches("worktree ").count(), 1);
}

fn passing_project_config() -> Config {
  let mut config = Config::default();
  config.verification.checks = vec![ProjectVerificationCheck {
    name: "project verification".into(),
    command: vec!["true".into()],
    working_directory: ".".into(),
    environment: Default::default(),
    timeout_secs: None,
  }];
  config
}

fn passing_report() -> VerificationReport {
  VerificationReport {
    passed: true,
    started_at: "2026-08-16T10:00:00Z".parse().expect("valid timestamp"),
    finished_at: "2026-08-16T10:00:01Z".parse().expect("valid timestamp"),
    commands: Vec::new(),
    executions: Vec::new(),
    warnings: Vec::new(),
  }
}

fn passing_project_verification() -> ProjectVerificationRun {
  ProjectVerificationRun {
    revision: "revision".into(),
    run_id: tenet_domain::ids::VerificationRunId::new(),
    suite_hash: passing_project_config()
      .verification
      .suite_hash()
      .expect("suite hash"),
    checks: Vec::new(),
    passed: true,
    started_at: "2026-08-16T10:00:00Z".parse().expect("valid timestamp"),
    finished_at: "2026-08-16T10:00:01Z".parse().expect("valid timestamp"),
  }
}

async fn candidate(
  repository: &TempRepo,
  manager: &WorkspaceManager,
  id: &str,
  base: &str,
  path: &str,
  content: &str,
  suggested_checks: Vec<String>,
) -> WorkExecution {
  store::ensure_layout(repository.path())
    .await
    .expect("ensure integration storage");
  let persisted = store::read_state(repository.path())
    .await
    .expect("read integration state");
  if persisted.run_id.as_deref() != Some(manager.run_id()) {
    store::write_catalog(
      repository.path(),
      &RequirementCatalog {
        spec_hash: "integration-spec".into(),
        requirements: vec![requirement()],
        acceptance_criteria: vec![criterion()],
        verification_obligations: vec![obligation()],
        coverage: CatalogCoverage::derive("diamond", &[requirement()]),
      },
    )
    .await
    .expect("persist integration catalog");
    let mut state = State::fresh();
    state.run_id = Some(manager.run_id().into());
    state.status = tenet_domain::model::RunStatus::Running;
    state.phase = tenet_domain::model::Phase::Integrating;
    state.cycle = 1;
    store::write_state(repository.path(), &state)
      .await
      .expect("persist integration run");
    store::write_roadmap(
      repository.path(),
      manager.run_id(),
      1,
      base,
      "integration-spec",
      &ReconcileResult {
        summary: "integration fixtures".into(),
        requirements: Vec::new(),
        work_units: ["B", "C", "future", "check", "cancel"]
          .into_iter()
          .map(|work_unit_id| unit(work_unit_id, &[]))
          .collect(),
      },
    )
    .await
    .expect("persist integration work graph");
  }
  let lease_id = format!("lease-{id}-{}", Uuid::new_v4());
  let workspace = manager
    .create_worker(&lease_id, base)
    .await
    .expect("create candidate workspace");
  fs::write(workspace.join(path), content)
    .await
    .expect("write candidate file");
  let revision = git::commit_all(&workspace, &format!("candidate {id}"))
    .await
    .expect("commit candidate");
  let execution = WorkExecution {
    lease: WorkLease {
      id: lease_id,
      worker_id: format!("worker-{id}"),
      work_unit: WorkUnit {
        suggested_checks: suggested_checks
          .into_iter()
          .map(|command| CandidateCheck {
            obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
            command,
          })
          .collect(),
        ..unit(id, &[])
      },
      base_revision: base.into(),
      workspace: workspace.clone(),
      issued_at: "now".into(),
    },
    worker_summary: summary(Vec::new()),
    verification: passing_report(),
    base_revision: base.into(),
    candidate_revision: revision,
    changed_paths: vec![path.into()],
    discoveries: Vec::new(),
  };
  manager
    .remove(&workspace)
    .await
    .expect("remove candidate workspace");
  execution
}

#[tokio::test]
async fn integration_rejects_merge_conflicts_without_advancing_canonical_head() {
  let repository = TempRepo::new();
  let base = git::head(repository.path()).await.expect("base revision");
  let manager = WorkspaceManager::new(repository.path().to_path_buf(), "conflict");
  let first = candidate(
    &repository,
    &manager,
    "B",
    &base,
    "shared.txt",
    "B",
    Vec::new(),
  )
  .await;
  let second = candidate(
    &repository,
    &manager,
    "C",
    &base,
    "shared.txt",
    "C",
    Vec::new(),
  )
  .await;
  let mut integrator = Integrator::create(
    repository.path().to_path_buf(),
    &manager,
    base,
    passing_project_config(),
  )
  .await
  .expect("create integrator");
  assert!(matches!(
    integrator.integrate(&first).await.expect("integrate first"),
    IntegrationOutcome::Accepted { .. }
  ));
  let accepted = git::head(repository.path()).await.expect("accepted head");

  let outcome = integrator
    .integrate(&second)
    .await
    .expect("reject conflict");

  assert!(matches!(outcome, IntegrationOutcome::MergeConflict { .. }));
  assert_eq!(
    git::head(repository.path()).await.expect("canonical head"),
    accepted
  );
  integrator
    .cleanup(&manager)
    .await
    .expect("cleanup integration");
}

#[tokio::test]
async fn integration_rejects_stale_candidate_base() {
  let repository = TempRepo::new();
  let base = git::head(repository.path()).await.expect("base revision");
  let manager = WorkspaceManager::new(repository.path().to_path_buf(), "stale");
  let future = candidate(
    &repository,
    &manager,
    "future",
    &base,
    "future.txt",
    "future",
    Vec::new(),
  )
  .await;
  let mut stale = future.clone();
  stale.base_revision.clone_from(&future.candidate_revision);
  stale
    .lease
    .base_revision
    .clone_from(&future.candidate_revision);
  let mut integrator = Integrator::create(
    repository.path().to_path_buf(),
    &manager,
    base,
    passing_project_config(),
  )
  .await
  .expect("create integrator");

  let outcome = integrator
    .integrate(&stale)
    .await
    .expect("detect stale base");

  assert_eq!(outcome, IntegrationOutcome::StaleBase);
  integrator
    .cleanup(&manager)
    .await
    .expect("cleanup integration");
}

#[tokio::test]
async fn integration_rejects_candidate_and_project_verification_failures() {
  let repository = TempRepo::new();
  let base = git::head(repository.path()).await.expect("base revision");
  let checks_manager = WorkspaceManager::new(repository.path().to_path_buf(), "checks");
  let failed_check = candidate(
    &repository,
    &checks_manager,
    "check",
    &base,
    "check.txt",
    "bad",
    vec!["false".into()],
  )
  .await;
  let mut check_integrator = Integrator::create(
    repository.path().to_path_buf(),
    &checks_manager,
    base.clone(),
    passing_project_config(),
  )
  .await
  .expect("create check integrator");
  assert!(matches!(
    check_integrator
      .integrate(&failed_check)
      .await
      .expect("run candidate check"),
    IntegrationOutcome::VerificationFailed { .. }
  ));
  check_integrator
    .cleanup(&checks_manager)
    .await
    .expect("cleanup check integration");

  let project_manager = WorkspaceManager::new(repository.path().to_path_buf(), "project-check");
  let project_failure = candidate(
    &repository,
    &project_manager,
    "project-check",
    &base,
    "project-check",
    "present",
    Vec::new(),
  )
  .await;
  let mut config = Config::default();
  config.verification.checks = vec![ProjectVerificationCheck {
    name: "project-check".into(),
    command: vec!["sh".into(), "-c".into(), "test ! -f project-check".into()],
    working_directory: ".".into(),
    environment: Default::default(),
    timeout_secs: None,
  }];
  let mut project_integrator = Integrator::create(
    repository.path().to_path_buf(),
    &project_manager,
    base,
    config,
  )
  .await
  .expect("create project verification integrator");

  let outcome = project_integrator
    .integrate(&project_failure)
    .await
    .expect("run project verification");

  assert!(matches!(
    outcome,
    IntegrationOutcome::ProjectVerificationFailed { .. }
  ));
  project_integrator
    .cleanup(&project_manager)
    .await
    .expect("cleanup project verification integration");
}

#[cfg(unix)]
async fn repository_after_disposable_check(command: &str) -> (TempRepo, String, String) {
  use std::os::unix::fs::{symlink, PermissionsExt};

  let repository = TempRepo::new();
  std::fs::write(repository.path().join("tenet.toml"), "protected\n").expect("protected file");
  std::fs::write(repository.path().join("victim.txt"), "present\n").expect("victim file");
  std::fs::write(repository.path().join("script.sh"), "#!/bin/sh\n").expect("script file");
  let mut permissions = std::fs::metadata(repository.path().join("script.sh"))
    .expect("script metadata")
    .permissions();
  permissions.set_mode(0o755);
  std::fs::set_permissions(repository.path().join("script.sh"), permissions)
    .expect("script permissions");
  std::fs::write(repository.path().join("target.txt"), "target\n").expect("target file");
  symlink("target.txt", repository.path().join("link.txt")).expect("test symlink");
  run_git(repository.path(), &["add", "-A"]);
  run_git(
    repository.path(),
    &["commit", "-m", "verification fixtures"],
  );
  let base = git::head(repository.path()).await.expect("base revision");
  let manager = WorkspaceManager::new(repository.path().to_path_buf(), "immutable-check");
  let candidate = candidate(
    &repository,
    &manager,
    "check",
    &base,
    "check.txt",
    "candidate\n",
    vec![command.into()],
  )
  .await;
  let candidate_revision = candidate.candidate_revision.clone();
  let mut integrator = Integrator::create(
    repository.path().to_path_buf(),
    &manager,
    base,
    passing_project_config(),
  )
  .await
  .expect("create integrator");

  let outcome = integrator
    .integrate(&candidate)
    .await
    .expect("integrate immutable candidate");
  let IntegrationOutcome::Accepted { revision, .. } = outcome else {
    panic!("mutation in disposable check must not reject the immutable candidate")
  };
  assert_eq!(
    git::head(repository.path()).await.expect("canonical head"),
    revision
  );
  integrator
    .cleanup(&manager)
    .await
    .expect("cleanup integration");
  (repository, revision, candidate_revision)
}

#[cfg(unix)]
#[tokio::test]
async fn suggested_check_cannot_modify_candidate_source() {
  let (repository, accepted_revision, candidate_revision) = repository_after_disposable_check(
    "printf hacked > check.txt && git add -A && git commit -m malicious-check",
  )
  .await;
  assert_eq!(
    std::fs::read_to_string(repository.path().join("check.txt")).expect("candidate source"),
    "candidate\n"
  );
  assert_eq!(
    git::head(repository.path()).await.expect("canonical head"),
    accepted_revision
  );
  assert_eq!(
    run_git(
      repository.path(),
      &["show", &format!("{candidate_revision}:check.txt")]
    ),
    "candidate"
  );
}

#[cfg(unix)]
#[tokio::test]
async fn suggested_check_cannot_modify_protected_file() {
  let (repository, _, _) = repository_after_disposable_check("printf hacked > tenet.toml").await;
  assert_eq!(
    std::fs::read_to_string(repository.path().join("tenet.toml")).expect("protected file"),
    "protected\n"
  );
}

#[cfg(unix)]
#[tokio::test]
async fn suggested_check_cannot_delete_tracked_file() {
  let (repository, _, _) = repository_after_disposable_check("rm victim.txt").await;
  assert!(repository.path().join("victim.txt").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn suggested_check_cannot_change_executable_mode() {
  use std::os::unix::fs::PermissionsExt;

  let (repository, _, _) = repository_after_disposable_check("chmod -x script.sh").await;
  let mode = std::fs::metadata(repository.path().join("script.sh"))
    .expect("script metadata")
    .permissions()
    .mode();

  assert_ne!(mode & 0o111, 0);
}

#[cfg(unix)]
#[tokio::test]
async fn suggested_check_cannot_change_symlink_target() {
  let (repository, _, _) = repository_after_disposable_check("ln -sf victim.txt link.txt").await;
  assert_eq!(
    std::fs::read_link(repository.path().join("link.txt")).expect("symlink target"),
    PathBuf::from("target.txt")
  );
}

#[tokio::test]
async fn cancellation_before_integration_leaves_canonical_head_unchanged() {
  let repository = TempRepo::new();
  let base = git::head(repository.path()).await.expect("base revision");
  let manager = WorkspaceManager::new(repository.path().to_path_buf(), "cancel-before");
  let candidate = candidate(
    &repository,
    &manager,
    "cancel",
    &base,
    "cancel.txt",
    "candidate",
    Vec::new(),
  )
  .await;
  let cancel = CancellationToken::new();
  cancel.cancel();
  let mut integrator = Integrator::create_with_cancel(
    repository.path().to_path_buf(),
    &manager,
    base.clone(),
    passing_project_config(),
    cancel,
  )
  .await
  .expect("create integrator");

  let error = integrator
    .integrate(&candidate)
    .await
    .expect_err("cancellation fences integration");

  assert!(error.to_string().contains("cancelled"));
  assert_eq!(
    git::head(repository.path()).await.expect("canonical head"),
    base
  );
  integrator
    .cleanup(&manager)
    .await
    .expect("cleanup integrator");
}

async fn assert_cancellation_during_integration_verification(global: bool) {
  let repository = TempRepo::new();
  let base = git::head(repository.path()).await.expect("base revision");
  let manager = WorkspaceManager::new(repository.path().to_path_buf(), "cancel-verification");
  let marker = repository
    .path()
    .parent()
    .expect("temporary parent")
    .join(format!("tenet-cancel-marker-{}", Uuid::new_v4()));
  let command = format!("touch '{}' && sleep 30", marker.display());
  let candidate = candidate(
    &repository,
    &manager,
    "cancel",
    &base,
    "cancel.txt",
    "candidate",
    (!global).then_some(command.clone()).into_iter().collect(),
  )
  .await;
  let mut config = Config::default();
  if global {
    config.verification.checks = vec![ProjectVerificationCheck {
      name: "cancellable".into(),
      command: vec!["sh".into(), "-c".into(), command],
      working_directory: ".".into(),
      environment: Default::default(),
      timeout_secs: None,
    }];
  }
  let cancel = CancellationToken::new();
  let trigger = cancel.clone();
  let marker_for_trigger = marker.clone();
  let cancellation = tokio::spawn(async move {
    while !marker_for_trigger.exists() {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
    trigger.cancel();
  });
  let mut integrator = Integrator::create_with_cancel(
    repository.path().to_path_buf(),
    &manager,
    base.clone(),
    config,
    cancel,
  )
  .await
  .expect("create integrator");

  let error = integrator
    .integrate(&candidate)
    .await
    .expect_err("verification cancellation propagates");
  cancellation.await.expect("cancellation trigger");

  assert!(error.to_string().contains("cancelled"));
  assert_eq!(
    git::head(repository.path()).await.expect("canonical head"),
    base
  );
  integrator
    .cleanup(&manager)
    .await
    .expect("cleanup integrator");
  let _ = std::fs::remove_file(marker);
}

#[tokio::test]
async fn cancellation_during_suggested_check_leaves_canonical_head_unchanged() {
  assert_cancellation_during_integration_verification(false).await;
}

#[tokio::test]
async fn cancellation_during_global_verification_leaves_canonical_head_unchanged() {
  assert_cancellation_during_integration_verification(true).await;
}

async fn write_recovery_transaction(
  repository: &TempRepo,
  old_head: &str,
  new_head: &str,
  phase: IntegrationPhase,
) -> IntegrationTransaction {
  let mut report = passing_project_verification();
  report.revision = new_head.into();
  let catalog = RequirementCatalog {
    spec_hash: "recovery-spec".into(),
    requirements: vec![requirement()],
    acceptance_criteria: vec![criterion()],
    verification_obligations: vec![obligation()],
    coverage: CatalogCoverage::derive("diamond", &[requirement()]),
  };
  store::write_catalog(repository.path(), &catalog)
    .await
    .expect("persist recovery catalog");
  let mut state = State::fresh();
  state.run_id = Some("recovery-run".into());
  state.status = tenet_domain::model::RunStatus::Running;
  state.phase = tenet_domain::model::Phase::Integrating;
  state.cycle = 1;
  store::write_state(repository.path(), &state)
    .await
    .expect("persist recovery run");
  store::write_roadmap(
    repository.path(),
    "recovery-run",
    1,
    old_head,
    "recovery-spec",
    &ReconcileResult {
      summary: "recovery".into(),
      requirements: Vec::new(),
      work_units: vec![unit("A", &[])],
    },
  )
  .await
  .expect("persist recovery roadmap");
  store::record_manual_verification(repository.path(), "recovery-run", &report)
    .await
    .expect("persist recovery verification");
  let mut transaction = IntegrationTransaction {
    version: IntegrationTransaction::VERSION,
    id: Uuid::new_v4().to_string(),
    run_id: "recovery-run".into(),
    work_unit: unit("A", &[]),
    candidate_revision: new_head.into(),
    old_head: old_head.into(),
    new_head: new_head.into(),
    phase: IntegrationPhase::Prepared,
    verification_run_id: report.run_id,
    verification_hash: store::project_verification_hash(&report).expect("hash evidence"),
    created_at: "created".into(),
    updated_at: "updated".into(),
  };
  store::write_integration_journal(repository.path(), &transaction)
    .await
    .expect("prepare recovery transaction");
  if matches!(
    phase,
    IntegrationPhase::GitCommitted | IntegrationPhase::StateCommitted
  ) {
    transaction.phase = IntegrationPhase::GitCommitted;
    store::write_integration_journal(repository.path(), &transaction)
      .await
      .expect("mark recovery Git committed");
  }
  if phase == IntegrationPhase::StateCommitted {
    transaction.phase = IntegrationPhase::StateCommitted;
    store::write_integration_journal(repository.path(), &transaction)
      .await
      .expect("mark recovery state committed");
  }
  transaction
}

#[tokio::test]
async fn preflight_failure_does_not_orphan_unfinished_integration() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend, 1).await;
  let old_head = git::head(repository.path()).await.expect("old head");
  fs::write(repository.path().join("integrated.txt"), "integrated")
    .await
    .expect("write integrated state");
  let new_head = git::commit_all(repository.path(), "integrated candidate")
    .await
    .expect("new head");
  let transaction = write_recovery_transaction(
    &repository,
    &old_head,
    &new_head,
    IntegrationPhase::GitCommitted,
  )
  .await;
  std::fs::write(repository.path().join("tenet.toml"), "not valid toml =")
    .expect("invalidate config");

  controller
    .run(CancellationToken::new())
    .await
    .expect_err("invalid preflight remains fail closed");

  let state = store::read_state(repository.path())
    .await
    .expect("preserved state");
  assert_eq!(state.run_id.as_deref(), Some("recovery-run"));
  assert_eq!(
    store::read_integration_journal(repository.path())
      .await
      .expect("journal"),
    Some(transaction)
  );
}

#[tokio::test]
async fn prepared_journal_with_old_head_is_abandoned_without_advancement() {
  let repository = TempRepo::new();
  store::ensure_layout(repository.path())
    .await
    .expect("ensure layout");
  let old_head = git::head(repository.path()).await.expect("old head");
  write_recovery_transaction(
    &repository,
    &old_head,
    "1111111111111111111111111111111111111111",
    IntegrationPhase::Prepared,
  )
  .await;
  let mut state = State::fresh();

  store::recover_integration(repository.path(), &mut state, &passing_project_config())
    .await
    .expect("abandon uncommitted transaction");

  assert!(state.completed_work_units.is_empty());
  assert!(store::read_integration_journal(repository.path())
    .await
    .expect("read journal")
    .is_none());
}

async fn assert_new_head_transaction_recovers(phase: IntegrationPhase) {
  let repository = TempRepo::new();
  store::ensure_layout(repository.path())
    .await
    .expect("ensure layout");
  let old_head = git::head(repository.path()).await.expect("old head");
  std::fs::write(repository.path().join("recovered.txt"), "committed").expect("recovery file");
  run_git(repository.path(), &["add", "recovered.txt"]);
  run_git(repository.path(), &["commit", "-m", "already advanced"]);
  let new_head = git::head(repository.path()).await.expect("new head");
  write_recovery_transaction(&repository, &old_head, &new_head, phase).await;
  let mut state = State::fresh();

  store::recover_integration(repository.path(), &mut state, &passing_project_config())
    .await
    .expect("recover committed transaction");

  assert_eq!(state.completed_work_units.len(), 1);
  assert!(store::read_integration_journal(repository.path())
    .await
    .expect("read journal")
    .is_none());
}

#[tokio::test]
async fn changed_project_suite_invalidates_recovery_evidence() {
  let repository = TempRepo::new();
  store::ensure_layout(repository.path())
    .await
    .expect("ensure layout");
  let old_head = git::head(repository.path()).await.expect("old head");
  std::fs::write(repository.path().join("recovered.txt"), "committed").expect("recovery file");
  run_git(repository.path(), &["add", "recovered.txt"]);
  run_git(repository.path(), &["commit", "-m", "already advanced"]);
  let new_head = git::head(repository.path()).await.expect("new head");
  write_recovery_transaction(
    &repository,
    &old_head,
    &new_head,
    IntegrationPhase::GitCommitted,
  )
  .await;
  let mut changed = passing_project_config();
  changed.verification.checks[0].command = vec!["false".into()];
  let mut state = State::fresh();

  let error = store::recover_integration(repository.path(), &mut state, &changed)
    .await
    .expect_err("changed suite must stale prior evidence");

  assert!(error.to_string().contains("stale suite"));
  assert!(state.completed_work_units.is_empty());
}

#[tokio::test]
async fn prepared_journal_with_new_head_recovers_state() {
  assert_new_head_transaction_recovers(IntegrationPhase::Prepared).await;
}

#[tokio::test]
async fn git_committed_journal_with_new_head_recovers_state() {
  assert_new_head_transaction_recovers(IntegrationPhase::GitCommitted).await;
}

#[tokio::test]
async fn integration_recovery_rejects_unexpected_head() {
  let repository = TempRepo::new();
  store::ensure_layout(repository.path())
    .await
    .expect("ensure layout");
  write_recovery_transaction(
    &repository,
    "1111111111111111111111111111111111111111",
    "2222222222222222222222222222222222222222",
    IntegrationPhase::GitCommitted,
  )
  .await;
  let mut state = State::fresh();

  let error = store::recover_integration(repository.path(), &mut state, &passing_project_config())
    .await
    .expect_err("unexpected HEAD must fail closed");

  assert!(error.to_string().contains("matches neither expected old"));
}

#[tokio::test]
async fn prior_done_then_dirty_run_records_latest_failed_attempt() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend, 2).await;
  controller
    .run(CancellationToken::new())
    .await
    .expect("first run completes");
  std::fs::write(repository.path().join("dirty.txt"), "dirty").expect("dirty repository");

  controller
    .run(CancellationToken::new())
    .await
    .expect_err("dirty second run fails preflight");
  let state = store::read_state(repository.path())
    .await
    .expect("read latest attempt state");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Failed);
  assert_eq!(state.phase, tenet_domain::model::Phase::Initialized);
  assert!(state
    .last_error
    .as_deref()
    .is_some_and(|error| error.contains("clean canonical")));
}

#[tokio::test]
async fn prior_done_then_invalid_config_records_latest_failed_attempt() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend, 2).await;
  controller
    .run(CancellationToken::new())
    .await
    .expect("first run completes");
  std::fs::write(repository.path().join("tenet.toml"), "not valid toml =").expect("invalid config");

  controller
    .run(CancellationToken::new())
    .await
    .expect_err("invalid configuration fails preflight");
  let state = store::read_state(repository.path())
    .await
    .expect("read latest attempt state");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Failed);
  assert!(state
    .run_id
    .as_deref()
    .is_some_and(|id| id.starts_with("preflight-")));
}

async fn rewrite_config(repository: &TempRepo, configure: impl FnOnce(&mut Config)) {
  let mut config = tenet_domain::config::read_config(repository.path())
    .await
    .expect("read test config");
  configure(&mut config);
  fs::write(
    repository.path().join("tenet.toml"),
    toml::to_string_pretty(&config).expect("serialize test config"),
  )
  .await
  .expect("write test config");
  run_git(repository.path(), &["add", "tenet.toml"]);
  run_git(repository.path(), &["commit", "-m", "adjust test bounds"]);
}

#[tokio::test]
async fn repair_attempt_count_matches_configured_bound_exactly() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::NeverPassingVerification));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  rewrite_config(&repository, |config| config.max_repair_attempts = 2).await;

  controller
    .run(CancellationToken::new())
    .await
    .expect_err("permanently failing verification exhausts repairs");

  assert_eq!(backend.repair_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn repair_budget_is_shared_across_empty_and_verification_recovery() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(
    BackendMode::EmptyThenNeverPassingVerification,
  ));
  let (controller, _) = configured_controller(&repository, backend.clone(), 1).await;
  rewrite_config(&repository, |config| config.max_repair_attempts = 2).await;

  controller
    .run(CancellationToken::new())
    .await
    .expect_err("combined recovery exhausts one shared repair budget");

  assert_eq!(backend.repair_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn mechanical_proof_does_not_route_model_suspicion_to_repair() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::SemanticGapThenRepair));
  let (controller, _) = configured_controller(&repository, backend.clone(), 2).await;

  let state = controller.run(CancellationToken::new()).await.expect("run");
  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert!(!state
    .completed_work_units
    .iter()
    .any(|completed| completed.work_unit.id == "semantic-fix"));
  assert_eq!(backend.assessment_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn incomplete_model_assessment_cannot_block_mechanical_proof_at_max_cycles() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::IncompleteAssessment));
  let (controller, _) = configured_controller(&repository, backend, 2).await;
  rewrite_config(&repository, |config| {
    config.max_cycles = 5;
    config.stagnation_limit = 10;
  })
  .await;

  let state = controller.run(CancellationToken::new()).await.expect("run");
  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
}

#[tokio::test]
async fn incomplete_model_assessment_cannot_create_stagnation() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::IncompleteAssessment));
  let (controller, _) = configured_controller(&repository, backend, 2).await;
  rewrite_config(&repository, |config| {
    config.max_cycles = 10;
    config.stagnation_limit = 1;
  })
  .await;

  let state = controller.run(CancellationToken::new()).await.expect("run");
  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
}

#[tokio::test]
async fn equivalent_work_across_revisions_blocks_before_max_cycles() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::SemanticallyRepeatedWork));
  let (controller, _) = configured_controller(&repository, backend.clone(), 2).await;
  rewrite_config(&repository, |config| {
    config.max_cycles = 10;
    config.stagnation_limit = 1;
  })
  .await;
  let initial_revision = git::head(repository.path())
    .await
    .expect("initial revision");

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("semantic stagnation produces blocked state");

  assert_eq!(state.status, RunStatus::Blocked);
  assert_eq!(state.cycle, 2);
  assert_ne!(
    git::head(repository.path())
      .await
      .expect("advanced revision"),
    initial_revision
  );
  assert!(state
    .blocked_reason
    .as_deref()
    .is_some_and(|reason| reason.contains("Stagnation limit (1)")));
  assert_eq!(
    backend.workspaces.lock().expect("workspace lock").len(),
    1,
    "a new revision and generated work ID must not authorize repeated execution"
  );
}

#[tokio::test]
async fn cancellation_after_prepared_journal_prevents_fast_forward() {
  let repository = TempRepo::new();
  store::ensure_layout(repository.path())
    .await
    .expect("ensure layout");
  let base = git::head(repository.path()).await.expect("base revision");
  let manager = WorkspaceManager::new(repository.path().to_path_buf(), "cancel-prepared");
  let candidate = candidate(
    &repository,
    &manager,
    "cancel",
    &base,
    "cancel.txt",
    "candidate",
    Vec::new(),
  )
  .await;
  let cancel = CancellationToken::new();
  let trigger = cancel.clone();
  let repository_path = repository.path().to_path_buf();
  let monitor = tokio::spawn(async move {
    while store::read_integration_journal(&repository_path)
      .await
      .expect("read prepared integration")
      .is_none()
    {
      tokio::task::yield_now().await;
    }
    trigger.cancel();
  });
  let mut integrator = Integrator::create_with_cancel(
    repository.path().to_path_buf(),
    &manager,
    base.clone(),
    passing_project_config(),
    cancel,
  )
  .await
  .expect("create integrator");

  let error = integrator
    .integrate(&candidate)
    .await
    .expect_err("prepared cancellation must fence fast-forward");
  monitor.await.expect("journal monitor");

  assert!(error.to_string().contains("cancelled"));
  assert_eq!(
    git::head(repository.path()).await.expect("canonical head"),
    base
  );
  integrator
    .cleanup(&manager)
    .await
    .expect("cleanup integrator");
}

#[tokio::test]
async fn state_committed_journal_with_new_head_recovers_idempotently() {
  assert_new_head_transaction_recovers(IntegrationPhase::StateCommitted).await;
}

#[tokio::test]
async fn orphan_evidence_before_journal_does_not_claim_completion() {
  let repository = TempRepo::new();
  store::ensure_layout(repository.path())
    .await
    .expect("ensure layout");
  let orphan = passing_project_verification();
  store::record_manual_verification(repository.path(), "orphan-run", &orphan)
    .await
    .expect("save orphan evidence");
  let mut state = State::fresh();

  store::recover_integration(repository.path(), &mut state, &passing_project_config())
    .await
    .expect("no journal means no recovery action");

  assert!(state.completed_work_units.is_empty());
}

#[tokio::test]
async fn mechanical_proof_skips_unneeded_assessor_workspace_cleanup() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::CleanupFailure));
  let (controller, _) = configured_controller(&repository, backend.clone(), 2).await;

  let state = controller.run(CancellationToken::new()).await.expect("run");
  let catalog = store::read_catalog(repository.path())
    .await
    .expect("read catalog")
    .expect("catalog");
  let graph = controller_evidence::load(repository.path(), &catalog)
    .await
    .expect("load evidence");
  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert_eq!(backend.assessment_calls.load(Ordering::SeqCst), 0);
  assert_eq!(
    graph.proof_derivations[&ObligationId::from("REQ-001/AC-01/VO-01")].state,
    ProofState::Proven
  );
}
