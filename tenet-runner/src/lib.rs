//! Concrete verifier process execution for Tenet: spawning, timeouts,
//! environment construction, bounded stdout/stderr capture, and termination
//! observation. Implements the [`VerifierRunner`] port from
//! [`tenet_application`].

use std::{
  collections::BTreeMap,
  ffi::{OsStr, OsString},
  fs,
  io::Read,
  path::{Component, Path, PathBuf},
  process::{Command, Stdio},
  sync::mpsc,
  thread,
  time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tenet_application::ports::{ExecutedVerifier, VerifierRun, VerifierRunner};
use tenet_domain::{
  algebra::{
    AssuranceProfileId, EvidenceResult, ExecutionContext, LOCAL_V1, PROTECTED_V1,
    PlatformInformation, RUNNER_SEMANTICS_V1, RunnerSemanticsId,
  },
  evidence::{
    ExecutionEnvironmentIdentity, ExecutionProvenance, OracleIdentity, RunnerIdentity,
    VerifierObservation,
  },
  policy::{CommandArgument, CommandCwd, VerifierProtection},
};
use tenet_kernel::digest::canonical_digest;

/// Private namespace paths inside the Bubblewrap sandbox. The tmpfs-backed
/// view is invisible to processes outside the namespace, so the digest-
/// verified copy the verifier reads cannot be transiently mutated. The whole
/// filesystem is read-bound first, so bwrap cannot create mount points under
/// the root; the view and expectation tmpfses therefore live under `/mnt`,
/// an existing directory covered by a fresh writable tmpfs so its subpaths
/// can be created.
const PRIVATE_VIEW_ROOT: &str = "/mnt/tenet-view";
const PRIVATE_CHECKS_ROOT: &str = "/mnt/tenet-private";
const PRIVATE_CHECKS_PATH: &str = "/mnt/tenet-private/checks";
const PRIVATE_DIRECTORIES_PATH: &str = "/mnt/tenet-private/directories";
const PRIVATE_MODES_PATH: &str = "/mnt/tenet-private/modes";
/// Exit code used when the in-namespace digest verification fails; it is not
/// part of any admitted exit-code policy, so the run yields infrastructure.
const PRIVATE_VIEW_CHECK_FAILED_EXIT: &str = "70";

/// Local process runner. Structured argv is passed directly to the operating
/// system launcher; no host shell is ever interpreted.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalProcessRunner;

impl VerifierRunner for LocalProcessRunner {
  fn run(&self, request: &VerifierRun<'_>) -> Result<ExecutedVerifier> {
    let environment = configured_environment(request);
    let working_directory = resolve_cwd(request)?;
    let argv = request
      .verifier
      .command
      .argv
      .iter()
      .map(|argument| resolve_argument(argument, request))
      .collect::<Result<Vec<_>>>()?;
    let configured_program = argv.first().context("verifier argv is empty")?;
    let resolved_program = resolve_program(
      request
        .verifier
        .command
        .argv
        .first()
        .context("verifier argv is empty")?,
      configured_program,
      &working_directory,
      &environment,
    );
    let resolved_program_digest = resolved_program
      .as_deref()
      .and_then(|path| file_digest(path).ok());

    // Protected verification is fail-closed: when the platform cannot
    // enforce read-only Candidate/Authority views with separate writable
    // scratch and controlled output, the runner returns an explicit
    // infrastructure result instead of downgrading to `LOCAL_V1`.
    let backend = match request.verifier.protection {
      VerifierProtection::Local => None,
      VerifierProtection::Protected => match ProtectionBackend::detect() {
        Some(backend) => Some(backend),
        None => {
          let context = execution_context(None, None, LOCAL_V1);
          let execution = provenance(request, &context, &environment, "local")?;
          return Ok(infrastructure_result(
            context,
            execution,
            format!(
              "protected verification is unsupported on this platform ({} {}); Tenet refuses to downgrade assurance",
              std::env::consts::OS,
              std::env::consts::ARCH
            ),
          ));
        }
      },
    };
    let protection_identity = backend.map(ProtectionBackend::identity).unwrap_or("local");
    let assurance = if backend.is_some() {
      PROTECTED_V1
    } else {
      LOCAL_V1
    };
    let context = execution_context(
      resolved_program.as_deref(),
      resolved_program_digest,
      assurance,
    );
    let execution = provenance(request, &context, &environment, protection_identity)?;

    let Some(program) = resolved_program else {
      return Ok(infrastructure_result(
        context,
        execution,
        format!(
          "cannot resolve verifier program `{}`",
          configured_program.to_string_lossy()
        ),
      ));
    };
    // A same-user hostile producer can transiently mutate materialized view
    // files on the host while a run is in flight, so Bubblewrap protected
    // execution must not read the host roots directly: the private copy is
    // built inside a fresh namespace's tmpfs from a trusted expectation list
    // delivered through descriptors no other process can reach, and the
    // verifier is exec'd only after every byte matched inside that namespace.
    let mut private_inputs: Option<PrivateInputs> = None;
    let mut command = match backend {
      None => {
        let mut command = Command::new(&program);
        command.args(&argv[1..]);
        command
      }
      Some(ProtectionBackend::Seatbelt) => {
        // The workspace stages protected views on read-only volumes whose
        // backing store is unlinked, so the roots handed to this runner are
        // already immutable; the profile confines the verifier itself.
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command
          .arg("-p")
          .arg(seatbelt_profile(request))
          .arg(&program)
          .args(&argv[1..]);
        command
      }
      Some(ProtectionBackend::Bubblewrap) => {
        let inputs = PrivateInputs::open(request)?;
        let script = private_view_script(request, &program, &argv, &working_directory)?;
        private_inputs = Some(inputs);
        bubblewrap_command(request, &script, private_inputs.as_ref().expect("inputs"))
      }
    };
    let (candidate_root_env, authority_root_env) = if private_inputs.is_some() {
      (
        PathBuf::from(PRIVATE_VIEW_ROOT).join("candidate"),
        PathBuf::from(PRIVATE_VIEW_ROOT).join("authority"),
      )
    } else {
      (
        request.candidate_root.to_path_buf(),
        request.authority_root.to_path_buf(),
      )
    };
    command
      .env_clear()
      .envs(&environment)
      .env("TENET_AUTHORITY_ID", &request.authority_id.0.0)
      .env("TENET_CANDIDATE_ID", &request.candidate_id.0.0)
      .env("TENET_CANDIDATE_ROOT", &candidate_root_env)
      .env("TENET_AUTHORITY_ROOT", &authority_root_env)
      .env("TENET_SCRATCH_ROOT", request.scratch_root)
      .env("TENET_OUTPUT_ROOT", request.output_root);
    if backend.is_some() {
      // Runtime temp space must land inside the only writable regions.
      command.env("TMPDIR", request.scratch_root);
    }
    command
      .current_dir(&working_directory)
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped());
    #[cfg(unix)]
    {
      use std::os::unix::process::CommandExt;
      // SAFETY: `setpgid` is async-signal-safe and touches no Rust-managed memory.
      unsafe {
        command.pre_exec(|| {
          if libc::setpgid(0, 0) != 0 {
            return Err(std::io::Error::last_os_error());
          }
          Ok(())
        });
      }
    }
    let mut child = match command.spawn() {
      Ok(child) => child,
      Err(error) => {
        drop(private_inputs);
        return Ok(infrastructure_result(
          context,
          execution,
          format!("start verifier `{}`: {error}", request.verifier.id),
        ));
      }
    };
    // The private expectation descriptors are inherited by exactly this
    // spawn; nothing further in this process may hold them open.
    drop(private_inputs);
    let stdout = child.stdout.take().context("capture verifier stdout")?;
    let stderr = child.stderr.take().context("capture verifier stderr")?;
    let limit = request.verifier.max_output_bytes;
    let (stdout_sender, stdout_receiver) = mpsc::channel();
    let (stderr_sender, stderr_receiver) = mpsc::channel();
    thread::spawn(move || {
      let _ = stdout_sender.send(read_bounded(stdout, limit));
    });
    thread::spawn(move || {
      let _ = stderr_sender.send(read_bounded(stderr, limit));
    });

    let deadline = Instant::now()
      .checked_add(Duration::from_millis(request.verifier.command.timeout_ms))
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
    #[cfg(unix)]
    terminate(&mut child).context("terminate verifier descendants")?;
    drop(child);

    let drain_deadline = Instant::now() + Duration::from_secs(5);
    let stdout = collect_output(&stdout_receiver, drain_deadline)?;
    let stderr = collect_output(&stderr_receiver, drain_deadline)?;
    let exit_code = status.code();
    let result = request
      .verifier
      .command
      .result
      .interpret(exit_code, timed_out, false);
    let infrastructure_error = match (timed_out, exit_code, result) {
      (true, _, _) => Some("verifier timed out".into()),
      (false, None, _) => Some("verifier terminated by signal".into()),
      (false, Some(code), EvidenceResult::InfrastructureError) => {
        Some(format!("verifier returned unrecognized exit code {code}"))
      }
      _ => None,
    };
    Ok(ExecutedVerifier {
      observation: VerifierObservation {
        exit_code,
        stdout,
        stderr,
        timed_out,
      },
      result,
      infrastructure_error,
      context,
      execution,
    })
  }
}

