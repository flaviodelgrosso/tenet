use std::{
  collections::BTreeMap,
  io::Read,
  path::Path,
  process::{Command, Stdio},
  thread,
  time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde::Serialize;
use tenet_domain::{
  digest::canonical_digest,
  evidence::{
    ExecutionEnvironmentIdentity, ExecutionProvenance, OracleIdentity, RunnerIdentity,
    VerifierObservation,
  },
  policy::{EnvironmentMode, VerifierAuthority, VerifierSpec},
};

pub struct ExecutedVerifier {
  pub observation: VerifierObservation,
  pub execution: ExecutionProvenance,
}

pub fn run_verifier(
  candidate_checkout: &Path,
  oracle_bundle: Option<&Path>,
  verifier: &VerifierSpec,
  authority_revision: &str,
  revision: &str,
  oracle_identity: &OracleIdentity,
) -> Result<ExecutedVerifier> {
  let configured_executable = verifier.argv.first().context("verifier argv is empty")?;
  let candidate_checkout = candidate_checkout
    .canonicalize()
    .context("canonicalize candidate checkout")?;
  let (execution_root, executable) = match verifier.authority {
    VerifierAuthority::Project => (candidate_checkout.clone(), configured_executable.into()),
    VerifierAuthority::AuthoritySnapshot => {
      let bundle = oracle_bundle
        .context("authority_snapshot verifier has no materialized oracle bundle")?
        .canonicalize()
        .context("canonicalize authority oracle bundle")?;
      let executable = bundle
        .join(configured_executable)
        .canonicalize()
        .context("resolve authority oracle executable")?;
      if !executable.starts_with(&bundle) || !executable.is_file() {
        anyhow::bail!("authority oracle executable must be a file inside its bundle");
      }
      (bundle, executable)
    }
  };
  let cwd = execution_root.join(&verifier.cwd);
  let mut command = Command::new(&executable);
  if verifier.environment_mode == EnvironmentMode::Declared {
    command.env_clear();
  }
  command
    .args(&verifier.argv[1..])
    .envs(&verifier.env)
    .env("TENET_AUTHORITY_REVISION", authority_revision)
    .env("TENET_CANDIDATE_REVISION", revision)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  if verifier.authority == VerifierAuthority::AuthoritySnapshot {
    command.env("TENET_CANDIDATE_ROOT", &candidate_checkout);
  }
  let execution = execution_provenance(verifier, authority_revision, revision, oracle_identity)?;
  #[cfg(unix)]
  let cwd_handle = open_verifier_cwd(&execution_root, &verifier.cwd)?;
  #[cfg(unix)]
  {
    use std::os::{fd::AsRawFd, unix::process::CommandExt};
    let cwd_fd = cwd_handle.as_raw_fd();
    // SAFETY: `fchdir` and `setpgid` are async-signal-safe. `cwd_handle` stays open until spawn
    // returns, and the descriptor names a directory reached without following symlinks.
    unsafe {
      command.pre_exec(move || {
        if libc::fchdir(cwd_fd) != 0 {
          return Err(std::io::Error::last_os_error());
        }
        if libc::setpgid(0, 0) != 0 {
          return Err(std::io::Error::last_os_error());
        }
        Ok(())
      });
    }
  }
  #[cfg(not(unix))]
  {
    let cwd = cwd
      .canonicalize()
      .with_context(|| format!("resolve verifier working directory `{}`", verifier.cwd))?;
    if !cwd.starts_with(&execution_root) {
      anyhow::bail!(
        "verifier working directory `{}` escapes its execution root",
        verifier.cwd
      );
    }
    command.current_dir(cwd);
  }
  let mut child = command
    .spawn()
    .with_context(|| format!("start verifier `{}` in {}", verifier.id, cwd.display()))?;
  let stdout = child.stdout.take().context("capture verifier stdout")?;
  let stderr = child.stderr.take().context("capture verifier stderr")?;
  let limit = verifier.max_output_bytes;
  let stdout_reader = thread::spawn(move || read_bounded(stdout, limit));
  let stderr_reader = thread::spawn(move || read_bounded(stderr, limit));

  let deadline = Instant::now()
    .checked_add(Duration::from_secs(verifier.timeout_seconds))
    .context("verifier timeout exceeds the platform clock range")?;
  let (status, timed_out) = loop {
    if let Some(status) = child.try_wait().context("wait for verifier")? {
      break (status, false);
    }
    if Instant::now() >= deadline {
      terminate(&mut child).context("terminate timed-out verifier")?;
      break (child.wait().context("reap timed-out verifier")?, true);
    }
    thread::sleep(Duration::from_millis(10));
  };

  let stdout = stdout_reader
    .join()
    .map_err(|_| anyhow::anyhow!("verifier stdout reader panicked"))??;
  let stderr = stderr_reader
    .join()
    .map_err(|_| anyhow::anyhow!("verifier stderr reader panicked"))??;
  Ok(ExecutedVerifier {
    observation: VerifierObservation {
      exit_code: status.code(),
      stdout,
      stderr,
      timed_out,
    },
    execution,
  })
}
const RUNNER_IDENTITY: &str = "tenet.local_process_runner.v1";
const TENET_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunnerAttributes<'a> {
  identity: &'a str,
  tenet_version: &'a str,
  os: &'a str,
  architecture: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionEnvironmentInputs<'a> {
  schema_version: u32,
  runner: RunnerAttributes<'a>,
  environment_mode: EnvironmentMode,
  configured_environment: &'a BTreeMap<String, String>,
  verifier_id: &'a str,
  argv: &'a [String],
  cwd: &'a str,
  authority: VerifierAuthority,
  oracle_identity: &'a OracleIdentity,
  tenet_inputs: BTreeMap<&'static str, &'a str>,
}

