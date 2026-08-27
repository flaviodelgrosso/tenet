#[cfg(unix)]
use std::os::fd::{FromRawFd, RawFd};
use std::{
  io::{IsTerminal, Read},
  path::PathBuf,
  process::ExitCode,
  sync::Arc,
};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::Parser;
use tenet_acp::acp::AcpRuntime;
use tenet_controller::{catalog, controller::manual_verify, evidence, AgentBackend};
use tenet_domain::{
  config::read_config,
  evidence::EvidencePolicy,
  human_attestation::{HumanAttestationBinding, HumanAttestationRecord},
  ids::ObligationId,
  model::{CatalogApproval, RunStatus},
  proof::{statement_hash, EvidenceContract},
};
use tenet_runtime::{
  authority::{AuthorityBootstrap, AuthorityInitialization},
  store,
};
use tenet_storage::{DatabaseHealth, Storage};

use crate::{
  agents,
  cli::{Cli, Command, DbCommand, DumpCommand, EvidenceCommand, RequirementsCommand},
  run::{self, RunOptions},
};

pub(crate) struct App {
  cwd: PathBuf,
  backend: Arc<dyn AgentBackend>,
  command: Option<Command>,
  authority: AuthorityBootstrap,
}

impl App {
  pub(crate) fn new() -> Result<Self> {
    let cli = Cli::parse();
    let cur_dir = std::env::current_dir().context("current directory")?;

    let authority = AuthorityBootstrap::from_environment()?;
    Ok(Self {
      cwd: cli.cwd.unwrap_or(cur_dir),
      backend: Arc::new(AcpRuntime),
      command: Some(cli.command),
      authority,
    })
  }

  pub(crate) async fn run(mut self) -> Result<ExitCode> {
    let command = self
      .command
      .take()
      .expect("Clap requires an explicit subcommand");
    if command.requires_authority() {
      self.authority.install(&self.cwd)?;
    }
    let exit_code = match command {
      Command::Init => {
        self.initialize().await?;
        ExitCode::SUCCESS
      }
      Command::Run { quiet, verbose } | Command::Resume { quiet, verbose } => {
        let state = run::run(self.cwd, self.backend, RunOptions { quiet, verbose }).await?;
        if matches!(
          state.status,
          RunStatus::Blocked | RunStatus::Failed | RunStatus::Stopped
        ) {
          ExitCode::from(2)
        } else {
          ExitCode::SUCCESS
        }
      }
      Command::Agent { command } => {
        if agents::handle(&self.cwd, command).await? {
          ExitCode::SUCCESS
        } else {
          ExitCode::from(2)
        }
      }
      Command::Status { json } => {
        self.print_status(json).await?;
        ExitCode::SUCCESS
      }
      Command::Verify { json } => self.verify(json).await?,
      Command::Db {
        command: DbCommand::Check,
      } => {
        self.check_database().await?;
        ExitCode::SUCCESS
      }
      Command::State {
        command: DumpCommand::Dump { json },
      } => {
        self.dump_state(json).await?;
        ExitCode::SUCCESS
      }
      Command::Requirements {
        command: RequirementsCommand::Dump { json },
      } => {
        self.dump_requirements(json).await?;
        ExitCode::SUCCESS
      }
      Command::Requirements {
        command: RequirementsCommand::Approve,
      } => {
        self.approve_requirements().await?;
        ExitCode::SUCCESS
      }
      Command::Evidence {
        command: EvidenceCommand::Dump { json, requirement },
      } => {
        self.dump_evidence(json, requirement.as_deref()).await?;
        ExitCode::SUCCESS
      }
      Command::Evidence {
        command:
          EvidenceCommand::Attest {
            obligation,
            statement,
            attestor,
            signing_key_fd,
          },
      } => {
        self
          .attest(&obligation, &statement, &attestor, signing_key_fd)
          .await?;
        ExitCode::SUCCESS
      }
      Command::Roadmap {
        command: DumpCommand::Dump { json },
      } => {
        self.dump_roadmap(json).await?;
        ExitCode::SUCCESS
      }
    };
    Ok(exit_code)
  }