/// Standard OS enforcement primitives for protected verification. Detection
/// is runtime and honest: an unavailable backend yields an explicit
/// infrastructure result, never a silent downgrade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProtectionBackend {
  /// macOS Seatbelt via `/usr/bin/sandbox-exec`.
  Seatbelt,
  /// Linux Bubblewrap via `bwrap` on `PATH`.
  Bubblewrap,
}

impl ProtectionBackend {
  fn detect() -> Option<Self> {
    if cfg!(target_os = "macos") {
      return Path::new("/usr/bin/sandbox-exec")
        .is_file()
        .then_some(Self::Seatbelt);
    }
    if cfg!(target_os = "linux") {
      return path_executable("bwrap").then_some(Self::Bubblewrap);
    }
    None
  }

  fn identity(self) -> &'static str {
    match self {
      Self::Seatbelt => "seatbelt",
      Self::Bubblewrap => "bubblewrap",
    }
  }
}

/// True when this platform can enforce `PROTECTED_V1` verification. Callers
/// use this to decide whether a protected verifier is runnable; the runner
/// itself fails closed with an infrastructure result when it is not.
pub fn protection_backend_available() -> bool {
  ProtectionBackend::detect().is_some()
}

fn path_executable(name: &str) -> bool {
  let Some(path) = std::env::var_os("PATH") else {
    return false;
  };
  std::env::split_paths(&path).any(|directory| directory.join(name).is_file())
}