fn execution_provenance(
  verifier: &VerifierSpec,
  authority_revision: &str,
  revision: &str,
  oracle_identity: &OracleIdentity,
) -> Result<ExecutionProvenance> {
  let runner = RunnerAttributes {
    identity: RUNNER_IDENTITY,
    tenet_version: TENET_VERSION,
    os: std::env::consts::OS,
    architecture: std::env::consts::ARCH,
  };
  let identity = execution_environment_identity(
    verifier,
    authority_revision,
    revision,
    oracle_identity,
    runner,
  )?;
  Ok(ExecutionProvenance {
    runner_identity: RunnerIdentity(runner.identity.into()),
    tenet_version: runner.tenet_version.into(),
    os: runner.os.into(),
    architecture: runner.architecture.into(),
    environment_mode: verifier.environment_mode,
    execution_environment_identity: ExecutionEnvironmentIdentity(identity),
  })
}

fn execution_environment_identity(
  verifier: &VerifierSpec,
  authority_revision: &str,
  revision: &str,
  oracle_identity: &OracleIdentity,
  runner: RunnerAttributes<'_>,
) -> Result<String> {
  let mut tenet_inputs = BTreeMap::from([
    ("TENET_AUTHORITY_REVISION", authority_revision),
    ("TENET_CANDIDATE_REVISION", revision),
  ]);
  if verifier.authority == VerifierAuthority::AuthoritySnapshot {
    // The temporary path varies by run; its candidate-tree content is deterministically bound to R.
    tenet_inputs.insert("TENET_CANDIDATE_ROOT_CONTENT_REVISION", revision);
  }
  canonical_digest(&ExecutionEnvironmentInputs {
    schema_version: 1,
    runner,
    environment_mode: verifier.environment_mode,
    configured_environment: &verifier.env,
    verifier_id: &verifier.id,
    argv: &verifier.argv,
    cwd: &verifier.cwd,
    authority: verifier.authority,
    oracle_identity,
    tenet_inputs,
  })
  .context("derive execution environment identity")
}

