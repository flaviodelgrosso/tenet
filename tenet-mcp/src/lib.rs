use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use rmcp::{
  Json, ServerHandler, ServiceExt, handler::server::wrapper::Parameters, tool, tool_handler,
  tool_router, transport::stdio,
};
use schemars::Schema;
use tenet_application::{
  application::{ApproveRequest, EvidenceRequest, GateRequest, Tenet},
  response::{ApprovalResult, EvidenceResult, GateResult, ProposalResult, StatusResult},
};
use tenet_domain::contract::ContractProposalInput;
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub struct TenetMcp {
  tenet: Tenet,
  operation_lock: Arc<Mutex<()>>,
}

impl TenetMcp {
  pub fn new(cwd: PathBuf) -> Self {
    Self {
      tenet: Tenet::new(cwd),
      operation_lock: Arc::new(Mutex::new(())),
    }
  }

  async fn run_operation<T: Send + 'static>(
    &self,
    operation: impl FnOnce(Tenet) -> Result<T> + Send + 'static,
  ) -> Result<T, String> {
    let guard = self.operation_lock.clone().lock_owned().await;
    let tenet = self.tenet.clone();
    tokio::task::spawn_blocking(move || {
      let _guard = guard;
      operation(tenet)
    })
    .await
    .map_err(|error| format!("Tenet operation task failed: {error}"))?
    .map_err(|error| format!("{error:#}"))
  }
}

#[tool_router]
impl TenetMcp {
  #[tool(
    name = "tenet_status",
    description = "Inspect current Tenet initialization, specification, contract, digest, last-gate, verdict, and unresolved-obligation state without executing verifiers."
  )]
  async fn status(&self) -> Result<Json<StatusResult>, String> {
    self.run_operation(|tenet| tenet.status()).await.map(Json)
  }

  #[tool(
    name = "tenet_contract_schema",
    description = "Return the Rust-derived schema for semantic fields agents supply to tenet_contract_propose. Tenet assigns canonical persisted fields such as schemaVersion internally."
  )]
  async fn contract_schema(&self) -> Json<Schema> {
    Json(self.tenet.contract_schema())
  }

  #[tool(
    name = "tenet_policy_schema",
    description = "Return the authoritative verification-policy schema generated from Tenet's Rust RepositoryConfig type."
  )]
  async fn policy_schema(&self) -> Json<Schema> {
    Json(self.tenet.policy_schema())
  }

  #[tool(
    name = "tenet_contract_propose",
    description = "Validate and store a completion-contract proposal bound to the current specification and policy. Supply the semantic proposal fields from tenet_contract_schema; Tenet assigns its canonical schema version. Returns its exact deterministic proposal ID and digest pending human approval."
  )]
  async fn contract_propose(
    &self,
    Parameters(proposal): Parameters<ContractProposalInput>,
  ) -> Result<Json<ProposalResult>, String> {
    self
      .run_operation(move |tenet| tenet.propose(proposal))
      .await
      .map(Json)
  }

  #[tool(
    name = "tenet_contract_approve",
    description = "Admit the exact pending proposal identified by proposalId and proposalDigest. MUST NOT be called unless the human explicitly approved that exact current proposal after seeing its ID, digest, requirements, obligations, verifier mappings, and assurances. Silence, generic acknowledgement, or approval of an earlier proposal is not approval. Tenet revalidates identity, digest, specification, policy, and contract semantics before admission."
  )]
  async fn contract_approve(
    &self,
    Parameters(request): Parameters<ApproveRequest>,
  ) -> Result<Json<ApprovalResult>, String> {
    self
      .run_operation(move |tenet| tenet.approve(&request))
      .await
      .map(Json)
  }

  #[tool(
    name = "tenet_gate",
    description = "Evaluate one explicitly supplied exact candidate revision under one explicitly supplied exact authority revision. Returns the structured deterministic verdict, typed obligation results, and blockers; not_done, inconclusive, and infrastructure_error are normal Tenet outcomes rather than transport errors."
  )]
  async fn gate(
    &self,
    Parameters(request): Parameters<GateRequest>,
  ) -> Result<Json<GateResult>, String> {
    self
      .run_operation(move |tenet| tenet.gate(&request))
      .await
      .map(Json)
  }

  #[tool(
    name = "tenet_evidence",
    description = "Return structured persisted evidence and gate decisions for the explicitly supplied exact candidate revision using Tenet's evidence model."
  )]
  async fn evidence(
    &self,
    Parameters(request): Parameters<EvidenceRequest>,
  ) -> Result<Json<EvidenceResult>, String> {
    self
      .run_operation(move |tenet| tenet.exact_evidence(&request))
      .await
      .map(Json)
  }
}

#[tool_handler(
  name = "tenet",
  version = "0.1.0",
  instructions = "Tenet is an agent-neutral completion authority. Inspect status first, obtain explicit human approval before contract admission, pass exact authority and candidate revisions to gate, and claim completion only when the returned verdict is done."
)]
impl ServerHandler for TenetMcp {}

pub fn run(cwd: PathBuf) -> Result<()> {
  tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?
    .block_on(async move {
      let service = TenetMcp::new(cwd).serve(stdio()).await?;
      service.waiting().await?;
      Ok(())
    })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn aborted_request_holds_lock_until_blocking_operation_finishes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .expect("build test runtime");
    runtime.block_on(async {
      let server = TenetMcp::new(PathBuf::new());
      let (first_started_sender, first_started) = tokio::sync::oneshot::channel();
      let (release_sender, release) = std::sync::mpsc::channel();
      let first_server = server.clone();
      let first = tokio::spawn(async move {
        first_server
          .run_operation(move |_| {
            first_started_sender
              .send(())
              .map_err(|_| anyhow::anyhow!("first-start receiver dropped"))?;
            release
              .recv()
              .map_err(|error| anyhow::anyhow!("release sender dropped: {error}"))?;
            Ok(())
          })
          .await
      });
      first_started
        .await
        .expect("first blocking operation started");
      first.abort();
      assert!(
        first
          .await
          .expect_err("request task should abort")
          .is_cancelled()
      );

      let (second_started_sender, mut second_started) = tokio::sync::oneshot::channel();
      let second = tokio::spawn(async move {
        server
          .run_operation(move |_| {
            second_started_sender
              .send(())
              .map_err(|_| anyhow::anyhow!("second-start receiver dropped"))?;
            Ok(())
          })
          .await
      });
      tokio::task::yield_now().await;
      tokio::task::yield_now().await;
      assert!(matches!(
        second_started.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
      ));

      release_sender.send(()).expect("release first operation");
      second_started
        .await
        .expect("second operation started after release");
      second
        .await
        .expect("join second request")
        .expect("run second operation");
    });
  }
}