fn seatbelt_escape(value: &str) -> String {
  value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Seatbelt matches resolved filesystem paths, so a writable subpath must be
/// canonicalized (`/var/folders` lives behind the `/var` symlink on macOS).
fn seatbelt_writable_path(path: &Path) -> String {
  let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
  seatbelt_escape(&resolved.to_string_lossy())
}

/// Seatbelt policy: everything readable and executable, all writes denied
/// except the run's private scratch and controlled output directories.
/// Sandboxes are inherited across fork/exec, so background descendants of a
/// verifier remain confined.
fn seatbelt_profile(request: &VerifierRun<'_>) -> String {
  let writable = [request.scratch_root, request.output_root]
    .iter()
    .map(|path| format!("(subpath \"{}\")", seatbelt_writable_path(path)))
    .collect::<Vec<_>>()
    .join(" ");
  format!(
    "(version 1)(allow default)(deny file-write*)(allow file-write* (literal \"/dev/null\") {writable})"
  )
}

/// Kernel-private files holding the trusted expectations for the private
/// view (file digests in `sha256sum -c` format, expected directories, and
/// expected modes). They are delivered to the sandbox through descriptors
/// inherited by exactly this spawn; because no path names their contents, an
/// external process cannot rewrite the expectations while the run proceeds.
struct PrivateInputs {
  checks: fs::File,
  directories: fs::File,
  modes: fs::File,
}

impl PrivateInputs {
  fn open(request: &VerifierRun<'_>) -> Result<Self> {
    let mut checks = Vec::new();
    let mut modes = Vec::new();
    for digest in request.view_digests {
      if digest.path.is_empty()
        || digest.path.contains(['\n', '\r', '\t'])
        || digest.sha256_hex.len() != 64
        || !digest
          .sha256_hex
          .bytes()
          .all(|byte| byte.is_ascii_hexdigit())
      {
        bail!("protected view expectation cannot be expressed in the verification format");
      }
      let view_path = format!("{PRIVATE_VIEW_ROOT}/{}", digest.path);
      checks.extend_from_slice(digest.sha256_hex.as_bytes());
      checks.extend_from_slice(b"  ");
      checks.extend_from_slice(view_path.as_bytes());
      checks.push(b'\n');
      modes.extend_from_slice(view_path.as_bytes());
      modes.push(b'\t');
      modes.extend_from_slice(if digest.executable { b"755" } else { b"644" });
      modes.push(b'\n');
    }
    let mut directories = Vec::new();
    for directory in request.view_directories {
      if directory.is_empty() || directory.contains(['\n', '\r', '\t']) {
        bail!("protected view directory expectation cannot be expressed");
      }
      directories.extend_from_slice(format!("{PRIVATE_VIEW_ROOT}/{directory}\n").as_bytes());
    }
    Ok(Self {
      checks: private_file(&checks)?,
      directories: private_file(&directories)?,
      modes: private_file(&modes)?,
    })
  }
}

/// Create a seekable private file whose only handle is the returned
/// descriptor; the file is not reachable by any path.
fn private_file(contents: &[u8]) -> Result<fs::File> {
  #[cfg(target_os = "linux")]
  {
    use std::{
      io::{Seek, SeekFrom, Write},
      os::fd::FromRawFd,
    };
    let name = std::ffi::CString::new("tenet-private-input").expect("static name");
    // SAFETY: `name` is a valid NUL-terminated string; `memfd_create` only
    // allocates a descriptor. Without `MFD_CLOEXEC` the descriptor is
    // inherited by the spawned `bwrap` under the same number.
    let descriptor = unsafe { libc::memfd_create(name.as_ptr(), 0) };
    if descriptor < 0 {
      return Err(std::io::Error::last_os_error().into());
    }
    let mut file = unsafe {
      // SAFETY: this `File` is the sole owner of the fresh descriptor.
      fs::File::from_raw_fd(descriptor)
    };
    file.write_all(contents)?;
    file.flush()?;
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
  }
  #[cfg(not(target_os = "linux"))]
  {
    let _ = contents;
    bail!("the bubblewrap protected backend requires Linux")
  }
}

/// Bubblewrap invocation: the whole filesystem is read-bound, only the run's
/// scratch and output directories are writable, the process runs in fresh
/// namespaces, and the verifier is started through a prelude that builds a
/// private digest-verified view before exec.
fn bubblewrap_command(request: &VerifierRun<'_>, script: &str, inputs: &PrivateInputs) -> Command {
  use std::os::fd::AsRawFd;
  let mut command = Command::new("bwrap");
  command
    .arg("--ro-bind")
    .arg("/")
    .arg("/")
    .arg("--dev")
    .arg("/dev")
    .arg("--proc")
    .arg("/proc")
    .arg("--tmpfs")
    .arg("/mnt")
    .arg("--tmpfs")
    .arg(PRIVATE_VIEW_ROOT)
    .arg("--tmpfs")
    .arg(PRIVATE_CHECKS_ROOT)
    .arg("--file")
    .arg(inputs.checks.as_raw_fd().to_string())
    .arg(PRIVATE_CHECKS_PATH)
    .arg("--file")
    .arg(inputs.directories.as_raw_fd().to_string())
    .arg(PRIVATE_DIRECTORIES_PATH)
    .arg("--file")
    .arg(inputs.modes.as_raw_fd().to_string())
    .arg(PRIVATE_MODES_PATH)
    .arg("--bind")
    .arg(request.scratch_root)
    .arg(request.scratch_root)
    .arg("--bind")
    .arg(request.output_root)
    .arg(request.output_root)
    .arg("--unshare-all")
    .arg("--die-with-parent")
    .arg("--chdir")
    .arg(PRIVATE_VIEW_ROOT)
    .arg("--")
    .arg("/bin/sh")
    .arg("-c")
    .arg(script);
  command
}

/// Map a host path inside the protected view to its private-namespace path.
fn private_view_path(path: &Path, request: &VerifierRun<'_>) -> Option<PathBuf> {
  for (root, bucket) in [
    (request.candidate_root, "candidate"),
    (request.authority_root, "authority"),
  ] {
    if let Ok(relative) = path.strip_prefix(root) {
      let base = Path::new(PRIVATE_VIEW_ROOT).join(bucket);
      return Some(if relative.as_os_str().is_empty() {
        base
      } else {
        base.join(relative)
      });
    }
  }
  None
}

fn shell_quote(value: &str) -> String {
  let mut quoted = String::from('\'');
  for character in value.chars() {
    if character == '\'' {
      quoted.push_str("'\\''");
    } else {
      quoted.push(character);
    }
  }
  quoted.push('\'');
  quoted
}

fn utf8_path(path: &Path, what: &str) -> Result<String> {
  Ok(
    path
      .to_str()
      .with_context(|| format!("{what} is not valid UTF-8"))?
      .to_owned(),
  )
}

/// Build the in-namespace prelude: it materializes the private view strictly
/// from the trusted expectation list (so extra or removed host files cannot
/// enter), pins modes, verifies every byte with `sha256sum -c`, and only then
/// execs the verifier. Any mismatch exits with a code no admitted policy
/// interprets, yielding an infrastructure result.
fn private_view_script(
  request: &VerifierRun<'_>,
  program: &Path,
  argv: &[OsString],
  working_directory: &Path,
) -> Result<String> {
  let view_cwd = private_view_path(working_directory, request)
    .context("verifier working directory escapes the protected view")?;
  let view_program = private_view_path(program, request).unwrap_or_else(|| program.to_path_buf());
  let mut script = String::from("set -e\n");
  script.push_str(&format!(
    "SRC_CANDIDATE={}\nSRC_AUTHORITY={}\n",
    shell_quote(&utf8_path(request.candidate_root, "candidate root")?),
    shell_quote(&utf8_path(request.authority_root, "authority root")?),
  ));
  script.push_str(&format!(
    "mkdir -p '{root}/candidate' '{root}/authority'\n",
    root = PRIVATE_VIEW_ROOT
  ));
  script.push_str(&format!(
    "while IFS= read -r line; do p=\"${{line#*  }}\"; rel=\"${{p#{root}/}}\"; mkdir -p \"$(dirname \"$p\")\"; case \"$rel\" in candidate/*) cp -p \"$SRC_CANDIDATE/${{rel#candidate/}}\" \"$p\";; authority/*) cp -p \"$SRC_AUTHORITY/${{rel#authority/}}\" \"$p\";; *) exit {failure};; esac; done < {checks}\n",
    root = PRIVATE_VIEW_ROOT,
    failure = PRIVATE_VIEW_CHECK_FAILED_EXIT,
    checks = PRIVATE_CHECKS_PATH,
  ));
  script.push_str(&format!(
    "while IFS= read -r line; do mkdir -p \"$line\"; done < {directories}\n",
    directories = PRIVATE_DIRECTORIES_PATH
  ));
  script.push_str(&format!(
    "while IFS=\"$(printf '\\t')\" read -r path mode; do chmod \"$mode\" \"$path\"; done < {modes}\n",
    modes = PRIVATE_MODES_PATH
  ));
  script.push_str(&format!(
    "sha256sum -c {checks} >/dev/null 2>&1 || exit {failure}\n",
    checks = PRIVATE_CHECKS_PATH,
    failure = PRIVATE_VIEW_CHECK_FAILED_EXIT
  ));
  script.push_str(&format!(
    "cd {}\n",
    shell_quote(&utf8_path(&view_cwd, "view path")?)
  ));
  script.push_str(&format!(
    "exec {}",
    shell_quote(&utf8_path(&view_program, "view path")?)
  ));
  for argument in &argv[1..] {
    let view_argument = private_view_path(Path::new(argument), request)
      .map(|path| path.to_string_lossy().into_owned())
      .unwrap_or_else(|| argument.to_string_lossy().into_owned());
    script.push(' ');
    script.push_str(&shell_quote(&view_argument));
  }
  script.push('\n');
  Ok(script)
}

const RUNNER_IDENTITY: &str = "tenet.local_process_runner.v1";
const TENET_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunnerAttributes<'a> {
  identity: &'a str,
  semantics: &'a str,
  tenet_version: &'a str,
  os: &'a str,
  architecture: &'a str,
  protection: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionEnvironmentInputs<'a> {
  schema_version: u32,
  runner: RunnerAttributes<'a>,
  inherited_environment_digests: BTreeMap<String, String>,
  verifier_id: &'a str,
  command: &'a tenet_domain::policy::CommandSpec,
  oracle_identity: &'a OracleIdentity,
  tenet_inputs: BTreeMap<&'static str, &'a str>,
  resolved_program_digest: &'a Option<String>,
  program_identity: &'a Option<String>,
}

