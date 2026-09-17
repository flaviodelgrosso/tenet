//! Regression coverage for the agent-facing workflow guidance.
//!
//! A fresh coding agent follows the generated Skill, the derived context
//! guidance, and the MCP instructions — not the implementation. These tests
//! pin the clarifications that failed in real sessions: verifier definitions
//! vs Contract references, the Admission-vs-implementation ordering, the
//! producer-side `tenet authority grant` ban, and the native approval UX at
//! `AUTHORITY_ADMISSION` that must not require copying content IDs.

use std::{fs, path::Path, process::Command};

use tenet_domain::{
  algebra::{
    AssuranceRequirementV1, COMPLETION_POLICY_V1, CompletionContractV1, CompletionPolicyId,
    Criterion, CriterionId, EvidenceControlRequirementV1, EvidenceRequirementV1, Requirement,
    Verifier, VerifierId, VerifierMaterial,
  },
  policy::{CandidateCapturePolicy, ProjectConfig},
};

fn tenet(root: &Path, args: &[&str], secret: Option<&str>) -> std::process::Output {
  let mut command = Command::new(env!("CARGO_BIN_EXE_tenet"));
  command.arg("--cwd").arg(root).args(args);
  command.env_remove("TENET_ADMISSION_SECRET");
  if let Some(secret) = secret {
    command.env("TENET_ADMISSION_SECRET", secret);
  }
  command.output().expect("run tenet")
}

