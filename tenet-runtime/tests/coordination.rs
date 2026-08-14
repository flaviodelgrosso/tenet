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

use tenet_domain::{
  config::{Config, CustomAgentConfig},
  events::{EventSink, RunEvent},
  model::{
    ArchitectOutput, CompletedWorkUnit, Discovery, ReconcileResult, Requirement,
    RequirementAssessment, RequirementCatalog, RequirementStatus, VerificationReport,
    WorkExecution, WorkLease, WorkScope, WorkUnit, WorkerSummary,
  },
};
use tenet_runtime::{
  backend::{AgentBackend, BackendContext, LaunchMetadata},
  controller::Controller,
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

fn requirement() -> Requirement {
  Requirement {
    id: "REQ-001".into(),
    title: "Diamond".into(),
    description: "Complete the diamond graph".into(),
    acceptance_criteria: vec!["A, B, C, and D exist".into()],
  }
}

fn unit(id: &str, dependencies: &[&str]) -> WorkUnit {
  WorkUnit {
    id: id.into(),
    title: format!("Implement {id}"),
    objective: format!("Create {id}"),
    requirement_ids: vec!["REQ-001".into()],
    acceptance_criteria: vec![format!("{id}.txt exists")],
    suggested_checks: vec![format!("test -f {id}.txt")],
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
    id: "REQ-001".into(),
    status: if satisfied {
      RequirementStatus::Satisfied
    } else {
      RequirementStatus::Missing
    },
    evidence: satisfied
      .then(|| "all files exist".into())
      .into_iter()
      .collect(),
    gaps: (!satisfied)
      .then(|| "diamond incomplete".into())
      .into_iter()
      .collect(),
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
enum BackendMode {
  Normal,
  FailB,
  WorkerTimeout,
  ProtectedMutation,
  WaitForCancellation,
}

struct FakeBackend {
  mode: BackendMode,
  workspaces: Mutex<Vec<(String, PathBuf)>>,
  waiting: Notify,
  active: AtomicUsize,
  max_active: AtomicUsize,
  discoveries_seen: AtomicUsize,
}

impl FakeBackend {
  fn new(mode: BackendMode) -> Self {
    Self {
      mode,
      workspaces: Mutex::new(Vec::new()),
      active: AtomicUsize::new(0),
      max_active: AtomicUsize::new(0),
      waiting: Notify::new(),
      discoveries_seen: AtomicUsize::new(0),
    }
  }

  fn record_active(&self) -> ActiveGuard<'_> {
    let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
    self.max_active.fetch_max(active, Ordering::SeqCst);
    ActiveGuard(&self.active)
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
  async fn architect(&self, _ctx: &BackendContext, _spec: &str) -> Result<ArchitectOutput> {
    Ok(ArchitectOutput {
      requirements: vec![requirement()],
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
  ) -> Result<ReconcileResult> {
    self
      .discoveries_seen
      .fetch_add(discoveries.len(), Ordering::SeqCst);
    let satisfied = ["A", "B", "C", "D"]
      .iter()
      .all(|id| ctx.cwd.join(format!("{id}.txt")).exists());
    Ok(ReconcileResult {
      complete: satisfied,
      summary: if satisfied {
        "complete".into()
      } else {
        "work remains".into()
      },
      requirements: vec![assessment(satisfied)],
      work_units: if satisfied { Vec::new() } else { graph_units() },
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
    _ctx: &BackendContext,
    _catalog: &RequirementCatalog,
    _work_unit: &WorkUnit,
    _report: &VerificationReport,
  ) -> Result<WorkerSummary> {
    bail!("repair not expected")
  }

  async fn assess(
    &self,
    ctx: &BackendContext,
    _catalog: &RequirementCatalog,
  ) -> Result<ReconcileResult> {
    let satisfied = ["A", "B", "C", "D"]
      .iter()
      .all(|id| ctx.cwd.join(format!("{id}.txt")).exists());
    Ok(ReconcileResult {
      complete: satisfied,
      summary: "assessment".into(),
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
  let mut config = Config::default();
  config.agent.custom = Some(CustomAgentConfig {
    command: "unused".into(),
    args: Vec::new(),
    env: BTreeMap::new(),
  });
  config.execution.max_parallel_workers = max_parallel_workers;
  config.verification.commands = vec!["true".into()];
  fs::write(
    repository.path().join("tenet.toml"),
    toml::to_string_pretty(&config).expect("serialize config"),
  )
  .await
  .expect("write config");
  fs::create_dir_all(repository.path().join(".tenet"))
    .await
    .expect("create tenet dir");
  fs::write(repository.path().join(".tenet/spec.md"), "diamond")
    .await
    .expect("write spec");
  run_git(repository.path(), &["add", "tenet.toml"]);
  run_git(repository.path(), &["commit", "-m", "configure"]);
  let (_, spec_hash) = store::spec_text_and_hash(repository.path(), &config)
    .await
    .expect("hash spec");
  store::write_catalog(
    repository.path(),
    &RequirementCatalog {
      spec_hash,
      requirements: vec![requirement()],
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
  assert!(backend.max_active.load(Ordering::SeqCst) >= 2);
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
  drop(workspaces);
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
    started_at: "start".into(),
    finished_at: "finish".into(),
    commands: Vec::new(),
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
        suggested_checks,
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
  config.verification.commands = vec!["test ! -f regression".into()];
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