fn configured_environment(request: &VerifierRun<'_>) -> BTreeMap<OsString, OsString> {
  let mut environment = BTreeMap::new();
  for name in &request.verifier.command.env.inherit {
    if let Some(value) = std::env::var_os(name) {
      environment.insert(OsString::from(name), value);
    }
  }
  environment.extend(
    request
      .verifier
      .command
      .env
      .set
      .iter()
      .map(|(name, value)| (OsString::from(name), OsString::from(value))),
  );
  environment
}

fn inherited_environment_digests(
  names: &[String],
  environment: &BTreeMap<OsString, OsString>,
) -> BTreeMap<String, String> {
  names
    .iter()
    .filter_map(|name| {
      environment.get(OsStr::new(name)).map(|value| {
        (
          name.clone(),
          tenet_kernel::digest::bytes_digest(value.as_encoded_bytes()),
        )
      })
    })
    .collect()
}

fn resolve_cwd(request: &VerifierRun<'_>) -> Result<PathBuf> {
  let path = match &request.verifier.command.cwd {
    CommandCwd::Candidate(path) => scoped_path(request.candidate_root, path)?,
    CommandCwd::Authority(path) => scoped_path(request.authority_root, path)?,
    CommandCwd::Scratch => request.scratch_root.to_path_buf(),
  };
  if !path.is_dir() {
    bail!(
      "verifier working directory is not a directory: {}",
      path.display()
    );
  }
  Ok(path)
}

fn resolve_argument(argument: &CommandArgument, request: &VerifierRun<'_>) -> Result<OsString> {
  Ok(match argument {
    CommandArgument::Literal(value) => OsString::from(value),
    CommandArgument::CandidatePath(path) => scoped_path(request.candidate_root, path)?.into(),
    CommandArgument::AuthorityPath(path) => scoped_path(request.authority_root, path)?.into(),
    CommandArgument::ScratchPath(path) => scoped_path(request.scratch_root, path)?.into(),
  })
}

fn scoped_path(root: &Path, relative: &str) -> Result<PathBuf> {
  if relative != "."
    && (relative.is_empty()
      || Path::new(relative).is_absolute()
      || Path::new(relative)
        .components()
        .any(|component| !matches!(component, Component::Normal(_))))
  {
    bail!("typed command path escapes its root: {relative}");
  }
  Ok(if relative == "." {
    root.to_path_buf()
  } else {
    root.join(relative)
  })
}

fn resolve_program(
  argument: &CommandArgument,
  configured: &OsStr,
  cwd: &Path,
  environment: &BTreeMap<OsString, OsString>,
) -> Option<PathBuf> {
  match argument {
    CommandArgument::Literal(_) => {
      let configured = Path::new(configured);
      if configured.components().count() != 1 || configured == Path::new(".") {
        return None;
      }
      let path = environment.get(OsStr::new("PATH"))?;
      std::env::split_paths(path)
        .map(|directory| {
          if directory.is_absolute() {
            directory.join(configured)
          } else {
            cwd.join(directory).join(configured)
          }
        })
        .find(|path| path.is_file())
    }
    CommandArgument::CandidatePath(_)
    | CommandArgument::AuthorityPath(_)
    | CommandArgument::ScratchPath(_) => {
      let configured = PathBuf::from(configured);
      configured.is_file().then_some(configured)
    }
  }
}

fn execution_context(
  resolved_program: Option<&Path>,
  resolved_program_digest: Option<String>,
  assurance: &str,
) -> ExecutionContext {
  ExecutionContext {
    assurance: AssuranceProfileId(assurance.into()),
    runner_semantics: RunnerSemanticsId(RUNNER_SEMANTICS_V1.into()),
    platform: PlatformInformation {
      os: std::env::consts::OS.into(),
      architecture: std::env::consts::ARCH.into(),
    },
    resolved_program: resolved_program.map(|path| path.to_string_lossy().into_owned()),
    resolved_program_digest,
  }
}

fn provenance(
  request: &VerifierRun<'_>,
  context: &ExecutionContext,
  environment: &BTreeMap<OsString, OsString>,
  protection: &str,
) -> Result<ExecutionProvenance> {
  let runner = RunnerAttributes {
    identity: RUNNER_IDENTITY,
    semantics: RUNNER_SEMANTICS_V1,
    tenet_version: TENET_VERSION,
    os: &context.platform.os,
    architecture: &context.platform.architecture,
    protection,
  };
  let inherited_environment_digests =
    inherited_environment_digests(&request.verifier.command.env.inherit, environment);
  let program_identity = logical_program_identity(request.verifier.command.argv.first());
  let identity = execution_environment_identity(
    request,
    runner,
    inherited_environment_digests,
    &program_identity,
    &context.resolved_program_digest,
  )?;
  Ok(ExecutionProvenance {
    runner_identity: RunnerIdentity(runner.identity.into()),
    runner_semantics: context.runner_semantics.clone(),
    assurance: context.assurance.clone(),
    tenet_version: runner.tenet_version.into(),
    platform: context.platform.clone(),
    resolved_program: context.resolved_program.clone(),
    resolved_program_digest: context.resolved_program_digest.clone(),
    oracle_identity: request.oracle_identity.clone(),
    execution_environment_identity: ExecutionEnvironmentIdentity(identity),
  })
}

