//! Deterministic verification policy behavior.

use std::collections::BTreeSet;

use tenet_domain::policy::{
  CandidateCapturePolicy, CommandArgument, CommandCwd, PolicyError, VerificationPolicy,
  VerifierAuthority,
};

/// Hash a policy into its canonical identity digest.
pub fn policy_digest(policy: &VerificationPolicy) -> Result<String, serde_json::Error> {
  crate::digest::canonical_digest(policy)
}

/// True when the candidate capture boundary excludes a project-relative path.
pub fn candidate_excludes(candidate: &CandidateCapturePolicy, path: &str) -> bool {
  let path = path.replace('\\', "/");
  candidate_path_is_reserved(&path)
    || candidate
      .exclude
      .iter()
      .any(|rule| selector_matches(rule, &path))
}

pub fn validate_candidate_surface(candidate: &CandidateCapturePolicy) -> Result<(), PolicyError> {
  if candidate.include.is_empty() {
    return Err(PolicyError::CandidateSurfaceUnconfigured);
  }
  Ok(())
}

/// True when candidate semantics always exclude `path`, regardless of selectors.
pub fn candidate_path_is_reserved(path: &str) -> bool {
  [".tenet", ".git"].iter().any(|reserved| {
    path == *reserved
      || (path.starts_with(reserved) && path.as_bytes().get(reserved.len()) == Some(&b'/'))
  })
}

fn selector_matches(selector: &str, path: &str) -> bool {
  let selector = selector.replace('\\', "/");
  if selector == "**" {
    return true;
  }
  selector
    .strip_suffix("/**")
    .map_or(selector == path, |prefix| {
      path == prefix
        || (path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'/'))
    })
}
fn valid_candidate_selector(value: &str) -> bool {
  if value == "**" {
    return true;
  }
  let base = value.strip_suffix("/**").unwrap_or(value);
  !(base.is_empty()
    || base == "."
    || base.contains('*')
    || base.contains("//")
    || base.starts_with("./")
    || base.contains("/./")
    || base.ends_with("/.")
    || base.ends_with('/')
    || candidate_path_is_reserved(base)
    || !is_safe_relative_path(base))
}

fn validate_candidate_selectors(selectors: &[String], inclusion: bool) -> Result<(), PolicyError> {
  if let Some(selector) = selectors
    .iter()
    .find(|selector| !valid_candidate_selector(selector))
  {
    return Err(if inclusion {
      PolicyError::InvalidCandidateInclusion(selector.clone())
    } else {
      PolicyError::InvalidCandidateExclusion(selector.clone())
    });
  }
  Ok(())
}

pub fn validate_policy(policy: &VerificationPolicy) -> Result<(), PolicyError> {
  if policy.version != 1 {
    return Err(PolicyError::UnsupportedVersion(policy.version));
  }
  if !is_safe_relative_path(&policy.spec_path) {
    return Err(PolicyError::InvalidSpecPath);
  }
  if !is_safe_relative_path(&policy.candidate.root)
    || candidate_path_is_reserved(&policy.candidate.root)
  {
    return Err(PolicyError::InvalidCandidateRoot);
  }
  validate_candidate_selectors(&policy.candidate.include, true)?;
  validate_candidate_selectors(&policy.candidate.exclude, false)?;
  let mut ids = BTreeSet::new();
  for verifier in &policy.verifiers {
    if verifier.id.trim().is_empty() {
      return Err(PolicyError::BlankVerifierId);
    }
    if !ids.insert(verifier.id.as_str()) {
      return Err(PolicyError::DuplicateVerifier(verifier.id.clone()));
    }
    if verifier
      .command
      .argv
      .first()
      .is_none_or(|argument| argument.value().is_empty())
      || verifier
        .command
        .argv
        .iter()
        .any(|argument| argument.value().contains('\0'))
    {
      return Err(PolicyError::EmptyArgv(verifier.id.clone()));
    }
    if verifier.command.argv.iter().any(|argument| {
      !matches!(argument, CommandArgument::Literal(_)) && !is_safe_relative_path(argument.value())
    }) {
      return Err(PolicyError::InvalidCommandPath(verifier.id.clone()));
    }
    if matches!(
      verifier.command.argv.first(),
      Some(CommandArgument::Literal(program))
        if std::path::Path::new(program).components().count() != 1
          || program == "."
    ) {
      return Err(PolicyError::InvalidExecutable(verifier.id.clone()));
    }
    let valid_cwd = match &verifier.command.cwd {
      CommandCwd::Candidate(path) | CommandCwd::Authority(path) => is_safe_relative_path(path),
      CommandCwd::Scratch => true,
    };
    if !valid_cwd {
      return Err(PolicyError::InvalidCwd(verifier.id.clone()));
    }
    if verifier.command.timeout_ms == 0 {
      return Err(PolicyError::InvalidTimeout(verifier.id.clone()));
    }
    if verifier.max_output_bytes == 0 {
      return Err(PolicyError::InvalidOutputLimit(verifier.id.clone()));
    }
    let result = &verifier.command.result;
    if !result.pass.is_disjoint(&result.fail)
      || !result.pass.is_disjoint(&result.inconclusive)
      || !result.fail.is_disjoint(&result.inconclusive)
    {
      return Err(PolicyError::OverlappingExitCodes(verifier.id.clone()));
    }
    let mut inherited = BTreeSet::new();
    for name in &verifier.command.env.inherit {
      if !valid_environment_name(name) {
        return Err(PolicyError::InvalidEnvironmentName(verifier.id.clone()));
      }
      if is_reserved_environment_name(name) {
        return Err(PolicyError::ReservedEnvironmentName(verifier.id.clone()));
      }
      if !inherited.insert(name) {
        return Err(PolicyError::InvalidEnvironmentName(verifier.id.clone()));
      }
    }
    for name in verifier.command.env.set.keys() {
      if !valid_environment_name(name) {
        return Err(PolicyError::InvalidEnvironmentName(verifier.id.clone()));
      }
      if is_reserved_environment_name(name) {
        return Err(PolicyError::ReservedEnvironmentName(verifier.id.clone()));
      }
    }
    match verifier.authority {
      VerifierAuthority::Project if verifier.oracle_path.is_some() => {
        return Err(PolicyError::UnexpectedOraclePath(verifier.id.clone()));
      }
      VerifierAuthority::Project => {}
      VerifierAuthority::AuthoritySnapshot => {
        let valid_oracle_path = verifier.oracle_path.as_deref().is_some_and(|path| {
          path != "."
            && path != ".tenet"
            && !path.starts_with(".tenet/store")
            && path != ".tenet/tmp"
            && !path.starts_with(".tenet/tmp/")
            && is_safe_relative_path(path)
        });
        if !valid_oracle_path {
          return Err(PolicyError::InvalidOraclePath(verifier.id.clone()));
        }
        let Some(CommandArgument::AuthorityPath(executable)) = verifier.command.argv.first() else {
          return Err(PolicyError::InvalidExecutable(verifier.id.clone()));
        };
        let oracle_path = verifier.oracle_path.as_deref().unwrap_or_default();
        if executable != oracle_path
          && !(executable.starts_with(oracle_path)
            && executable.as_bytes().get(oracle_path.len()) == Some(&b'/'))
        {
          return Err(PolicyError::InvalidExecutable(verifier.id.clone()));
        }
      }
    }
  }
  Ok(())
}

