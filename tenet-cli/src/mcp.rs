use std::{path::PathBuf, sync::Arc};

use rmcp::{
  ErrorData, Json, ServerHandler, ServiceExt, handler::server::wrapper::Parameters, tool,
  tool_handler, tool_router, transport::stdio,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tenet_application::{
  application::{AuthoritySubmitRequest, RequirementCheckRequest, Tenet},
  response::{
    AuthoritySubmissionResult, ContextResult, RequirementCheckResult, TenetError, VerifyResult,
  },
};
use tokio::sync::Mutex;
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AuthoritySubmitParameters {
  submission: AuthoritySubmitRequest,
}

#[derive(Clone)]
pub struct TenetMcp {
  tenet: Tenet,
  operation_lock: Arc<Mutex<()>>,
}

impl TenetMcp {
  pub fn new(cwd: PathBuf, admission_secret: Option<Vec<u8>>) -> Self {
    Self {
      tenet: Tenet::new(
        cwd,
        Arc::new(tenet_workspace::LocalWorkspace),
        Arc::new(tenet_runner::LocalProcessRunner),
        admission_secret,
      ),
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
    .map_err(|error| ErrorData::internal_error(format!("Tenet operation failed: {error}"), None))?;
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
    name = "tenet_context",
    description = "Derive the current Tenet phase, exact active Admission/Authority/CompletionPolicy identities, current Candidate when available, Requirement-check status, and next action from persisted immutable objects and refs. Workflow phase is never persisted."
  )]
  async fn context(&self) -> Result<Json<ContextResult>, ErrorData> {
    self.run_operation(|tenet| tenet.context()).await.map(Json)
  }

  #[tool(
    name = "tenet_authority_submit",
    description = "Submit exactly one authority lifecycle stage: PROPOSAL, RECONCILIATION, CLARIFICATION, or ADMISSION. Every transition is content-addressed and bound to exact Proposal, Reconciliation, and Authority identities. ADMISSION additionally requires a trusted admission grant bound to the exact proposal and authority; the kernel verifies its mac under the trusted secret, which the candidate producer cannot possess. MCP user input is not cryptographic identity and descriptions are not the enforcement."
  )]
  async fn authority_submit(
    &self,
    Parameters(request): Parameters<AuthoritySubmitParameters>,
  ) -> Result<Json<AuthoritySubmissionResult>, ErrorData> {
    self
      .run_operation(move |tenet| tenet.authority_submit(request.submission))
      .await
      .map(Json)
  }

  #[tool(
    name = "tenet_requirement_check",
    description = "Capture the current Candidate and run every verifier for one Requirement with a fresh Candidate materialization per verifier. Persist Candidate-specific development Evaluation evidence; this operation never returns protocol-level DONE and never promotes its runs into final evidence."
  )]
  async fn requirement_check(
    &self,
    Parameters(request): Parameters<RequirementCheckRequest>,
  ) -> Result<Json<RequirementCheckResult>, ErrorData> {
    self
      .run_operation(move |tenet| tenet.requirement_check(&request))
      .await
      .map(Json)
  }

  #[tool(
    name = "tenet_verify",
    description = "Capture one final Candidate, rerun every required verifier using a fresh Candidate materialization for each, persist one Final Evaluation, and derive completion in the kernel. This is the only protocol operation that can return DONE; a post-verification Candidate change returns INCONCLUSIVE without reassigning historical evidence."
  )]
  async fn verify(&self) -> Result<Json<VerifyResult>, ErrorData> {
    self.run_operation(|tenet| tenet.verify()).await.map(Json)
  }
}

#[tool_handler(
  name = "tenet",
  version = "0.7.0",
  instructions = "Tenet exposes exactly four completion operations: context, staged authority submission, requirement checking, and final verification. The complete lifecycle is also available through the tenet CLI with identical kernel semantics; MCP is an optional adapter. ADMISSION requires a trusted admission grant the candidate producer cannot mint. LOCAL_V1 is not same-user tamper resistance; PROTECTED_V1 is enforced read-only Candidate/Authority verification with separate writable scratch and controlled output, and the runner fails closed when the platform cannot enforce it. AuthorityBound is not independent authorship; fresh materialization is not sandboxing; content addressing is not writer authentication; MCP user input is not cryptographic human identity; verifier Pass is not task completion. Only tenet_verify can return DONE after kernel evaluation of the exact active Admission, Authority, Candidate, and Final Evaluation."
)]
impl ServerHandler for TenetMcp {}
pub fn run(cwd: PathBuf) -> anyhow::Result<()> {
  // A malformed secret degrades to `None` so the server still serves reads;
  // every admission attempt then fails closed with `admission_secret_unavailable`.
  let admission_secret = crate::admission_secret().unwrap_or(None);
  tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?
    .block_on(async move {
      let service = TenetMcp::new(cwd, admission_secret).serve(stdio()).await?;
      service.waiting().await?;
      Ok(())
    })
}