fn execution_environment_identity(
  request: &VerifierRun<'_>,
  runner: RunnerAttributes<'_>,
  inherited_environment_digests: BTreeMap<String, String>,
  program_identity: &Option<String>,
  resolved_program_digest: &Option<String>,
) -> Result<String> {
  let tenet_inputs = BTreeMap::from([
    ("TENET_AUTHORITY_ID", request.authority_id.0.0.as_str()),
    ("TENET_CANDIDATE_ID", request.candidate_id.0.0.as_str()),
  ]);
  canonical_digest(&ExecutionEnvironmentInputs {
    schema_version: 1,
    runner,
    inherited_environment_digests,
    verifier_id: &request.verifier.id,
    command: &request.verifier.command,
    oracle_identity: request.oracle_identity,
    tenet_inputs,
    resolved_program_digest,
    program_identity,
  })
  .context("derive execution environment identity")
}

fn logical_program_identity(argument: Option<&CommandArgument>) -> Option<String> {
  Some(match argument? {
    CommandArgument::Literal(value) => format!("literal:{value}"),
    CommandArgument::CandidatePath(path) => format!("candidate:{path}"),
    CommandArgument::AuthorityPath(path) => format!("authority:{path}"),
    CommandArgument::ScratchPath(path) => format!("scratch:{path}"),
  })
}

