use std::{path::PathBuf, sync::Arc};

use rmcp::{
  ErrorData, Json, ServerHandler, ServiceExt, handler::server::wrapper::Parameters, tool,
  tool_handler, tool_router, transport::stdio,
};
use schemars::Schema;
use tenet_application::{
  application::{ApproveRequest, CandidateCaptureRequest, EvidenceRequest, GateRequest, Tenet},
  response::{
    ApprovalResult, AuthoritySealResult, CandidateCaptureResult, EvidenceResult, GateResult,
    ProposalResult, StatusResult, TenetError,
  },
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
    operation: impl FnOnce(Tenet) -> std::result::Result<T, TenetError> + Send + 'static,
  ) -> Result<T, ErrorData> {
    let guard = self.operation_lock.clone().lock_owned().await;
    let tenet = self.tenet.clone();
    let result = tokio::task::spawn_blocking(move || {
      let _guard = guard;
      operation(tenet)
    })
    .await
    .map_err(|error| {
      ErrorData::internal_error(format!("Tenet operation task failed: {error}"), None)
    })?;
    result.map_err(|error| {
      let data = serde_json::to_value(&error).ok();
      if error.code == "internal_error" {
        ErrorData::internal_error(error.message, data)
      } else {
        ErrorData::invalid_params(error.message, data)
      }
    })
  }
}

#[tool_router]
impl TenetMcp {
  #[tool(
    name = "tenet_status",
    description = "Inspect Tenet status first. It reports specification, contract, policy, and unresolved-obligation state; when the contract is missing and no verifiers are configured, construct verification authority before candidate engineering."
  )]
  async fn status(&self) -> Result<Json<StatusResult>, ErrorData> {
    self.run_operation(|tenet| tenet.status()).await.map(Json)
  }

  #[tool(
    name = "tenet_contract_schema",
    description = "Return the Rust-derived schema for semantic fields agents supply to tenet_contract_propose. Callers reference verifier IDs only; Tenet derives canonical verifier authorities from policy."
  )]
  async fn contract_schema(&self) -> Json<Schema> {
    Json(self.tenet.contract_schema())
  }

  #[tool(
    name = "tenet_policy_schema",
    description = "Return the authoritative Rust-derived policy schema. Before proposing a contract, inspect the specification and use this schema to define suitable verifier(s) in .tenet/tenet.toml when the current policy has none. Editing policy and creating authority-owned oracle assets for authority_snapshot are authority-definition work allowed before sealing A. Project verifiers execute from Candidate Snapshot R; authority_snapshot verifiers execute from a sealed A bundle, with argv[0] naming a bundled executable and TENET_CANDIDATE_ROOT exposing R."
  )]
  async fn policy_schema(&self) -> Json<Schema> {
    Json(self.tenet.policy_schema())
  }

  #[tool(
    name = "tenet_contract_propose",
    description = "Before calling this tool, construct verification authority: inspect the specification, ensure .tenet/tenet.toml contains suitable verifier definitions, use tenet_policy_schema for its format, create authority-owned oracle assets for authority_snapshot verifiers, and re-read tenet_status after changes. Then validate and store a proposal bound to the current specification and policy. Project-authority verifiers remain valid; do not implement candidate product behavior merely because no verifier is configured. Tenet returns the verification profile and warnings for weak configurations."
  )]
  async fn contract_propose(
    &self,
    Parameters(proposal): Parameters<ContractProposalInput>,
  ) -> Result<Json<ProposalResult>, ErrorData> {
    self
      .run_operation(move |tenet| tenet.propose(proposal))
      .await
      .map(Json)
  }

  #[tool(
    name = "tenet_contract_approve",
    description = "Admit the exact pending proposal identified by proposalId and proposalDigest. MUST NOT be called unless the human explicitly approved that exact current proposal after seeing Tenet's returned canonical proposal, ID, digest, verifier mappings, verification profile, and warnings. Silence, generic acknowledgement, or approval of an earlier proposal is not approval. Tenet revalidates identity, digest, specification, policy, and contract semantics before admission."
  )]
  async fn contract_approve(
    &self,
    Parameters(request): Parameters<ApproveRequest>,
  ) -> Result<Json<ApprovalResult>, ErrorData> {
    self
      .run_operation(move |tenet| tenet.approve(&request))
      .await
      .map(Json)
  }
  #[tool(
    name = "tenet_authority_seal",
    description = "Seal the approved authority definition into Authority Capsule A and return authorityId. Call only after exact human approval admitted the current contract. The returned authorityId requires explicit human selection before candidate engineering or candidate capture."
  )]
  async fn authority_seal(&self) -> Result<Json<AuthoritySealResult>, ErrorData> {
    self
      .run_operation(|tenet| tenet.authority_seal())
      .await
      .map(Json)
  }
  #[tool(
    name = "tenet_candidate_capture",
    description = "Capture the candidate root only after a human explicitly selected the exact sealed authorityId and candidate engineering is complete. The capture policy comes only from that Authority Capsule, excludes Tenet administration and common source-control administration by default, rejects symlinks, and returns candidateId."
  )]
  async fn candidate_capture(
    &self,
    Parameters(request): Parameters<CandidateCaptureRequest>,
  ) -> Result<Json<CandidateCaptureResult>, ErrorData> {
    self
      .run_operation(move |tenet| tenet.candidate_capture(&request))
      .await
      .map(Json)
  }

  #[tool(
    name = "tenet_gate",
    description = "Evaluate the explicitly supplied immutable Candidate Snapshot candidateId under explicitly supplied immutable Authority Capsule authorityId. Tenet integrity-checks both, loads all authority semantics only from A, and never reads mutable workspace control state during verification."
  )]
  async fn gate(
    &self,
    Parameters(request): Parameters<GateRequest>,
  ) -> Result<Json<GateResult>, ErrorData> {
    self
      .run_operation(move |tenet| tenet.gate(&request))
      .await
      .map(Json)
  }

  #[tool(
    name = "tenet_evidence",
    description = "Return structured persisted evidence and gate decisions for the explicitly supplied exact authorityId and candidateId pair."
  )]
  async fn evidence(
    &self,
    Parameters(request): Parameters<EvidenceRequest>,
  ) -> Result<Json<EvidenceResult>, ErrorData> {
    self
      .run_operation(move |tenet| tenet.exact_evidence(&request))
      .await
      .map(Json)
  }
}

