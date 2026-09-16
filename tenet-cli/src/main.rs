mod cli;
mod mcp;

use std::{path::Path, process::ExitCode, sync::Arc};

use anyhow::{Context, Result};
use clap::Parser;
use tenet_application::{
  application::{AuthoritySubmitRequest, InitializeRequest, RequirementCheckRequest, Tenet},
  response::{ErrorResult, TenetError},
};
use tenet_domain::{
  algebra::{CompletionContractV1, EvaluationId},
  authority::{AdmissionGrant, Finding, Issue},
  completion::Verdict,
  contract::RequirementId,
  evidence::{AuthorityId, ContentObjectId},
};

use crate::cli::{AuthorityCommand, Cli, Command, ReceiptCommand, RequirementCommand};

/// Exit codes are part of the machine-readable contract:
/// 0 success, 1 invalid input or error, 2 NOT_DONE, 3 INCONCLUSIVE,
/// 4 infrastructure failure.
const EXIT_OK: u8 = 0;
const EXIT_ERROR: u8 = 1;
const EXIT_NOT_DONE: u8 = 2;
const EXIT_INCONCLUSIVE: u8 = 3;
const EXIT_INFRASTRUCTURE: u8 = 4;

fn main() -> ExitCode {
  let cli = Cli::parse();
  let json = cli.command.json_requested();
  match run_command(cli) {
    Ok(code) => ExitCode::from(code),
    Err(error) => {
      report_error(error, json);
      ExitCode::from(EXIT_ERROR)
    }
  }
}

fn run_command(cli: Cli) -> Result<u8> {
  let cwd = cli.cwd.unwrap_or(std::env::current_dir()?);
  let tenet = Tenet::new(
    cwd.clone(),
    Arc::new(tenet_workspace::LocalWorkspace),
    Arc::new(tenet_runner::LocalProcessRunner),
    admission_secret()?,
  );
  match cli.command {
    Command::Init { spec, json } => {
      let result = tenet.initialize(&InitializeRequest { spec_path: spec })?;
      if json {
        print_json(&result)?;
      } else {
        println!("initialized: {}", result.initialized);
        println!(
          "specification: {} ({})",
          result.spec_path, result.spec_digest
        );
        println!("skill: {}", result.skill_path);
      }
      Ok(EXIT_OK)
    }
    Command::Doctor { json, receipt } => {
      if let Some(receipt) = receipt {
        return verify_receipt(&tenet, &receipt, json);
      }
      let result = tenet.doctor()?;
      if json {
        print_json(&result)?;
      } else {
        for check in &result.checks {
          println!(
            "{}: {} — {}",
            if check.passed { "PASS" } else { "FAIL" },
            check.name,
            check.detail
          );
        }
      }
      if result.healthy {
        Ok(EXIT_OK)
      } else {
        Err(TenetError::new("doctor_failed", "one or more doctor checks failed").into())
      }
    }
    Command::Status { json } => {
      let result = tenet.context()?;
      if json {
        print_json(&result)?;
      } else {
        println!("phase: {:?}", result.phase);
        if let Some(id) = &result.authority_id {
          println!("authority: {}", id.0.0);
        }
        if let Some(id) = &result.current_candidate_id {
          println!("candidate: {}", id.0.0);
        }
        println!("next: {}", result.next_action);
      }
      Ok(EXIT_OK)
    }
    Command::Authority { command } => run_authority(&tenet, command),
    Command::Requirement {
      command: RequirementCommand::Check { id, json },
    } => {
      let requirement = RequirementId(id);
      let result = tenet.requirement_check(&RequirementCheckRequest {
        requirement_id: requirement,
      })?;
      if json {
        print_json(&result)?;
      } else {
        println!("candidate: {}", result.candidate_id.0.0);
        println!("evaluation: {}", result.evaluation_id.0.0);
        println!("state: {:?}", result.result.state);
      }
      Ok(EXIT_OK)
    }
    Command::Verify { json } => {
      let result = tenet.verify()?;
      if json {
        print_json(&result)?;
      } else {
        println!("candidate: {}", result.candidate_id.0.0);
        println!("evaluation: {}", result.evaluation_id.0.0);
        println!("verdict: {:?}", result.verdict);
        if let Some(reason) = &result.reason {
          println!("reason: {reason}");
        }
      }
      Ok(match result.verdict {
        Verdict::Done => EXIT_OK,
        Verdict::NotDone => EXIT_NOT_DONE,
        Verdict::Inconclusive => EXIT_INCONCLUSIVE,
        Verdict::InfrastructureError => EXIT_INFRASTRUCTURE,
      })
    }
    Command::Blockers { json } => {
      let result = tenet.blockers()?;
      if json {
        print_json(&result)?;
      } else {
        println!("phase: {:?}", result.phase);
        for blocker in &result.blockers {
          println!("{}: {}", blocker.code, blocker.message);
        }
        if result.blockers.is_empty() {
          println!("no blockers");
        }
      }
      Ok(EXIT_OK)
    }
    Command::Evidence { requirement, json } => {
      let requirement = requirement.map(RequirementId);
      let result = tenet.evidence(requirement.as_ref())?;
      if json {
        print_json(&result)?;
      } else {
        println!("evaluation: {}", result.evaluation_id.0.0);
        println!("candidate: {}", result.evaluation.candidate.0.0);
        println!("state: {:?}", result.result.state);
        for run in &result.evaluation.runs {
          println!(
            "verifier {}: {:?} ({:?})",
            run.verifier.0, run.observation.result, run.context.assurance.0
          );
        }
      }
      Ok(EXIT_OK)
    }
    Command::Receipt {
      command: ReceiptCommand::Verify { id, json },
    } => verify_receipt(&tenet, &id, json),
    Command::Mcp => {
      mcp::run(cwd)?;
      Ok(EXIT_OK)
    }
    Command::Version => {
      println!("tenet {}", env!("CARGO_PKG_VERSION"));
      Ok(EXIT_OK)
    }
  }
}