fn infrastructure_result(
  context: ExecutionContext,
  execution: ExecutionProvenance,
  message: String,
) -> ExecutedVerifier {
  ExecutedVerifier {
    observation: VerifierObservation {
      exit_code: None,
      stdout: String::new(),
      stderr: String::new(),
      timed_out: false,
    },
    result: EvidenceResult::InfrastructureError,
    infrastructure_error: Some(message),
    context,
    execution,
  }
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

fn file_digest(path: &Path) -> Result<String> {
  let mut file = fs::File::open(path)?;
  let mut digest = Sha256::new();
  let mut buffer = [0_u8; 8192];
  loop {
    let count = file.read(&mut buffer)?;
    if count == 0 {
      break;
    }
    digest.update(&buffer[..count]);
  }
  let bytes = digest.finalize();
  let mut encoded = String::with_capacity(7 + bytes.len() * 2);
  encoded.push_str("sha256:");
  for byte in bytes {
    use std::fmt::Write as _;
    write!(&mut encoded, "{byte:02x}").context("encode program digest")?;
  }
  Ok(encoded)
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

fn collect_output(receiver: &mpsc::Receiver<Result<String>>, deadline: Instant) -> Result<String> {
  let timeout = deadline.saturating_duration_since(Instant::now());
  match receiver.recv_timeout(timeout) {
    Ok(result) => result.map_err(|error| anyhow::anyhow!("read verifier output: {error}")),
    Err(mpsc::RecvTimeoutError::Timeout) => Err(anyhow::anyhow!(
      "verifier descendants retained an output pipe"
    )),
    Err(mpsc::RecvTimeoutError::Disconnected) => {
      Err(anyhow::anyhow!("verifier output reader panicked"))
    }
  }
}

#[cfg(test)]
mod tests {
  use std::{
    collections::BTreeSet,
    fs,
    time::{Duration, Instant},
  };

  use tenet_application::ports::{VerifierRun, VerifierRunner};
  use tenet_domain::{
    algebra::{EvidenceResult, RUNNER_SEMANTICS_V1},
    evidence::{AuthorityId, CandidateId, ContentObjectId, OracleIdentity},
    policy::{
      CommandArgument, CommandCwd, CommandSpec, EnvironmentSpec, ExitCodePolicy, VerifierAuthority,
      VerifierSpec,
    },
  };

  #[cfg(unix)]
  fn execute_script(
    script: Option<&str>,
    timeout_ms: u64,
  ) -> tenet_application::ports::ExecutedVerifier {
    use std::os::unix::fs::PermissionsExt;

    let candidate = tempfile::tempdir().expect("candidate");
    let authority = tempfile::tempdir().expect("authority");
    let scratch = tempfile::tempdir().expect("scratch");
    let output = tempfile::tempdir().expect("output");
    let program = candidate.path().join("verify.sh");
    if let Some(script) = script {
      fs::write(&program, script).expect("program");
      let mut permissions = fs::metadata(&program).expect("metadata").permissions();
      permissions.set_mode(0o755);
      fs::set_permissions(&program, permissions).expect("executable");
    }
    let verifier = VerifierSpec {
      id: "V1".into(),
      command: CommandSpec {
        argv: vec![CommandArgument::CandidatePath("verify.sh".into())],
        cwd: CommandCwd::Candidate(".".into()),
        env: EnvironmentSpec::default(),
        timeout_ms,
        result: exit_policy(),
      },
      max_output_bytes: 1_024,
      authority: VerifierAuthority::Project,
      oracle_path: None,
      protection: tenet_domain::policy::VerifierProtection::Local,
    };
    let authority_id = AuthorityId(content('a'));
    let candidate_id = CandidateId(content('b'));
    let oracle = OracleIdentity::Project {
      verifier_id: "V1".into(),
      candidate_id: candidate_id.clone(),
      definition_digest: "sha256:definition".into(),
    };
    LocalProcessRunner
      .run(&VerifierRun {
        candidate_root: candidate.path(),
        authority_root: authority.path(),
        scratch_root: scratch.path(),
        output_root: output.path(),
        verifier: &verifier,
        authority_id: &authority_id,
        candidate_id: &candidate_id,
        oracle_identity: &oracle,
        view_digests: &[],
        view_directories: &[],
      })
      .expect("run verifier")
  }

  #[cfg(unix)]
  #[test]
  fn spawn_resolution_failure_is_infrastructure_error() {
    let executed = execute_script(None, 1_000);
    assert_eq!(executed.result, EvidenceResult::InfrastructureError);
    assert_eq!(executed.observation.exit_code, None);
  }

  #[cfg(unix)]
  #[test]
  fn unknown_normal_exit_is_infrastructure_error() {
    let executed = execute_script(Some("#!/bin/sh\nexit 2\n"), 1_000);
    assert_eq!(executed.result, EvidenceResult::InfrastructureError);
    assert_eq!(executed.observation.exit_code, Some(2));
  }

  #[cfg(unix)]
  #[test]
  fn timeout_is_infrastructure_error() {
    let executed = execute_script(Some("#!/bin/sh\nwhile :; do :; done\n"), 20);
    assert_eq!(executed.result, EvidenceResult::InfrastructureError);
    assert!(executed.observation.timed_out);
  }

  #[cfg(unix)]
  #[test]
  fn signal_is_infrastructure_error() {
    let executed = execute_script(Some("#!/bin/sh\nkill -TERM $$\n"), 1_000);
    assert_eq!(executed.result, EvidenceResult::InfrastructureError);
    assert_eq!(executed.observation.exit_code, None);
  }

  #[cfg(unix)]
  #[test]
  fn verifier_descendants_cannot_outlive_the_observed_process() {
    let started = Instant::now();
    let executed = execute_script(Some("#!/bin/sh\nsleep 5 &\nexit 0\n"), 1_000);
    assert_eq!(executed.result, EvidenceResult::Pass);
    assert!(started.elapsed() < Duration::from_secs(2));
  }

  use super::{LocalProcessRunner, inherited_environment_digests};

  fn content(byte: char) -> ContentObjectId {
    ContentObjectId(format!("sha256:{}", byte.to_string().repeat(64)))
  }

  fn exit_policy() -> ExitCodePolicy {
    ExitCodePolicy {
      pass: BTreeSet::from([0]),
      fail: BTreeSet::from([7]),
      inconclusive: BTreeSet::from([9]),
    }
  }

  #[test]
  fn exit_code_policy_classifies_only_declared_codes() {
    let policy = exit_policy();
    assert_eq!(
      policy.interpret(Some(0), false, false),
      EvidenceResult::Pass
    );
    assert_eq!(
      policy.interpret(Some(7), false, false),
      EvidenceResult::Fail
    );
    assert_eq!(
      policy.interpret(Some(9), false, false),
      EvidenceResult::Inconclusive
    );
    assert_eq!(
      policy.interpret(Some(2), false, false),
      EvidenceResult::InfrastructureError
    );
  }

  #[test]
  fn inherited_environment_values_change_their_fingerprint_without_being_stored() {
    let names = vec!["TOKEN".to_owned()];
    let first =
      inherited_environment_digests(&names, &[("TOKEN".into(), "first-secret".into())].into());
    let second =
      inherited_environment_digests(&names, &[("TOKEN".into(), "second-secret".into())].into());
    assert_ne!(first, second);
    assert!(!format!("{first:?}").contains("first-secret"));
  }

  #[cfg(unix)]
  #[test]
  fn equivalent_fresh_materializations_have_stable_execution_identity() {
    let first = execute_script(Some("#!/bin/sh\nexit 0\n"), 1_000);
    let second = execute_script(Some("#!/bin/sh\nexit 0\n"), 1_000);
    assert_eq!(
      first.execution.execution_environment_identity,
      second.execution.execution_environment_identity
    );
  }

  #[cfg(unix)]
  #[test]
  fn executable_bytes_change_the_execution_identity() {
    let first = execute_script(Some("#!/bin/sh\nexit 0\n"), 1_000);
    let second = execute_script(Some("#!/bin/sh\n# materially changed\nexit 0\n"), 1_000);
    assert_ne!(
      first.execution.execution_environment_identity,
      second.execution.execution_environment_identity
    );
  }

  #[cfg(unix)]
  #[test]
  fn runner_uses_typed_paths_and_explicit_environment() {
    use std::os::unix::fs::PermissionsExt;

    let candidate = tempfile::tempdir().expect("candidate");
    let authority = tempfile::tempdir().expect("authority");
    let scratch = tempfile::tempdir().expect("scratch");
    let output = tempfile::tempdir().expect("output");
    let program = candidate.path().join("tools/verify");
    fs::create_dir(candidate.path().join("tools")).expect("tools");
    fs::write(
      &program,
      "#!/bin/sh\ntest \"$MARKER\" = declared && test -z \"$HOME\" && test \"$1\" = \"$TENET_SCRATCH_ROOT/out\"\n",
    )
    .expect("program");
    let mut permissions = fs::metadata(&program).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&program, permissions).expect("executable");
    let verifier = VerifierSpec {
      id: "V1".into(),
      command: CommandSpec {
        argv: vec![
          CommandArgument::Literal("verify".into()),
          CommandArgument::ScratchPath("out".into()),
        ],
        cwd: CommandCwd::Candidate(".".into()),
        env: EnvironmentSpec {
          inherit: Vec::new(),
          set: [
            ("MARKER".into(), "declared".into()),
            ("PATH".into(), "tools".into()),
          ]
          .into(),
        },
        timeout_ms: 1_000,
        result: exit_policy(),
      },
      max_output_bytes: 1_024,
      authority: VerifierAuthority::Project,
      oracle_path: None,
      protection: tenet_domain::policy::VerifierProtection::Local,
    };
    let authority_id = AuthorityId(content('a'));
    let candidate_id = CandidateId(content('b'));
    let oracle = OracleIdentity::Project {
      verifier_id: "V1".into(),
      candidate_id: candidate_id.clone(),
      definition_digest: "sha256:definition".into(),
    };
    let executed = LocalProcessRunner
      .run(&VerifierRun {
        candidate_root: candidate.path(),
        authority_root: authority.path(),
        scratch_root: scratch.path(),
        output_root: output.path(),
        verifier: &verifier,
        authority_id: &authority_id,
        candidate_id: &candidate_id,
        oracle_identity: &oracle,
        view_digests: &[],
        view_directories: &[],
      })
      .expect("run verifier");
    assert_eq!(executed.result, EvidenceResult::Pass);
    assert_eq!(executed.context.runner_semantics.0, RUNNER_SEMANTICS_V1);
    assert_eq!(
      executed.context.resolved_program.as_deref(),
      program.to_str()
    );
    assert!(executed.context.resolved_program_digest.is_some());
  }
  #[cfg(unix)]
  #[test]
  fn private_view_script_rewrites_view_paths_and_pins_verification() {
    use std::ffi::OsString;

    use tenet_application::ports::ViewDigest;

    let candidate = tempfile::tempdir().expect("candidate");
    let authority = tempfile::tempdir().expect("authority");
    let scratch = tempfile::tempdir().expect("scratch");
    let output = tempfile::tempdir().expect("output");
    let verifier = VerifierSpec {
      id: "V1".into(),
      command: CommandSpec {
        argv: vec![
          CommandArgument::CandidatePath("verify.sh".into()),
          CommandArgument::Literal("--flag".into()),
          CommandArgument::CandidatePath("input.txt".into()),
        ],
        cwd: CommandCwd::Candidate(".".into()),
        env: EnvironmentSpec::default(),
        timeout_ms: 1_000,
        result: exit_policy(),
      },
      max_output_bytes: 1_024,
      authority: VerifierAuthority::Project,
      oracle_path: None,
      protection: tenet_domain::policy::VerifierProtection::Protected,
    };
    let authority_id = AuthorityId(content('a'));
    let candidate_id = CandidateId(content('b'));
    let oracle = OracleIdentity::Project {
      verifier_id: "V1".into(),
      candidate_id: candidate_id.clone(),
      definition_digest: "sha256:definition".into(),
    };
    let digests = [ViewDigest {
      path: "candidate/verify.sh".into(),
      sha256_hex: "a".repeat(64),
      executable: true,
    }];
    let directories = ["candidate/nested".to_owned()];
    let request = VerifierRun {
      candidate_root: candidate.path(),
      authority_root: authority.path(),
      scratch_root: scratch.path(),
      output_root: output.path(),
      verifier: &verifier,
      authority_id: &authority_id,
      candidate_id: &candidate_id,
      oracle_identity: &oracle,
      view_digests: &digests,
      view_directories: &directories,
    };
    let program = candidate.path().join("verify.sh");
    let argv: Vec<OsString> = vec![
      program.clone().into(),
      OsString::from("--flag"),
      candidate.path().join("input.txt").into(),
    ];
    let script =
      super::private_view_script(&request, &program, &argv, candidate.path()).expect("script");
    // The private view is assembled inside the namespace from the trusted
    // list, verified, and only then exec'd with view-relative paths.
    assert!(script.contains("SRC_CANDIDATE="));
    assert!(script.contains("cp -p \"$SRC_CANDIDATE/"));
    assert!(script.contains("mkdir -p \"$line\""));
    assert!(script.contains("chmod \"$mode\" \"$path\""));
    assert!(script.contains("sha256sum -c /mnt/tenet-private/checks"));
    assert!(script.contains("|| exit 70"));
    assert!(script.contains("cd '/mnt/tenet-view/candidate'"));
    assert!(script.contains(
      "exec '/mnt/tenet-view/candidate/verify.sh' '--flag' '/mnt/tenet-view/candidate/input.txt'"
    ));
    assert!(!script.contains(&format!("exec '{}'", candidate.path().display())));
  }

  #[cfg(unix)]
  #[test]
  fn private_view_script_rejects_working_directory_outside_the_view() {
    let candidate = tempfile::tempdir().expect("candidate");
    let authority = tempfile::tempdir().expect("authority");
    let scratch = tempfile::tempdir().expect("scratch");
    let output = tempfile::tempdir().expect("output");
    let verifier = VerifierSpec {
      id: "V1".into(),
      command: CommandSpec {
        argv: vec![CommandArgument::CandidatePath("verify.sh".into())],
        cwd: CommandCwd::Candidate(".".into()),
        env: EnvironmentSpec::default(),
        timeout_ms: 1_000,
        result: exit_policy(),
      },
      max_output_bytes: 1_024,
      authority: VerifierAuthority::Project,
      oracle_path: None,
      protection: tenet_domain::policy::VerifierProtection::Protected,
    };
    let authority_id = AuthorityId(content('a'));
    let candidate_id = CandidateId(content('b'));
    let oracle = OracleIdentity::Project {
      verifier_id: "V1".into(),
      candidate_id: candidate_id.clone(),
      definition_digest: "sha256:definition".into(),
    };
    let request = VerifierRun {
      candidate_root: candidate.path(),
      authority_root: authority.path(),
      scratch_root: scratch.path(),
      output_root: output.path(),
      verifier: &verifier,
      authority_id: &authority_id,
      candidate_id: &candidate_id,
      oracle_identity: &oracle,
      view_digests: &[],
      view_directories: &[],
    };
    let program = candidate.path().join("verify.sh");
    let escaped = tempfile::tempdir().expect("escaped");
    let error = super::private_view_script(
      &request,
      &program,
      &[program.clone().into()],
      escaped.path(),
    )
    .expect_err("escaped working directory must fail");
    assert!(error.to_string().contains("escapes the protected view"));
  }

  #[test]
  fn shell_quote_escapes_single_quotes() {
    assert_eq!(super::shell_quote("it's"), "'it'\\''s'");
    assert_eq!(super::shell_quote("plain"), "'plain'");
  }

  #[cfg(unix)]
  #[test]
  fn private_inputs_reject_unrepresentable_expectations() {
    use tenet_application::ports::ViewDigest;

    let candidate = tempfile::tempdir().expect("candidate");
    let authority = tempfile::tempdir().expect("authority");
    let scratch = tempfile::tempdir().expect("scratch");
    let output = tempfile::tempdir().expect("output");
    let verifier = VerifierSpec {
      id: "V1".into(),
      command: CommandSpec {
        argv: vec![CommandArgument::CandidatePath("verify.sh".into())],
        cwd: CommandCwd::Candidate(".".into()),
        env: EnvironmentSpec::default(),
        timeout_ms: 1_000,
        result: exit_policy(),
      },
      max_output_bytes: 1_024,
      authority: VerifierAuthority::Project,
      oracle_path: None,
      protection: tenet_domain::policy::VerifierProtection::Protected,
    };
    let authority_id = AuthorityId(content('a'));
    let candidate_id = CandidateId(content('b'));
    let oracle = OracleIdentity::Project {
      verifier_id: "V1".into(),
      candidate_id: candidate_id.clone(),
      definition_digest: "sha256:definition".into(),
    };
    let newline_digest = [ViewDigest {
      path: "candidate/a\nb".into(),
      sha256_hex: "a".repeat(64),
      executable: false,
    }];
    let request = VerifierRun {
      candidate_root: candidate.path(),
      authority_root: authority.path(),
      scratch_root: scratch.path(),
      output_root: output.path(),
      verifier: &verifier,
      authority_id: &authority_id,
      candidate_id: &candidate_id,
      oracle_identity: &oracle,
      view_digests: &newline_digest,
      view_directories: &[],
    };
    let error = match super::PrivateInputs::open(&request) {
      Ok(inputs) => {
        drop(inputs);
        panic!("newline path must fail");
      }
      Err(error) => error,
    };
    assert!(error.to_string().contains("verification format"));
  }
  #[cfg(unix)]
  #[test]
  fn private_view_prelude_builds_verifies_and_execs() {
    use std::os::unix::fs::PermissionsExt;

    use tenet_application::ports::ViewDigest;

    fn sha256_hex(bytes: &[u8]) -> String {
      use sha2::{Digest as _, Sha256};
      let mut hasher = Sha256::new();
      hasher.update(bytes);
      hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
    }

    let workspace = tempfile::tempdir().expect("workspace");
    let staging = workspace.path().join("staging");
    let view_root = workspace.path().join("view");
    let private = workspace.path().join("private");
    let output = workspace.path().join("output");
    fs::create_dir_all(staging.join("candidate/nested")).expect("nested");
    fs::create_dir_all(staging.join("authority")).expect("authority staging");
    fs::create_dir_all(&private).expect("private");
    fs::create_dir_all(&output).expect("output");
    fs::write(
      staging.join("candidate/candidate.txt"),
      b"original candidate",
    )
    .expect("candidate");
    fs::write(staging.join("candidate/nested/deep.txt"), b"deep content").expect("deep");
    let program = staging.join("candidate/verify.sh");
    fs::write(
      &program,
      "#!/bin/sh\npwd > \"$TENET_OUTPUT_ROOT/cwd\"\nexit 0\n",
    )
    .expect("program");
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).expect("executable");
    fs::write(staging.join("authority/snapshot.json"), b"{}").expect("authority file");
    let digests: Vec<ViewDigest> = [
      ("candidate/candidate.txt", false),
      ("candidate/nested/deep.txt", false),
      ("candidate/verify.sh", true),
      ("authority/snapshot.json", false),
    ]
    .map(|(path, executable)| ViewDigest {
      sha256_hex: sha256_hex(&fs::read(staging.join(path)).expect("digest source")),
      path: path.into(),
      executable,
    })
    .to_vec();
    let directories = ["candidate/nested".to_owned()].to_vec();
    let verifier = VerifierSpec {
      id: "V1".into(),
      command: CommandSpec {
        argv: vec![CommandArgument::CandidatePath("verify.sh".into())],
        cwd: CommandCwd::Candidate(".".into()),
        env: EnvironmentSpec::default(),
        timeout_ms: 5_000,
        result: exit_policy(),
      },
      max_output_bytes: 1_024,
      authority: VerifierAuthority::Project,
      oracle_path: None,
      protection: tenet_domain::policy::VerifierProtection::Protected,
    };
    let authority_id = AuthorityId(content('a'));
    let candidate_id = CandidateId(content('b'));
    let oracle = OracleIdentity::Project {
      verifier_id: "V1".into(),
      candidate_id: candidate_id.clone(),
      definition_digest: "sha256:definition".into(),
    };
    let candidate_root = staging.join("candidate");
    let authority_root = staging.join("authority");
    let request = VerifierRun {
      candidate_root: &candidate_root,
      authority_root: &authority_root,
      scratch_root: workspace.path(),
      output_root: &output,
      verifier: &verifier,
      authority_id: &authority_id,
      candidate_id: &candidate_id,
      oracle_identity: &oracle,
      view_digests: &digests,
      view_directories: &directories,
    };
    let script = super::private_view_script(
      &request,
      &program,
      &[program.clone().into()],
      request.candidate_root,
    )
    .expect("prelude script");
    // Simulate the namespace by redirecting the fixed sandbox constants to
    // temporary paths; the shell semantics being exercised are identical.
    let view_text = view_root.to_string_lossy().into_owned();
    let rewrite = |text: &str| {
      let mut rewritten = text
        .replace(
          super::PRIVATE_CHECKS_PATH,
          &private.join("checks").to_string_lossy(),
        )
        .replace(
          super::PRIVATE_DIRECTORIES_PATH,
          &private.join("directories").to_string_lossy(),
        )
        .replace(
          super::PRIVATE_MODES_PATH,
          &private.join("modes").to_string_lossy(),
        );
      rewritten = rewritten.replace(super::PRIVATE_VIEW_ROOT, &view_text);
      if cfg!(target_os = "macos") {
        rewritten = rewritten.replace("sha256sum -c", "shasum -a 256 -c");
      }
      rewritten
    };
    let checks: String = digests
      .iter()
      .map(|digest| {
        format!(
          "{}  {}/{}\n",
          digest.sha256_hex,
          super::PRIVATE_VIEW_ROOT,
          digest.path
        )
      })
      .collect();
    fs::write(private.join("checks"), rewrite(&checks)).expect("checks file");
    let directory_lines: String = directories
      .iter()
      .map(|directory| format!("{}/{}\n", super::PRIVATE_VIEW_ROOT, directory))
      .collect();
    fs::write(private.join("directories"), rewrite(&directory_lines)).expect("directories file");
    let mode_lines: String = digests
      .iter()
      .map(|digest| {
        format!(
          "{}/{}\t{}\n",
          super::PRIVATE_VIEW_ROOT,
          digest.path,
          if digest.executable { "755" } else { "644" }
        )
      })
      .collect();
    fs::write(private.join("modes"), rewrite(&mode_lines)).expect("modes file");

    let status = std::process::Command::new("/bin/sh")
      .arg("-c")
      .arg(rewrite(&script))
      .env("TENET_OUTPUT_ROOT", &output)
      .status()
      .expect("run prelude");
    assert!(status.success(), "prelude failed: {script}");
    assert_eq!(
      fs::read(view_root.join("candidate/candidate.txt")).expect("view candidate"),
      b"original candidate"
    );
    assert_eq!(
      fs::read(view_root.join("candidate/nested/deep.txt")).expect("view deep"),
      b"deep content"
    );
    assert_ne!(
      fs::metadata(view_root.join("candidate/verify.sh"))
        .expect("view program")
        .permissions()
        .mode()
        & 0o111,
      0,
      "the pinned executable bit must survive the private copy"
    );
    assert_eq!(
      fs::read_to_string(output.join("cwd"))
        .expect("verifier cwd")
        .trim(),
      view_root.join("candidate").to_string_lossy(),
      "the verifier must exec with the private view as its working directory"
    );

    // Corrupting the staging source after the expectations were written
    // must abort the run before the verifier ever execs.
    fs::write(staging.join("candidate/candidate.txt"), b"CORRUPTED").expect("corrupt");
    fs::remove_dir_all(&view_root).expect("reset view");
    let status = std::process::Command::new("/bin/sh")
      .arg("-c")
      .arg(rewrite(&script))
      .env("TENET_OUTPUT_ROOT", &output)
      .status()
      .expect("run corrupted prelude");
    assert_eq!(status.code(), Some(70), "digest mismatch must exit 70");
  }
}