#[tool_handler(
  name = "tenet",
  version = "0.1.0",
  instructions = "Tenet is an agent-neutral completion authority. Keep authority construction separate from candidate engineering: inspect the specification and tenet_status, ensure suitable verifier definitions exist before tenet_contract_propose, and if none exist use tenet_policy_schema, edit .tenet/tenet.toml, create authority-owned oracle assets for authority_snapshot, and re-read tenet_status. This policy and oracle work is allowed before A is sealed; project-authority verifiers remain valid; do not implement candidate product behavior first. Then obtain explicit human approval, seal A, present authorityId for explicit human selection, engineer and capture R, and claim completion only when tenet_gate returns done for that exact pair."
)]
impl ServerHandler for TenetMcp {}

pub fn run(cwd: PathBuf) -> anyhow::Result<()> {
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
  use rmcp::model::ErrorCode;

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

  #[test]
  fn structured_application_error_survives_mcp_boundary() {
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .expect("build test runtime");
    runtime.block_on(async {
      let server = TenetMcp::new(PathBuf::new());
      let error = server
        .run_operation(|_| {
          Err::<(), _>(
            TenetError::new("oracle_executable_missing", "oracle executable is missing")
              .with_context(
                Some("acceptance".into()),
                Some("verify.sh".into()),
                Some(serde_json::json!({"kind": "missing"})),
              ),
          )
        })
        .await
        .expect_err("operation should fail");
      assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
      let data = error.data.expect("structured error data");
      assert_eq!(
        data.get("code").and_then(serde_json::Value::as_str),
        Some("oracle_executable_missing")
      );
      assert_eq!(
        data.get("verifierId").and_then(serde_json::Value::as_str),
        Some("acceptance")
      );
      assert_eq!(
        data.get("path").and_then(serde_json::Value::as_str),
        Some("verify.sh")
      );
      assert_eq!(
        data
          .get("details")
          .and_then(|value| value.get("kind"))
          .and_then(serde_json::Value::as_str),
        Some("missing")
      );
    });
  }
}