  async fn initialize(&mut self) -> Result<()> {
    let authority = self.authority.initialize(&self.cwd)?;
    store::ensure_layout(&self.cwd).await?;
    store::ensure_gitignore(&self.cwd).await?;
    let config = tenet_domain::config::ensure_config(&self.cwd).await?;
    store::ensure_spec(&self.cwd, &config).await?;
    self.print_initialization(&authority);
    Ok(())
  }

  fn print_initialization(&self, authority: &AuthorityInitialization) {
    println!("Initialized tenet in {}", self.cwd.display());
    println!(
      "Controller authority: {} ({})",
      authority.authority_id,
      authority.provider.display_name()
    );
    if authority.created {
      println!(
        "The controller credential was provisioned securely and will be resolved automatically."
      );
    } else {
      println!("The existing controller credential is available and consistent.");
    }
    if std::io::stdout().is_terminal() {
      println!("Run `tenet agents` to browse ACP Registry agents, then `tenet agents select <id>`");
    } else {
      println!("Set exactly one of agent.id (Registry) or [agent.custom] in tenet.toml; no agent was selected automatically");
    }
  }

  async fn print_status(&self, json: bool) -> Result<()> {
    let state = Storage::open_existing(&self.cwd)
      .await?
      .load_current_state()
      .await?;
    if json {
      println!("{}", serde_json::to_string_pretty(&state)?);
      return Ok(());
    }
    println!("status: {:?}", state.status);
    println!("phase: {:?}", state.phase);
    println!("cycle: {}", state.cycle);
    println!(
      "requirements: {}/{} verified ({} stale, {} contradicted)",
      state.requirement_counts.verified,
      state.requirement_counts.total,
      state.requirement_counts.stale,
      state.requirement_counts.contradicted
    );
    println!(
      "project checks: {}/{} {}",
      state.verification_layers.project_checks_passed,
      state.verification_layers.project_checks_total,
      if state.verification_layers.project_passed {
        "PASS"
      } else {
        "NOT PASSING"
      }
    );
    println!(
      "semantic obligations: {}/{} SATISFIED ({} gaps, {} uncertain)",
      state.verification_layers.semantic_satisfied,
      state.verification_layers.semantic_obligations_total,
      state.verification_layers.semantic_gaps,
      state.verification_layers.semantic_uncertain
    );
    println!(
      "contradictions: {}",
      state.verification_layers.contradictions
    );
    println!(
      "completion: {}",
      if state.verification_layers.completion_eligible {
        "ELIGIBLE"
      } else {
        "BLOCKED"
      }
    );
    for lease in state.active_leases.values() {
      println!(
        "active work: {} · {} ({})",
        lease.work_unit.id, lease.work_unit.title, lease.worker_id
      );
    }
    println!("summary: {}", state.last_summary);
    if let Some(reason) = state.blocked_reason {
      println!("blocked: {reason}");
    }
    if let Some(error) = state.last_error {
      println!("error: {error}");
    }
    Ok(())
  }

  async fn check_database(&self) -> Result<()> {
    let storage = Storage::open_existing(&self.cwd).await?;
    let quick = storage.quick_check().await?;
    let foreign_keys = storage.foreign_key_check().await?;
    if quick != DatabaseHealth::Ok || foreign_keys != DatabaseHealth::Ok {
      anyhow::bail!("database check failed: quick={quick:?}, foreign_keys={foreign_keys:?}");
    }
    println!("database: {}", storage.path().display());
    println!("quick_check: ok");
    println!("foreign_key_check: ok");
    Ok(())
  }

  async fn dump_state(&self, _json: bool) -> Result<()> {
    let value = Storage::open_existing(&self.cwd)
      .await?
      .load_current_state()
      .await?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
  }