fn run_authority(tenet: &Tenet, command: AuthorityCommand) -> Result<u8> {
  match command {
    AuthorityCommand::Prepare {
      contract,
      issues,
      json,
    } => {
      let contract: CompletionContractV1 = read_json(&contract)?;
      let issues: Vec<Issue> = issues
        .map(|path| read_json(&path))
        .transpose()?
        .unwrap_or_default();
      let result = tenet.authority_submit(AuthoritySubmitRequest::Proposal { contract, issues })?;
      if json {
        print_json(&result)?;
      } else {
        println!("proposal submitted");
      }
      Ok(EXIT_OK)
    }
    AuthorityCommand::Reconcile {
      proposal,
      findings,
      json,
    } => {
      let proposal_id = parse_proposal_id(&proposal)?;
      let findings: Vec<Finding> = findings
        .map(|path| read_json(&path))
        .transpose()?
        .unwrap_or_default();
      let result = tenet.authority_submit(AuthoritySubmitRequest::Reconciliation {
        proposal_id,
        findings,
      })?;
      if json {
        print_json(&result)?;
      } else {
        println!("reconciliation submitted");
      }
      Ok(EXIT_OK)
    }
    AuthorityCommand::Clarify {
      proposal,
      text,
      json,
    } => {
      let proposal_id = parse_proposal_id(&proposal)?;
      let result = tenet.authority_submit(AuthoritySubmitRequest::Clarification {
        proposal_id,
        clarification: text,
      })?;
      if json {
        print_json(&result)?;
      } else {
        println!("clarification submitted");
      }
      Ok(EXIT_OK)
    }
    AuthorityCommand::Grant {
      proposal,
      authority,
      json,
    } => {
      let proposal_id = parse_proposal_id(&proposal)?;
      let authority_id = parse_authority_id(&authority)?;
      let grant = tenet.mint_admission_grant(&proposal_id, &authority_id)?;
      if json {
        print_json(&grant)?;
      } else {
        println!("grant minted; store it where the admission step can read it");
      }
      Ok(EXIT_OK)
    }
    AuthorityCommand::Admit {
      proposal,
      reconciliation,
      authority,
      grant,
      json,
    } => {
      let proposal_id = parse_proposal_id(&proposal)?;
      let reconciliation_id = tenet_domain::authority::ReconciliationReportId(
        ContentObjectId::new(reconciliation).map_err(anyhow::Error::msg)?,
      );
      let authority_id = parse_authority_id(&authority)?;
      let grant: AdmissionGrant = read_json(&grant)?;
      let result = tenet.authority_submit(AuthoritySubmitRequest::Admission {
        proposal_id,
        reconciliation_id,
        authority_id,
        grant,
      })?;
      if json {
        print_json(&result)?;
      } else {
        println!("authority admitted");
      }
      Ok(EXIT_OK)
    }
    AuthorityCommand::Inspect { json } => {
      let result = tenet.authority_inspect()?;
      if json {
        print_json(&result)?;
      } else {
        match &result.active {
          Some(active) => println!(
            "active admission: {} (authority {})",
            active.admission_id.0.0, active.authority_id.0.0
          ),
          None => println!("no active admission"),
        }
        if let Some(proposal) = &result.proposal {
          println!(
            "proposal: {} (authority {})",
            proposal.proposal_id.0.0, proposal.authority_id.0.0
          );
        }
        if let Some(reconciliation) = &result.reconciliation {
          println!(
            "reconciliation: {} (proposal {})",
            reconciliation.reconciliation_id.0.0, reconciliation.proposal_id.0.0
          );
        }
      }
      Ok(EXIT_OK)
    }
  }
}

