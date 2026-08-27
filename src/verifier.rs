use std::{
  io::Read,
  path::Path,
  process::{Command, Stdio},
  thread,
  time::{Duration, Instant},
};

use anyhow::{Context, Result};
use tenet_domain::{
  evidence::VerifierObservation,
  policy::{VerifierAuthority, VerifierSpec},
};

pub fn run_verifier(
  candidate_checkout: &Path,
  oracle_bundle: Option<&Path>,
  verifier: &VerifierSpec,
) -> Result<VerifierObservation> {
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
  command
    .args(&verifier.argv[1..])
    .envs(&verifier.env)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  if verifier.authority == VerifierAuthority::AuthoritySnapshot {
    command.env("TENET_CANDIDATE_ROOT", &candidate_checkout);
  }
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
  Ok(VerifierObservation {
    exit_code: status.code(),
    stdout,
    stderr,
    timed_out,
  })
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