fn is_safe_relative_path(value: &str) -> bool {
  if value == "." {
    return true;
  }
  let path = std::path::Path::new(value);
  !value.trim().is_empty()
    && !value.contains('\\')
    && !value.contains("//")
    && !value.starts_with("./")
    && !value.contains("/./")
    && !value.ends_with("/.")
    && !value.ends_with('/')
    && !path.is_absolute()
    && path
      .components()
      .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn valid_environment_name(value: &str) -> bool {
  let mut bytes = value.bytes();
  bytes
    .next()
    .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
    && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn is_reserved_environment_name(value: &str) -> bool {
  matches!(
    value,
    "TENET_AUTHORITY_ID"
      | "TENET_CANDIDATE_ID"
      | "TENET_CANDIDATE_ROOT"
      | "TENET_AUTHORITY_ROOT"
      | "TENET_SCRATCH_ROOT"
      | "TENET_OUTPUT_ROOT"
      | "TMPDIR"
  )
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeSet;

  use tenet_domain::policy::{
    CandidateCapturePolicy, CommandArgument, CommandCwd, CommandSpec, ExitCodePolicy,
    ProjectConfig, VerifierAuthority, VerifierSpec,
  };

  use super::validate_policy;

  fn policy(argument: CommandArgument) -> ProjectConfig {
    ProjectConfig {
      version: 1,
      spec_path: "SPEC.md".into(),
      candidate: CandidateCapturePolicy {
        include: vec!["**".into()],
        ..Default::default()
      },
      verifiers: vec![VerifierSpec {
        id: "V1".into(),
        command: CommandSpec {
          argv: vec![argument],
          cwd: CommandCwd::Candidate(".".into()),
          env: Default::default(),
          timeout_ms: 1_000,
          result: ExitCodePolicy::default(),
        },
        max_output_bytes: 1_024,
        authority: VerifierAuthority::Project,
        oracle_path: None,
        protection: tenet_domain::policy::VerifierProtection::default(),
      }],
    }
  }

  #[test]
  fn candidate_path_escape_is_rejected() {
    assert!(validate_policy(&policy(CommandArgument::CandidatePath("../outside".into()))).is_err());
  }

  #[test]
  fn authority_path_escape_is_rejected() {
    assert!(validate_policy(&policy(CommandArgument::AuthorityPath("../outside".into()))).is_err());
  }

  #[test]
  fn overlapping_exit_codes_are_rejected() {
    let mut policy = policy(CommandArgument::CandidatePath("verify".into()));
    policy.verifiers[0].command.result.fail = BTreeSet::from([0]);
    assert!(validate_policy(&policy).is_err());
  }

  #[test]
  fn repository_control_paths_cannot_be_selected() {
    let mut policy = policy(CommandArgument::CandidatePath("verify".into()));
    policy.candidate.include = vec![".git/**".into()];
    assert!(validate_policy(&policy).is_err());
  }

  #[test]
  fn temporary_state_cannot_be_an_oracle_bundle() {
    let mut policy = policy(CommandArgument::AuthorityPath(".tenet/tmp/verify".into()));
    policy.verifiers[0].authority = VerifierAuthority::AuthoritySnapshot;
    policy.verifiers[0].oracle_path = Some(".tenet/tmp".into());
    assert!(validate_policy(&policy).is_err());
  }

  #[test]
  fn empty_non_program_literal_is_preserved() {
    let mut policy = policy(CommandArgument::Literal("program".into()));
    policy.verifiers[0]
      .command
      .argv
      .push(CommandArgument::Literal(String::new()));
    assert!(validate_policy(&policy).is_ok());
  }

  #[test]
  fn literal_program_paths_are_rejected() {
    assert!(validate_policy(&policy(CommandArgument::Literal("../outside".into()))).is_err());
  }
}