fn verify_receipt(tenet: &Tenet, receipt: &str, json: bool) -> Result<u8> {
  let receipt = EvaluationId(ContentObjectId::new(receipt.to_owned()).map_err(anyhow::Error::msg)?);
  match tenet.receipt_verify(&receipt) {
    Ok(result) => {
      if json {
        print_json(&result)?;
      } else {
        println!("verified receipt: {}", result.receipt_id.0.0);
        println!("authority: {}", result.authority_id.0.0);
        println!("candidate: {}", result.candidate_id.0.0);
        println!("verdict: {:?}", result.verdict);
      }
      Ok(EXIT_OK)
    }
    Err(error) if error.code == "receipt_not_complete" => {
      let encoded: ErrorResult = error.clone().into();
      if json {
        print_json(&encoded)?;
      } else {
        eprintln!("error: {}", error.message);
      }
      Ok(EXIT_NOT_DONE)
    }
    Err(error) => Err(error.into()),
  }
}

fn parse_proposal_id(value: &str) -> Result<tenet_domain::authority::ProposalId> {
  Ok(tenet_domain::authority::ProposalId(
    ContentObjectId::new(value.to_owned()).map_err(anyhow::Error::msg)?,
  ))
}

fn parse_authority_id(value: &str) -> Result<AuthorityId> {
  Ok(AuthorityId(
    ContentObjectId::new(value.to_owned()).map_err(anyhow::Error::msg)?,
  ))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
  let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
  serde_json::from_slice(&bytes).with_context(|| format!("parse JSON {}", path.display()))
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
  println!("{}", serde_json::to_string_pretty(value)?);
  Ok(())
}

/// Parse the trusted admission secret from `TENET_ADMISSION_SECRET` (hex).
/// The secret is held only in the operator process environment; it is never
/// stored in the repository. A malformed value fails closed.
pub(crate) fn admission_secret() -> Result<Option<Vec<u8>>> {
  let Some(value) = std::env::var_os("TENET_ADMISSION_SECRET") else {
    return Ok(None);
  };
  let value = value.to_string_lossy().trim().to_owned();
  if value.is_empty() {
    return Ok(None);
  }
  if value.len() % 2 != 0 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    return Err(anyhow::anyhow!(
      "TENET_ADMISSION_SECRET must be a hexadecimal string"
    ));
  }
  let mut secret = Vec::with_capacity(value.len() / 2);
  for pair in value.as_bytes().chunks(2) {
    let high = hex_digit(pair[0])?;
    let low = hex_digit(pair[1])?;
    secret.push((high << 4) | low);
  }
  Ok(Some(secret))
}

fn hex_digit(byte: u8) -> Result<u8> {
  match byte {
    b'0'..=b'9' => Ok(byte - b'0'),
    b'a'..=b'f' => Ok(byte - b'a' + 10),
    b'A'..=b'F' => Ok(byte - b'A' + 10),
    _ => Err(anyhow::anyhow!(
      "TENET_ADMISSION_SECRET must be a hexadecimal string"
    )),
  }
}

fn report_error(error: anyhow::Error, json: bool) {
  let typed = error
    .chain()
    .find_map(|cause| cause.downcast_ref::<TenetError>().cloned())
    .unwrap_or_else(|| TenetError::new("internal_error", error.to_string()));
  let error: ErrorResult = typed.into();
  if json {
    match serde_json::to_string_pretty(&error) {
      Ok(encoded) => println!("{encoded}"),
      Err(encoding_error) => eprintln!(
        "error: {}; additionally failed to encode JSON error: {encoding_error}",
        error.message
      ),
    }
  } else {
    eprintln!("error: {}", error.message);
  }
}
