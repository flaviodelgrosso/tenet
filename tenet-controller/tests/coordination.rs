use std::{
  collections::BTreeMap,
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
use tokio::{
  fs,
  sync::{mpsc, Notify},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use tenet_controller::{evidence as controller_evidence, AgentBackend, Controller};
use tenet_domain::{
  config::{read_config, Config, CustomAgentConfig, ProjectVerificationGate},
  events::{EventSink, RunEvent},
  evidence::{
    AcceptanceCriterion, EvidencePolicy, ImplementationState, VerificationKind,
    VerificationObligation, VerificationState,
  },
  ids::SpecFragmentId,
  ids::{CriterionId, ObligationId, RequirementId},
  model::{
    ArchitectOutput, CandidateCheck, CompletedWorkUnit, Discovery, DiscoveryRecord,
    DiscoveryStatus, IntegrationPhase, IntegrationTransaction, ReconcileResult, Requirement,
    RequirementAssessment, RequirementCatalog, State, VerificationReport, WorkExecution, WorkLease,
    WorkScope, WorkUnit, WorkerRole, WorkerSummary,
  },
  verification::{DependencyScopeAuthority, VerificationAuthority, VerificationSpec},
  worker::{derive_normative_fragments, CatalogCoverage, SpecReference},
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

fn annotated_reference(spec: &str) -> SpecReference {
  let metadata = spec
    .lines()
    .find(|line| line.starts_with("[fragmentId="))
    .expect("annotated fragment metadata");
  let metadata = metadata.trim_matches(['[', ']']);
  let fields: BTreeMap<_, _> = metadata
    .split_whitespace()
    .filter_map(|field| field.split_once('='))
    .collect();
  SpecReference {
    section: (fields["section"] != "<none>").then(|| fields["section"].to_owned()),
    fragment_id: SpecFragmentId::from(fields["fragmentId"]),
    text_hash: fields["textHash"].to_owned(),
  }
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
    kind: VerificationKind::Command,
    required: true,
    spec: VerificationSpec {
      program: "git".into(),
      args: vec!["diff".into(), "--check".into()],
      working_directory: ".".into(),
      environment: Default::default(),
    },
    authority: VerificationAuthority::ProjectConfigured,
    dependency_scope: vec!["*.txt".into()],
    dependency_scope_authority: DependencyScopeAuthority::ProjectConfigured,
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

fn graph_units() -> Vec<WorkUnit> {
  vec![
    unit("A", &[]),
    unit("B", &["A"]),
    unit("C", &["A"]),
    unit("D", &["B", "C"]),
  ]
}

fn assessment(satisfied: bool) -> RequirementAssessment {
  RequirementAssessment {
    requirement_id: RequirementId::from("REQ-001"),
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
  FailB,
  WorkerTimeout,
  ProtectedMutation,
  WaitForCancellation,
  RepairDiscovery,
  InvalidVerificationCheck,
  NeverPassingVerification,
  IncompleteAssessment,
  CleanupFailure,
  InvalidReconcileThenCorrect,
  InvalidAssessmentThenCorrect,
  AlwaysInvalidReconcile,
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
      semantic_feedback: Mutex::new(Vec::new()),
      assessment_calls: AtomicUsize::new(0),
    }
  }

  fn record_active(&self) -> ActiveGuard<'_> {
    let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
    self.max_active.fetch_max(active, Ordering::SeqCst);
    ActiveGuard(&self.active)
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
    self
      .mutate_read_only_workspace(ctx, tenet_domain::model::WorkerRole::Architect)
      .await?;
    let mut requirement = requirement();
    requirement.source_refs = vec![annotated_reference(spec)];
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
    _catalog: &RequirementCatalog,
    _recent: &[CompletedWorkUnit],
    discoveries: &[Discovery],
    _evidence: &[tenet_domain::evidence::EvidenceProjection],
    semantic_validation_feedback: Option<&str>,
  ) -> Result<ReconcileResult> {
    self
      .mutate_read_only_workspace(ctx, tenet_domain::model::WorkerRole::Reconcile)
      .await?;
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
    if matches!(self.mode, BackendMode::RepairDiscovery) {
      if let Some(unit) = work_units.iter_mut().find(|unit| unit.id == "A") {
        unit.suggested_checks = vec![CandidateCheck {
          obligation_id: ObligationId::from("REQ-001/AC-01/VO-01"),
          command: "grep -q repaired A.txt".into(),
        }];
      }
    }
    if matches!(self.mode, BackendMode::NeverPassingVerification) {
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
    if matches!(self.mode, BackendMode::AlwaysInvalidReconcile)
      || matches!(self.mode, BackendMode::InvalidReconcileThenCorrect) && reconcile_call == 0
    {
      work_units[0].verification_obligation_ids = vec![ObligationId::from("REQ-999/AC-01/VO-01")];
    }
    Ok(ReconcileResult {
      summary: if satisfied {
        "implementation present; evidence pending".into()
      } else {
        "work remains".into()
      },
      requirements: vec![assessment(satisfied)],
      work_units,
    })
  }

  async fn implement(
    &self,
    ctx: &BackendContext,
    _catalog: &RequirementCatalog,
    work_unit: &WorkUnit,
  ) -> Result<WorkerSummary> {
    let _active = self.record_active();
    self
      .workspaces
      .lock()
      .expect("workspace lock")
      .push((work_unit.id.clone(), ctx.cwd.clone()));
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
    _report: &VerificationReport,
  ) -> Result<WorkerSummary> {
    let repair_number = self.repair_calls.fetch_add(1, Ordering::SeqCst) + 1;
    if matches!(self.mode, BackendMode::NeverPassingVerification) && work_unit.id == "A" {
      fs::write(ctx.cwd.join("A.txt"), format!("repair-{repair_number}")).await?;
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
    _catalog: &RequirementCatalog,
    _evidence: &[tenet_domain::evidence::EvidenceProjection],
    semantic_validation_feedback: Option<&str>,
  ) -> Result<ReconcileResult> {
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
    if matches!(self.mode, BackendMode::IncompleteAssessment) && satisfied {
      return Ok(ReconcileResult {
        summary: "assessment still finds a gap".into(),
        requirements: vec![assessment(false)],
        work_units: vec![unit("assessment-gap", &[])],
      });
    }
    if matches!(self.mode, BackendMode::InvalidAssessmentThenCorrect) && assessment_call == 0 {
      let mut invalid = unit("assessment-invalid", &[]);
      invalid.verification_obligation_ids = vec![ObligationId::from("REQ-999/AC-01/VO-01")];
      return Ok(ReconcileResult {
        summary: "malformed assessment".into(),
        requirements: vec![assessment(satisfied)],
        work_units: vec![invalid],
      });
    }
    Ok(ReconcileResult {
      summary: "skeptical assessment".into(),
      requirements: vec![assessment(satisfied)],
      work_units: Vec::new(),
    })
  }
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
  config.verification.gates = vec![ProjectVerificationGate {
    obligation_ids: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
    spec: obligation().spec,
    dependency_scope: vec!["*.txt".into()],
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

async fn assert_read_only_role_is_isolated(role: tenet_domain::model::WorkerRole, commit: bool) {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::ReadOnlyMutation {
    role,
    commit,
  }));
  let (controller, _) = configured_controller(&repository, backend, 2).await;
  if role == tenet_domain::model::WorkerRole::Architect {
    fs::remove_file(repository.path().join(".tenet/requirements.json"))
      .await
      .expect("remove cached catalog");
  }
  let head_before = git::head(repository.path()).await.expect("head before run");

  let state = controller
    .run(CancellationToken::new())
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
    .contains("Do not guess, repair, or normalize identifiers"));
}

#[tokio::test]
async fn malformed_assessment_is_retried_with_feedback_and_corrected() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::InvalidAssessmentThenCorrect));
  let (controller, _) = configured_controller(&repository, backend.clone(), 2).await;

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("corrected assessment proceeds");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Done);
  assert_eq!(backend.assessment_calls.load(Ordering::SeqCst), 2);
  assert!(!repository
    .path()
    .join("semantic-assessment-attempt.txt")
    .exists());
  let feedback = backend
    .semantic_feedback
    .lock()
    .expect("semantic feedback lock");
  assert_eq!(feedback.len(), 1);
  assert_eq!(feedback[0].0, WorkerRole::Assess);
  assert!(feedback[0]
    .1
    .contains("assessment-invalid targets unknown verification obligation REQ-999/AC-01/VO-01"));
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
  let mut evidence_events = 0;
  let mut verified_transition = false;
  while let Ok(event) = events.try_recv() {
    match event {
      RunEvent::IntegrationAccepted { work_unit_id, .. } => integrations.push(work_unit_id),
      RunEvent::WorkerStarted { worker_id, .. } => {
        worker_ids.insert(worker_id);
      }
      RunEvent::CandidateProduced(candidate) => candidates.push(candidate),
      RunEvent::EvidenceEstablished(_) => evidence_events += 1,
      RunEvent::RequirementVerificationChanged {
        current: VerificationState::Verified,
        ..
      } => verified_transition = true,
      _ => {}
    }
  }
  assert_eq!(integrations, ["A", "B", "C", "D"]);
  assert_eq!(worker_ids.len(), 4);
  assert_eq!(candidates.len(), 4);
  assert!(evidence_events > 0);
  assert!(verified_transition);
  let catalog = store::read_catalog(repository.path())
    .await
    .expect("read catalog")
    .expect("catalog");
  let graph = controller_evidence::load(repository.path(), &catalog)
    .await
    .expect("read evidence graph");
  assert!(graph.all_required_verified(&EvidencePolicy));
  let explanation = graph
    .projection(&RequirementId::from("REQ-001"), &EvidencePolicy)
    .expect("requirement explanation");
  assert_eq!(explanation.verification_state, VerificationState::Verified);
  let head = git::head(repository.path()).await.expect("head");
  assert!(explanation.criteria[0].obligations[0]
    .evidence
    .iter()
    .any(|evidence| evidence.revision == head));

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
async fn assert_out_of_scope_change_is_rejected(mutation: ScopeMutation) {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::ScopeMutation(mutation)));
  let (controller, _) = configured_controller(&repository, backend, 1).await;
  let head_before = git::head(repository.path()).await.expect("head before run");

  let error = controller
    .run(CancellationToken::new())
    .await
    .expect_err("out-of-scope candidate must fail closed");

  assert!(error.to_string().contains("outside its declared scope"));
  assert_eq!(
    git::head(repository.path()).await.expect("canonical head"),
    head_before
  );
}

