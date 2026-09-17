use std::{path::PathBuf, sync::Arc};

use rmcp::{
  ErrorData, Json, RoleServer, ServerHandler, ServiceExt,
  handler::server::wrapper::Parameters,
  model::{
    Implementation, ListResourcesResult, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, Resource, ResourceContents, ServerCapabilities, ServerConfig,
  },
  service::RequestContext,
  tool, tool_handler, tool_router,
  transport::stdio,
};
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;
use tenet_application::{
  application::{AuthoritySubmitRequest, RequirementCheckRequest, Tenet},
  response::{
    AuthoritySubmissionResult, ContextResult, RequirementCheckResult, TenetError, VerifyResult,
  },
};
use tenet_domain::policy::ProjectConfig;
use tokio::sync::Mutex;

const AUTHORING_RESOURCE_URI: &str = "tenet://authoring/configuration";
const MCP_INSTRUCTIONS: &str = "Tenet exposes exactly four completion operations: context, staged authority submission, requirement checking, and final verification. Read tenet://authoring/configuration before configuring .tenet/tenet.toml: it exposes the JSON Schema generated from Tenet's authoritative ProjectConfig type. At AUTHORITY_REQUIRED, use tenet_context authoring facts to identify missing Candidate capture or verifier prerequisites, then use the authority-submit tool schema for the contract and staged request shape. Verifier definitions live only in .tenet/tenet.toml; Contract Criteria only reference configured verifier IDs, and reference IDs are unique across the Contract, so two Criteria never share one verifier and never rename the same command into a second definition to satisfy uniqueness. Candidate implementation may occur before Admission, but requirement checks and final verification are authoritative only under an admitted Authority; prefer admitting the Authority before implementing when practical. ADMISSION requires a trusted admission grant the candidate producer cannot mint; at AUTHORITY_ADMISSION tenet_context returns an admission preview carrying a human-readable summary and the exact Proposal, Reconciliation, and Authority identities: present it through the host agent's native confirmation mechanism with Review, Admit-and-continue, or Stop choices, never making the user copy content IDs, and on approval run the preview's trusted handoff command tenet authority admit-prepared --json in a context holding the trusted admission secret; the producer must never run tenet authority grant, not even as a fail-closed probe, never put TENET_ADMISSION_SECRET into its own environment, and user approval is not an AdmissionGrant; after the handoff, call tenet_context again and continue from the derived phase. Every persisted Admission is cryptographically revalidated on each trusted load and verification requires the trusted admission secret in-process. LOCAL_V1 is not same-user tamper resistance. PROTECTED_V1 verification runs only against an OS-enforced immutable view (macOS read-only volume with unlinked backing store, Linux private namespace copy verified against trusted digests before exec), never against a repository materialization. AuthorityBound is not independent authorship. Fresh materialization is not sandboxing. Content addressing is not writer authentication and MCP input is not cryptographic human identity. verifier Pass is not task completion; only kernel evaluation through tenet_verify can return DONE.";

fn authoring_resource() -> Result<String, ErrorData> {
  serde_json::to_string_pretty(&serde_json::json!({
    "schemaVersion": 1,
    "configurationSemantics": "tenet:authoring-configuration-resource:v1",
    "configPath": ".tenet/tenet.toml",
    "configurationSchema": schema_for!(ProjectConfig),
  }))
  .map_err(|error| {
    ErrorData::internal_error(format!("serialize authoring metadata: {error}"), None)
  })
}
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

#[tool_handler(name = "tenet", version = "0.7.0")]
impl ServerHandler for TenetMcp {
  fn get_info(&self) -> ServerConfig {
    ServerConfig::new(
      ServerCapabilities::builder()
        .enable_resources()
        .enable_tools()
        .build(),
    )
    .with_server_info(Implementation::new("tenet", "0.7.0"))
    .with_instructions(MCP_INSTRUCTIONS.to_string())
  }

  async fn list_resources(
    &self,
    _: Option<rmcp::model::PaginatedRequestParams>,
    _: RequestContext<RoleServer>,
  ) -> Result<ListResourcesResult, ErrorData> {
    Ok(ListResourcesResult::with_all_items(vec![
      Resource::new(AUTHORING_RESOURCE_URI, "Tenet authoring configuration")
        .with_description(
          "Canonical .tenet/tenet.toml schema generated from Tenet's ProjectConfig Rust type.",
        )
        .with_mime_type("application/schema+json"),
    ]))
  }

  async fn read_resource(
    &self,
    request: ReadResourceRequestParams,
    _: RequestContext<RoleServer>,
  ) -> Result<ReadResourceResponse, ErrorData> {
    if request.uri != AUTHORING_RESOURCE_URI {
      return Err(ErrorData::resource_not_found("resource not found", None));
    }
    let content = ResourceContents::text(authoring_resource()?, AUTHORING_RESOURCE_URI)
      .with_mime_type("application/schema+json");
    Ok(ReadResourceResult::new(vec![content]).into())
  }
}
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