  async fn dump_requirements(&self, _json: bool) -> Result<()> {
    let value = Storage::open_existing(&self.cwd)
      .await?
      .load_active_catalog()
      .await?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
  }

  async fn approve_requirements(&self) -> Result<()> {
    let storage = Storage::open(&self.cwd).await?;
    let catalog = storage
      .load_active_catalog()
      .await?
      .context("no active requirement catalog to approve")?;
    catalog::validate(&catalog).context("active requirement catalog is structurally invalid")?;
    let catalog_hash = catalog.catalog_hash()?;
    let approval = CatalogApproval {
      spec_hash: catalog.spec_hash.clone(),
      catalog_hash: catalog_hash.clone(),
      approved_at: Utc::now(),
    };
    storage.persist_catalog_approval(&approval).await?;
    println!("Approved requirement catalog.");
    println!("specification: {}", approval.spec_hash);
    println!("catalog: {catalog_hash}");
    println!("requirements: {}", catalog.requirements.len());
    Ok(())
  }

  async fn dump_evidence(&self, _json: bool, requirement: Option<&str>) -> Result<()> {
    let storage = Storage::open_existing(&self.cwd).await?;
    let catalog = storage
      .load_active_catalog()
      .await?
      .context("no active requirement catalog")?;
    let config = read_config(&self.cwd).await?;
    let graph = storage
      .load_evidence_graph(
        &catalog,
        &config.verification.trusted_checks,
        &config.verification.falsifiers,
        &config.verification.human_attestors,
      )
      .await?;
    if let Some(requirement) = requirement {
      let revision = tenet_runtime::git::head(&self.cwd).await?;
      let projection = graph
        .projection(&requirement.into(), EvidencePolicy::new(&revision))
        .context("unknown requirement")?;
      println!("{}", serde_json::to_string_pretty(&projection)?);
    } else {
      println!("{}", serde_json::to_string_pretty(&graph)?);
    }
    Ok(())
  }

  async fn attest(
    &self,
    obligation_id: &str,
    expected_statement: &str,
    attestor_id: &str,
    signing_key_fd: i32,
  ) -> Result<()> {
    let storage = Storage::open_existing(&self.cwd).await?;
    let catalog = storage
      .load_active_catalog()
      .await?
      .context("no active requirement catalog")?;
    let config = read_config(&self.cwd).await?;
    let obligation_id = ObligationId::from(obligation_id);
    let obligation = catalog
      .verification_obligations
      .iter()
      .find(|item| item.id == obligation_id)
      .context("unknown verification obligation")?;
    let statement = human_statement(&obligation.evidence_contract, expected_statement)
      .context("obligation has no human-attestation contract with that exact statement")?;
    let expected_statement_hash = statement_hash(statement);
    let attestor = config
      .verification
      .human_attestors
      .iter()
      .find(|item| item.id == attestor_id)
      .context("unknown configured human attestor")?;
    let revision = tenet_runtime::git::head(&self.cwd).await?;
    let repository_blobs = tenet_runtime::git::repository_blob_hashes(&self.cwd, &revision).await?;
    let dependencies = attestor
      .dependencies
      .materialize(&repository_blobs)
      .context("materialize configured human-attestation dependencies")?;
    let catalog_hash = catalog.catalog_hash()?;
    let mut secret_key = read_signing_key(signing_key_fd)?;
    let signed = HumanAttestationRecord::sign(
      attestor,
      &secret_key,
      HumanAttestationBinding {
        statement_hash: expected_statement_hash,
        obligation_id,
        catalog_hash: catalog_hash.clone(),
        revision,
        issued_at: Utc::now(),
        dependencies,
      },
    );
    secret_key.fill(0);
    let record = signed.context("sign exact human attestation")?;
    let mut graph = storage
      .load_evidence_graph(
        &catalog,
        &config.verification.trusted_checks,
        &config.verification.falsifiers,
        &config.verification.human_attestors,
      )
      .await?;
    let artifact_id =
      evidence::record_human_attestation(&self.cwd, &mut graph, &record, attestor, &catalog_hash)
        .await?;
    println!("Authenticated human attestation recorded.");
    println!("attestor: {}", attestor.id);
    println!("obligation: {}", record.obligation_id);
    println!("statement: {statement}");
    println!("statement hash: {}", record.statement_hash);
    println!("revision: {}", record.revision);
    println!("artifact: {artifact_id}");
    Ok(())
  }