fn ok_json(root: &Path, args: &[&str]) -> serde_json::Value {
  let output = tenet(root, args, None);
  assert!(
    output.status.success(),
    "tenet {args:?} failed: {}{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  serde_json::from_slice(&output.stdout).expect("JSON output")
}

fn init_repo(root: &Path) {
  fs::write(root.join("SPEC.md"), "# Specification\n").expect("spec");
  let output = tenet(root, &["init", "--json"], None);
  assert!(output.status.success(), "init failed");
}

fn generated_skill(root: &Path) -> String {
  fs::read_to_string(root.join(".agents/skills/tenet/SKILL.md")).expect("generated skill")
}

#[test]
fn generated_skill_separates_verifier_definitions_from_contract_references() {
  let directory = tempfile::tempdir().expect("repository");
  init_repo(directory.path());
  let skill = generated_skill(directory.path());
  for claim in [
    "Verifier definitions vs Contract references",
    "Definitions exist only in the config",
    "must exactly match an already-configured definition",
    "duplicate verifier identifier",
    "Two Criteria may not share one verifier",
    "verifier_not_configured",
    "verifier_material_mismatch",
  ] {
    assert!(skill.contains(claim), "skill omits {claim:?}");
  }
  assert!(
    !skill.contains("Multiple Criteria may use the same verifier"),
    "skill re-introduces the shared-verifier claim the kernel rejects"
  );
}

#[test]
fn generated_skill_states_the_admission_ordering_rule() {
  let directory = tempfile::tempdir().expect("repository");
  init_repo(directory.path());
  let skill = generated_skill(directory.path());
  for claim in [
    "Candidate implementation may occur before Admission",
    "admission_missing",
    "Prefer admitting the Authority before implementing",
  ] {
    assert!(skill.contains(claim), "skill omits {claim:?}");
  }
}

#[test]
fn generated_skill_bans_producer_side_grant_probing() {
  let directory = tempfile::tempdir().expect("repository");
  init_repo(directory.path());
  let skill = generated_skill(directory.path());
  for claim in [
    "never run `tenet authority grant`",
    "not even as a fail-closed probe",
    "Proposal ID, Reconciliation ID, and Authority ID",
    "stop all authority progression",
  ] {
    assert!(skill.contains(claim), "skill omits {claim:?}");
  }
}

#[test]
fn context_next_action_at_admission_routes_to_the_trusted_handoff() {
  let directory = tempfile::tempdir().expect("repository");
  let root = directory.path();
  init_repo(root);
  fs::write(root.join("candidate.txt"), "candidate").expect("candidate");
  fs::write(root.join("verify.sh"), "#!/bin/sh\nexit 0\n").expect("verifier script");
  #[cfg(unix)]
  fs::set_permissions(
    root.join("verify.sh"),
    std::os::unix::fs::PermissionsExt::from_mode(0o755),
  )
  .expect("executable");
  let policy = ProjectConfig {
    version: 1,
    spec_path: "SPEC.md".into(),
    candidate: CandidateCapturePolicy {
      root: ".".into(),
      include: vec!["candidate.txt".into(), "verify.sh".into()],
      exclude: vec![],
    },
    verifiers: vec![tenet_domain::policy::VerifierSpec {
      id: "V1".into(),
      command: tenet_domain::policy::CommandSpec {
        argv: vec![tenet_domain::policy::CommandArgument::CandidatePath(
          "verify.sh".into(),
        )],
        cwd: tenet_domain::policy::CommandCwd::Candidate(".".into()),
        env: tenet_domain::policy::EnvironmentSpec::default(),
        timeout_ms: 5_000,
        result: tenet_domain::policy::ExitCodePolicy {
          pass: [0].into(),
          fail: [1].into(),
          inconclusive: [].into(),
        },
      },
      max_output_bytes: 4_096,
      authority: tenet_domain::policy::VerifierAuthority::Project,
      oracle_path: None,
      protection: tenet_domain::policy::VerifierProtection::Local,
    }],
  };
  fs::write(
    root.join(".tenet/tenet.toml"),
    toml::to_string_pretty(&policy).expect("policy"),
  )
  .expect("write policy");
  let contract = CompletionContractV1 {
    schema_version: 1,
    policy: CompletionPolicyId(COMPLETION_POLICY_V1.into()),
    requirements: vec![Requirement {
      id: tenet_domain::contract::RequirementId("R1".into()),
      statement: "the candidate is complete".into(),
      criteria: vec![Criterion {
        id: CriterionId("C1".into()),
        proposition: "criterion C1 holds".into(),
        verifiers: vec![Verifier {
          id: VerifierId("V1".into()),
          material: VerifierMaterial::Candidate,
        }],
        evidence: EvidenceRequirementV1 {
          control: EvidenceControlRequirementV1::CandidateControlledPermitted,
          assurance: AssuranceRequirementV1::LocalOrStronger,
        },
      }],
    }],
  };
  let contract_path = root.join("contract.json");
  fs::write(
    &contract_path,
    serde_json::to_vec_pretty(&contract).expect("contract"),
  )
  .expect("write contract");

  // PROPOSAL and RECONCILIATION need no grant; the producer may legitimately
  // reach AUTHORITY_ADMISSION and must then be told to defer to the trusted
  // operator instead of probing `tenet authority grant`.
  let proposal = ok_json(
    root,
    &[
      "authority",
      "prepare",
      "--contract",
      &contract_path.to_string_lossy(),
      "--json",
    ],
  );
  ok_json(
    root,
    &[
      "authority",
      "reconcile",
      "--proposal",
      proposal["proposalId"].as_str().expect("proposal id"),
      "--json",
    ],
  );

  let status = ok_json(root, &["status", "--json"]);
  assert_eq!(status["phase"], "AUTHORITY_ADMISSION");
  let next_action = status["nextAction"].as_str().expect("next action");
  assert!(
    next_action.contains("native confirmation mechanism"),
    "next action does not route through native user approval: {next_action}"
  );
  assert!(
    next_action.contains("admit-prepared"),
    "next action does not name the trusted admission handoff: {next_action}"
  );
  assert!(
    next_action.contains("Never run tenet authority grant"),
    "next action does not ban producer-side grant minting: {next_action}"
  );
  assert!(
    next_action.contains("AdmissionGrant"),
    "next action does not state that approval is not a grant: {next_action}"
  );
}

#[test]
fn generated_skill_prescribes_the_native_approval_ux() {
  let directory = tempfile::tempdir().expect("repository");
  init_repo(directory.path());
  let skill = generated_skill(directory.path());
  for claim in [
    "native user-interaction or confirmation mechanism",
    "**Review authority**",
    "**Admit and continue**",
    "**Stop / reject**",
    "never make the user copy content IDs",
    "`tenet authority admit-prepared --json`",
    "User approval through an agent prompt is not an `AdmissionGrant`",
    "call `tenet_context` again",
    "never add `TENET_ADMISSION_SECRET` to your own environment",
  ] {
    assert!(skill.contains(claim), "skill omits {claim:?}");
  }
}
