use std::{
  io::Read,
  path::Path,
  process::{Command, Stdio},
  thread,
  time::{Duration, Instant},
};

use anyhow::{Context, Result};
use tenet_domain::{evidence::VerifierObservation, policy::VerifierSpec};

pub fn run_verifier(checkout: &Path, verifier: &VerifierSpec) -> Result<VerifierObservation> {
  let executable = verifier.argv.first().context("verifier argv is empty")?;
  let cwd = checkout.join(&verifier.cwd);
  #[cfg(unix)]
  use std::os::unix::process::CommandExt;
  let mut command = Command::new(executable);
  command
    .args(&verifier.argv[1..])
    .current_dir(&cwd)
    .envs(&verifier.env)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  #[cfg(unix)]
  command.process_group(0);
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