#[cfg(unix)]
fn open_verifier_cwd(checkout: &Path, relative: &str) -> Result<std::fs::File> {
  use std::{
    ffi::CString,
    os::{fd::FromRawFd, unix::ffi::OsStrExt},
    path::Component,
  };

  let mut directory = std::fs::File::open(checkout).context("open candidate checkout")?;
  for component in Path::new(relative).components() {
    let Component::Normal(name) = component else {
      if component == Component::CurDir {
        continue;
      }
      anyhow::bail!("verifier working directory must remain relative to the candidate checkout");
    };
    let name = CString::new(name.as_bytes()).context("verifier working directory contains NUL")?;
    // SAFETY: `directory` is an open directory descriptor, `name` is NUL-terminated, and the
    // returned descriptor is immediately owned by `File` when non-negative.
    let descriptor = unsafe {
      libc::openat(
        std::os::fd::AsRawFd::as_raw_fd(&directory),
        name.as_ptr(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
      )
    };
    if descriptor < 0 {
      return Err(std::io::Error::last_os_error()).with_context(|| {
        format!(
          "securely open verifier working directory component {} inside candidate checkout",
          name.to_string_lossy()
        )
      });
    }
    // SAFETY: `descriptor` is a fresh owned descriptor returned by successful `openat`.
    directory = unsafe { std::fs::File::from_raw_fd(descriptor) };
  }
  Ok(directory)
}

fn terminate(child: &mut std::process::Child) -> std::io::Result<()> {
  #[cfg(unix)]
  {
    let process_group = -(child.id() as i32);
    // SAFETY: The child was placed in its own process group before spawn. `kill` receives a
    // valid negative process-group identifier and does not dereference memory.
    let result = unsafe { libc::kill(process_group, libc::SIGKILL) };
    if result == 0 {
      return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
      return Ok(());
    }
    Err(error)
  }
  #[cfg(not(unix))]
  child.kill()
}

fn read_bounded(mut reader: impl Read, limit: usize) -> Result<String> {
  let mut kept = Vec::with_capacity(limit.min(8192));
  let mut buffer = [0_u8; 8192];
  loop {
    let count = reader.read(&mut buffer)?;
    if count == 0 {
      break;
    }
    let remaining = limit.saturating_sub(kept.len());
    kept.extend_from_slice(&buffer[..count.min(remaining)]);
  }
  Ok(String::from_utf8_lossy(&kept).into_owned())
}

#[cfg(test)]
mod tests {
  use std::collections::{BTreeMap, BTreeSet};

  use tenet_domain::{
    evidence::OracleIdentity,
    policy::{EnvironmentMode, VerifierAuthority, VerifierSpec},
  };

  use super::{RunnerAttributes, execution_environment_identity};

  fn fixture() -> (VerifierSpec, OracleIdentity) {
    let verifier = VerifierSpec {
      id: "quality".into(),
      argv: vec!["/usr/bin/true".into()],
      cwd: ".".into(),
      timeout_seconds: 1,
      max_output_bytes: 1024,
      env: BTreeMap::from([("PROFILE".into(), "release".into())]),
      environment_mode: EnvironmentMode::Declared,
      authority: VerifierAuthority::Project,
      oracle_path: None,
    };
    let oracle = OracleIdentity::Project {
      verifier_id: "quality".into(),
      candidate_revision: "candidate".into(),
      definition_digest: "sha256:definition".into(),
    };
    (verifier, oracle)
  }

  #[test]
  fn execution_identity_is_stable_for_identical_known_inputs() {
    let (verifier, oracle) = fixture();
    let runner = RunnerAttributes {
      identity: "runner",
      tenet_version: "1.0.0",
      os: "os",
      architecture: "arch",
    };
    let first =
      execution_environment_identity(&verifier, "authority", "candidate", &oracle, runner).unwrap();
    let second =
      execution_environment_identity(&verifier, "authority", "candidate", &oracle, runner).unwrap();
    assert_eq!(first, second);
  }

  #[test]
  fn execution_identity_binds_every_recorded_runner_attribute() {
    let (verifier, oracle) = fixture();
    let runners = [
      RunnerAttributes {
        identity: "runner",
        tenet_version: "1.0.0",
        os: "os",
        architecture: "arch",
      },
      RunnerAttributes {
        identity: "other-runner",
        tenet_version: "1.0.0",
        os: "os",
        architecture: "arch",
      },
      RunnerAttributes {
        identity: "runner",
        tenet_version: "2.0.0",
        os: "os",
        architecture: "arch",
      },
      RunnerAttributes {
        identity: "runner",
        tenet_version: "1.0.0",
        os: "other-os",
        architecture: "arch",
      },
      RunnerAttributes {
        identity: "runner",
        tenet_version: "1.0.0",
        os: "os",
        architecture: "other-arch",
      },
    ];
    let identities = runners
      .into_iter()
      .map(|runner| {
        execution_environment_identity(&verifier, "authority", "candidate", &oracle, runner)
          .unwrap()
      })
      .collect::<BTreeSet<_>>();
    assert_eq!(identities.len(), runners.len());
  }
}