  async fn dump_roadmap(&self, _json: bool) -> Result<()> {
    let storage = Storage::open_existing(&self.cwd).await?;
    let state = storage.load_current_state().await?;
    let value = match state.run_id {
      Some(run_id) => storage.load_latest_reconcile_result(&run_id).await?,
      None => None,
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
  }

  async fn verify(&self, json: bool) -> Result<ExitCode> {
    let report = manual_verify(&self.cwd).await?;
    if json {
      println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
      println!("revision: {}", report.revision);
      println!("suite: {}", report.suite_hash);
      for check in &report.checks {
        let mark = if check.result.exit_code == Some(0) && !check.result.timed_out {
          "PASS"
        } else if check.result.timed_out {
          "TIMEOUT"
        } else {
          "FAIL"
        };
        println!(
          "[{mark}] {}: {} ({} ms)",
          check.name, check.result.command, check.result.duration_ms
        );
        if mark != "PASS" {
          if !check.result.stderr.trim().is_empty() {
            eprintln!("{}", check.result.stderr.trim());
          }
          if !check.result.stdout.trim().is_empty() {
            eprintln!("{}", check.result.stdout.trim());
          }
        }
      }
      println!(
        "verification: {}",
        if report.passed { "PASS" } else { "FAIL" }
      );
    }
    Ok(if report.passed {
      ExitCode::SUCCESS
    } else {
      ExitCode::from(1)
    })
  }
}
fn human_statement<'a>(contract: &'a EvidenceContract, expected: &str) -> Option<&'a str> {
  match contract {
    EvidenceContract::HumanAttestation { statement } if statement == expected => Some(statement),
    EvidenceContract::All { requirements } | EvidenceContract::Any { requirements } => requirements
      .iter()
      .find_map(|requirement| human_statement(requirement, expected)),
    EvidenceContract::Artifact { .. } | EvidenceContract::HumanAttestation { .. } => None,
  }
}

#[cfg(unix)]
fn read_signing_key(descriptor: i32) -> Result<[u8; 32]> {
  if descriptor < 0 {
    bail!("human attestation signing-key file descriptor must not be negative");
  }
  // SAFETY: the explicit attest command transfers ownership of this inherited descriptor.
  let mut file = unsafe { std::fs::File::from_raw_fd(descriptor as RawFd) };
  let mut encoded = Vec::new();
  Read::by_ref(&mut file)
    .take(66)
    .read_to_end(&mut encoded)
    .context("read human attestation signing key file descriptor")?;
  while matches!(encoded.last(), Some(b'\n' | b'\r')) {
    encoded.pop();
  }
  if encoded.len() != 64 || !encoded.iter().all(u8::is_ascii_hexdigit) {
    encoded.fill(0);
    bail!("human attestation signing key must be exactly 64 hexadecimal characters");
  }
  let mut secret = [0_u8; 32];
  for (destination, pair) in secret.iter_mut().zip(encoded.as_chunks::<2>().0) {
    let high = (pair[0] as char)
      .to_digit(16)
      .context("invalid signing key")?;
    let low = (pair[1] as char)
      .to_digit(16)
      .context("invalid signing key")?;
    *destination = ((high << 4) | low) as u8;
  }
  encoded.fill(0);
  Ok(secret)
}

#[cfg(not(unix))]
fn read_signing_key(_descriptor: i32) -> Result<[u8; 32]> {
  bail!("inherited human signing-key descriptors require a Unix host")
}