#[tokio::test]
async fn candidate_modification_outside_scope_is_rejected() {
  assert_out_of_scope_change_is_rejected(ScopeMutation::Modification).await;
}

#[tokio::test]
async fn candidate_addition_outside_scope_is_rejected() {
  assert_out_of_scope_change_is_rejected(ScopeMutation::Addition).await;
}

#[tokio::test]
async fn candidate_deletion_outside_scope_is_rejected() {
  assert_out_of_scope_change_is_rejected(ScopeMutation::Deletion).await;
}

#[tokio::test]
async fn candidate_rename_outside_scope_is_rejected() {
  assert_out_of_scope_change_is_rejected(ScopeMutation::Rename).await;
}

async fn run_with_historical_a(mut historical: WorkUnit) {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::Normal));
  let (controller, _) = configured_controller(&repository, backend, 2).await;
  historical.id = "A".into();
  let mut state = State::fresh();
  state.completed_work_units.push(CompletedWorkUnit {
    work_unit: historical,
    completed_at: "historical".into(),
    verification_evidence: ".tenet/evidence/historical.json".into(),
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
async fn reused_work_id_with_changed_requirement_set_is_not_suppressed() {
  let mut historical = unit("A", &[]);
  historical.requirement_ids = vec!["REQ-OLD".into()];
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

  let second = controller
    .run(CancellationToken::new())
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
  let list = run_git(repository.path(), &["worktree", "list", "--porcelain"]);
  assert_eq!(list.matches("worktree ").count(), 1);
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

async fn candidate(
  _repository: &TempRepo,
  manager: &WorkspaceManager,
  id: &str,
  base: &str,
  path: &str,
  content: &str,
  suggested_checks: Vec<String>,
) -> WorkExecution {
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
    Config::default(),
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
    Config::default(),
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
async fn integration_rejects_candidate_check_and_global_regression() {
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
    Config::default(),
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

  let regression_manager = WorkspaceManager::new(repository.path().to_path_buf(), "regression");
  let regression = candidate(
    &repository,
    &regression_manager,
    "regression",
    &base,
    "regression",
    "present",
    Vec::new(),
  )
  .await;
  let mut config = Config::default();
  config.verification.gates = vec![ProjectVerificationGate {
    obligation_ids: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
    spec: VerificationSpec {
      program: "sh".into(),
      args: vec!["-lc".into(), "test ! -f regression".into()],
      working_directory: ".".into(),
      environment: Default::default(),
    },
    dependency_scope: vec!["**".into()],
  }];
  let mut regression_integrator = Integrator::create(
    repository.path().to_path_buf(),
    &regression_manager,
    base,
    config,
  )
  .await
  .expect("create regression integrator");

  let outcome = regression_integrator
    .integrate(&regression)
    .await
    .expect("run regression gate");

  assert!(matches!(
    outcome,
    IntegrationOutcome::RegressionDetected { .. }
  ));
  regression_integrator
    .cleanup(&regression_manager)
    .await
    .expect("cleanup regression integration");
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
    Config::default(),
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
    Config::default(),
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
    config.verification.gates = vec![ProjectVerificationGate {
      obligation_ids: vec![ObligationId::from("REQ-001/AC-01/VO-01")],
      spec: VerificationSpec {
        program: "sh".into(),
        args: vec!["-lc".into(), command],
        working_directory: ".".into(),
        environment: Default::default(),
      },
      dependency_scope: vec!["**".into()],
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
  let report = passing_report();
  let evidence_path = store::save_evidence(repository.path(), "recovery", &report)
    .await
    .expect("save recovery evidence");
  let evidence = evidence_path
    .strip_prefix(repository.path())
    .expect("relative evidence")
    .display()
    .to_string();
  let transaction = IntegrationTransaction {
    version: IntegrationTransaction::VERSION,
    id: Uuid::new_v4().to_string(),
    run_id: "recovery-run".into(),
    work_unit: unit("A", &[]),
    candidate_revision: new_head.into(),
    old_head: old_head.into(),
    new_head: new_head.into(),
    phase,
    verification_evidence: evidence,
    verification_hash: store::verification_hash(&report).expect("hash evidence"),
    created_at: "created".into(),
    updated_at: "updated".into(),
  };
  store::write_integration_journal(repository.path(), &transaction)
    .await
    .expect("write recovery journal");
  transaction
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

  store::recover_integration(repository.path(), &mut state)
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

  store::recover_integration(repository.path(), &mut state)
    .await
    .expect("recover committed transaction");

  assert_eq!(state.completed_work_units.len(), 1);
  assert!(store::read_integration_journal(repository.path())
    .await
    .expect("read journal")
    .is_none());
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

  let error = store::recover_integration(repository.path(), &mut state)
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
async fn maximum_cycle_count_blocks_on_exact_configured_cycle() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::IncompleteAssessment));
  let (controller, _) = configured_controller(&repository, backend, 2).await;
  rewrite_config(&repository, |config| {
    config.max_cycles = 5;
    config.stagnation_limit = 10;
  })
  .await;

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("max cycles produces blocked state");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Blocked);
  assert_eq!(state.cycle, 5);
  assert_eq!(
    state.blocked_reason.as_deref(),
    Some("Maximum cycle count (5) reached")
  );
}

#[tokio::test]
async fn stagnation_limit_blocks_on_exact_unchanged_transition() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::IncompleteAssessment));
  let (controller, _) = configured_controller(&repository, backend, 2).await;
  rewrite_config(&repository, |config| {
    config.max_cycles = 10;
    config.stagnation_limit = 1;
  })
  .await;

  let state = controller
    .run(CancellationToken::new())
    .await
    .expect("stagnation produces blocked state");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Blocked);
  assert_eq!(state.cycle, 5);
  assert!(state
    .blocked_reason
    .as_deref()
    .is_some_and(|reason| reason.contains("Stagnation limit (1)")));
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
  let journal = repository
    .path()
    .join(".tenet")
    .join(store::INTEGRATION_JOURNAL_FILE);
  let monitor = tokio::spawn(async move {
    while !journal.exists() {
      tokio::task::yield_now().await;
    }
    trigger.cancel();
  });
  let mut integrator = Integrator::create_with_cancel(
    repository.path().to_path_buf(),
    &manager,
    base.clone(),
    Config::default(),
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
  store::save_evidence(repository.path(), "orphan", &passing_report())
    .await
    .expect("save orphan evidence");
  let mut state = State::fresh();

  store::recover_integration(repository.path(), &mut state)
    .await
    .expect("no journal means no recovery action");

  assert!(state.completed_work_units.is_empty());
}

#[tokio::test]
async fn cleanup_failure_prevents_persisted_done_state() {
  let repository = TempRepo::new();
  let backend = Arc::new(FakeBackend::new(BackendMode::CleanupFailure));
  let (controller, _) = configured_controller(&repository, backend, 2).await;

  controller
    .run(CancellationToken::new())
    .await
    .expect_err("required cleanup failure must fail the run");
  let state = store::read_state(repository.path())
    .await
    .expect("read cleanup failure state");

  assert_eq!(state.status, tenet_domain::model::RunStatus::Failed);
  assert_ne!(state.status, tenet_domain::model::RunStatus::Done);
  assert!(state
    .last_error
    .as_deref()
    .is_some_and(|error| error.contains("workspace cleanup failed")));
}
