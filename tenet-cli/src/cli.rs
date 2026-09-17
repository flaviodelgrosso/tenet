use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
  name = "tenet",
  about = "Completion authority for exact admitted Authority and Candidate identities"
)]
pub struct Cli {
  #[arg(long, global = true, value_name = "DIR")]
  pub cwd: Option<PathBuf>,
  #[command(subcommand)]
  pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
  /// Initialize repository-contained Tenet state and integrations.
  Init {
    #[arg(long, value_name = "PATH")]
    spec: Option<PathBuf>,
    #[arg(long)]
    json: bool,
  },
  /// Validate repository, semantic, integrity, and integration invariants.
  Doctor {
    /// Verify a canonical Final Evaluation receipt by content identity.
    #[arg(long, value_name = "EVALUATION_ID")]
    receipt: Option<String>,
    #[arg(long)]
    json: bool,
  },
  /// Derive the current phase, identities, and requirement checks.
  Status {
    #[arg(long)]
    json: bool,
  },
  /// Authority lifecycle: propose, reconcile, clarify, grant, admit, inspect.
  Authority {
    #[command(subcommand)]
    command: AuthorityCommand,
  },
  /// Requirement-scoped verification against the exact active Admission.
  Requirement {
    #[command(subcommand)]
    command: RequirementCommand,
  },
  /// Run the Final Evaluation for the current Candidate. Exit code reflects the verdict.
  Verify {
    #[arg(long)]
    json: bool,
  },
  /// List the current blocking items derived from persisted facts.
  Blockers {
    #[arg(long)]
    json: bool,
  },
  /// Read persisted verifier evidence for the Final or one requirement Evaluation.
  Evidence {
    /// Inspect one requirement-scoped Evaluation instead of the Final Evaluation.
    #[arg(long, value_name = "REQUIREMENT_ID")]
    requirement: Option<String>,
    #[arg(long)]
    json: bool,
  },
  /// Receipt verification by content identity.
  Receipt {
    #[command(subcommand)]
    command: ReceiptCommand,
  },
  /// Run the four-operation Model Context Protocol server over stdio.
  Mcp,
  /// Print the Tenet executable version.
  Version,
}

#[derive(Subcommand)]
pub enum AuthorityCommand {
  /// Stage the authority surface and submit a PROPOSAL binding the exact contract.
  Prepare {
    /// CompletionContractV1 JSON file describing requirements, criteria, and verifiers.
    #[arg(long, value_name = "FILE")]
    contract: PathBuf,
    /// Optional JSON file containing an array of proposal issues.
    #[arg(long, value_name = "FILE")]
    issues: Option<PathBuf>,
    #[arg(long)]
    json: bool,
  },
  /// Submit RECONCILIATION for the exact proposal.
  Reconcile {
    #[arg(long, value_name = "PROPOSAL_ID")]
    proposal: String,
    /// Optional JSON file containing an array of reconciliation findings.
    #[arg(long, value_name = "FILE")]
    findings: Option<PathBuf>,
    #[arg(long)]
    json: bool,
  },
  /// Submit a CLARIFICATION for the exact proposal.
  Clarify {
    #[arg(long, value_name = "PROPOSAL_ID")]
    proposal: String,
    #[arg(long, value_name = "TEXT")]
    text: String,
    #[arg(long)]
    json: bool,
  },
  /// Trusted: mint an admission grant bound to the exact proposal and authority.
  /// Requires the trusted admission secret in this process; the candidate
  /// producer cannot mint valid grants without it.
  Grant {
    #[arg(long, value_name = "PROPOSAL_ID")]
    proposal: String,
    #[arg(long, value_name = "AUTHORITY_ID")]
    authority: String,
    #[arg(long)]
    json: bool,
  },
  /// Trusted handoff: derive the exact prepared proposal, reconciliation, and
  /// authority from repository state, mint the admission grant under the trusted
  /// admission secret, and submit ADMISSION in one step. Requires the trusted
  /// admission secret in this process; the candidate producer must run it only
  /// through a trusted context (the harness approval path or the operator's own
  /// shell), never with the secret in its own environment.
  AdmitPrepared {
    #[arg(long)]
    json: bool,
  },
  /// Admit the exact proposal, reconciliation, and authority under a trusted grant.
  Admit {
    #[arg(long, value_name = "PROPOSAL_ID")]
    proposal: String,
    #[arg(long, value_name = "RECONCILIATION_ID")]
    reconciliation: String,
    #[arg(long, value_name = "AUTHORITY_ID")]
    authority: String,
    /// JSON file containing the trusted admission grant.
    #[arg(long, value_name = "FILE")]
    grant: PathBuf,
    #[arg(long)]
    json: bool,
  },
  /// Inspect the exact proposal, reconciliation, and active admitted chain.
  Inspect {
    #[arg(long)]
    json: bool,
  },
}

#[derive(Subcommand)]
pub enum RequirementCommand {
  /// Capture the current Candidate, run the requirement's verifiers, and persist a
  /// requirement-scoped Evaluation. Development evidence only.
  Check {
    #[arg(long, value_name = "REQUIREMENT_ID")]
    id: String,
    #[arg(long)]
    json: bool,
  },
}

#[derive(Subcommand)]
pub enum ReceiptCommand {
  /// Verify that a Final Evaluation receipt content-derives DONE.
  Verify {
    #[arg(long, value_name = "EVALUATION_ID")]
    id: String,
    #[arg(long)]
    json: bool,
  },
}

impl Command {
  pub(crate) fn json_requested(&self) -> bool {
    match self {
      Self::Init { json, .. }
      | Self::Doctor { json, .. }
      | Self::Status { json }
      | Self::Verify { json }
      | Self::Blockers { json }
      | Self::Evidence { json, .. } => *json,
      Self::Mcp | Self::Version => false,
      Self::Authority { command } => match command {
        AuthorityCommand::Prepare { json, .. }
        | AuthorityCommand::Reconcile { json, .. }
        | AuthorityCommand::Clarify { json, .. }
        | AuthorityCommand::Grant { json, .. }
        | AuthorityCommand::AdmitPrepared { json }
        | AuthorityCommand::Admit { json, .. }
        | AuthorityCommand::Inspect { json } => *json,
      },
      Self::Requirement {
        command: RequirementCommand::Check { json, .. },
      } => *json,
      Self::Receipt {
        command: ReceiptCommand::Verify { json, .. },
      } => *json,
    }
  }
}
