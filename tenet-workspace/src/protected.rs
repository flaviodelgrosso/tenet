//! `PROTECTED_V1` view materialization against an external hostile producer.
//!
//! Fresh materialization inside the repository is not a boundary: a hostile
//! producer with ordinary same-user filesystem access can transiently mutate
//! a materialized snapshot and restore it, so post-run detection alone cannot
//! prove the verifier never observed different bytes. This module stages the
//! exact Candidate and Authority surfaces behind the smallest OS boundary
//! that actually enforces immutability for the platform:
//!
//! - macOS: the staged tree is copied into a disk image mounted read-only
//!   through DiskArbitration; the image file is then unlinked, so the only
//!   reference to the backing store is the kernel's mount. A same-user
//!   attacker cannot write a read-only volume, cannot reach the unlinked
//!   backing store, and cannot re-attach the original once detached. The
//!   mount's identity (volume device, root inode, and source device) is
//!   time and must be unchanged after the run, which detects any
//!   detach-and-remount substitution at the mountpoint.
//! - Linux: the staged tree is handed to the runner, which creates a fresh
//!   Bubblewrap namespace, copies the tree into a private tmpfs, verifies
//!   every file against the trusted digests inside that namespace, and only
//!   then execs the verifier. The tmpfs is invisible to processes outside
//!   the namespace, so the verified bytes cannot be mutated afterwards.
//!
//! Any failure to establish the boundary is an error: callers yield an
//! infrastructure result and never downgrade assurance.

#[cfg(target_os = "macos")]
use std::process::Command;
use std::{
  fs,
  path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use tenet_application::ports::{ProtectedView, ViewDigest};
use tenet_domain::evidence::ContentObjectId;

use crate::project::ContentStore;

/// True when this platform can stage protected views behind an enforcing
/// boundary. Callers must still treat a staging failure as infrastructure.
pub fn protected_materialization_available() -> bool {
  #[cfg(target_os = "macos")]
  {
    Path::new("/usr/bin/hdiutil").is_file()
  }
  #[cfg(not(target_os = "macos"))]
  {
    true
  }
}

pub(crate) fn stage(
  store: &ContentStore,
  root: &Path,
  candidate: &ContentObjectId,
  authority: &ContentObjectId,
) -> Result<Box<dyn ProtectedView>> {
  let temporary_root = root.join(".tenet/tmp/protected");
  fs::create_dir_all(&temporary_root)?;
  let staging = tempfile::Builder::new()
    .prefix("view-")
    .tempdir_in(temporary_root)
    .context("create protected view staging directory")?;
  let candidate_directory = staging.path().join("candidate");
  let authority_directory = staging.path().join("authority");
  fs::create_dir_all(&candidate_directory)?;
  fs::create_dir_all(&authority_directory)?;
  store.materialize_into(&candidate_directory, candidate)?;
  store.materialize_into(&authority_directory, authority)?;
  let expectations = view_expectations(store, candidate, authority)?;
  #[cfg(target_os = "macos")]
  {
    Ok(Box::new(ReadonlyVolume::stage(
      store,
      staging,
      expectations,
    )?))
  }
  #[cfg(not(target_os = "macos"))]
  {
    Ok(Box::new(StagedDirectory::new(staging, expectations)))
  }
}

/// Trusted expectations for the private namespace copy: every file's exact
/// bytes and mode, and every directory (including empty ones), so the view a
/// namespace-creating runner builds is exactly the captured tree.
pub(crate) struct ViewExpectations {
  pub digests: Vec<ViewDigest>,
  pub directories: Vec<String>,
}

fn view_expectations(
  store: &ContentStore,
  candidate: &ContentObjectId,
  authority: &ContentObjectId,
) -> Result<ViewExpectations> {
  let mut digests = Vec::new();
  let mut directories = Vec::new();
  for (prefix, id) in [("candidate", candidate), ("authority", authority)] {
    let manifest = store.manifest(id)?;
    for entry in manifest.entries {
      match entry.content_id {
        Some(content) => {
          let hex = content
            .0
            .strip_prefix("sha256:")
            .context("content identity is not sha256")?
            .to_owned();
          digests.push(ViewDigest {
            path: format!("{prefix}/{}", entry.path),
            sha256_hex: hex,
            executable: entry.executable,
          });
        }
        None => directories.push(format!("{prefix}/{}", entry.path)),
      }
    }
  }
  Ok(ViewExpectations {
    digests,
    directories,
  })
}
#[cfg(not(target_os = "macos"))]
struct StagedDirectory {
  // Held solely so the staging directory lives exactly as long as the view;
  // the runner's private namespace copy, not this directory, is the boundary.
  #[allow(dead_code)]
  staging: tempfile::TempDir,
  candidate: PathBuf,
  authority: PathBuf,
  expectations: ViewExpectations,
}
#[cfg(not(target_os = "macos"))]
impl StagedDirectory {
  fn new(staging: tempfile::TempDir, expectations: ViewExpectations) -> Self {
    let candidate = staging.path().join("candidate");
    let authority = staging.path().join("authority");
    Self {
      staging,
      candidate,
      authority,
      expectations,
    }
  }
}
#[cfg(not(target_os = "macos"))]
impl ProtectedView for StagedDirectory {
  fn candidate_root(&self) -> &Path {
    &self.candidate
  }

  fn authority_root(&self) -> &Path {
    &self.authority
  }

  fn view_digests(&self) -> &[ViewDigest] {
    &self.expectations.digests
  }

  fn view_directories(&self) -> &[String] {
    &self.expectations.directories
  }

  fn verify_intact(
    &self,
    _expected_candidate: &ContentObjectId,
    _expected_authority: &ContentObjectId,
  ) -> Result<bool> {
    // The enforcing boundary for this backend is the runner's private
    // namespace: the verifier read a tmpfs copy built inside that namespace
    // from the trusted expectations and verified before exec. The staging
    // directory here is transient by design and not the boundary.
    Ok(true)
  }
}

#[cfg(target_os = "macos")]
struct VolumeIdentity {
  /// Device number of the mounted volume; a re-attached substitute volume
  /// always receives a different disk device.
  device: u64,
  inode: u64,
  mount_from: String,
}

#[cfg(target_os = "macos")]
fn volume_identity(mount: &Path) -> Result<Option<VolumeIdentity>> {
  use std::{
    ffi::CStr,
    fs, io,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
  };

  let encoded = std::ffi::CString::new(mount.as_os_str().as_bytes())
    .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
  let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
  // SAFETY: `encoded` is a valid NUL-terminated path and `stat` is writable.
  if unsafe { libc::statfs(encoded.as_ptr(), &mut stat) } != 0 {
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
      return Ok(None);
    }
    return Err(error.into());
  }
  let metadata = fs::metadata(mount)?;
  Ok(Some(VolumeIdentity {
    device: metadata.dev(),
    inode: metadata.ino(),
    mount_from: unsafe { CStr::from_ptr(stat.f_mntfromname.as_ptr()) }
      .to_string_lossy()
      .into_owned(),
  }))
}

#[cfg(target_os = "macos")]
fn run_hdiutil(arguments: &[&str]) -> Result<()> {
  let output = Command::new("/usr/bin/hdiutil")
    .args(arguments)
    .output()
    .context("spawn hdiutil")?;
  if !output.status.success() {
    anyhow::bail!(
      "hdiutil {} failed: {}",
      arguments.first().copied().unwrap_or_default(),
      String::from_utf8_lossy(&output.stderr).trim()
    );
  }
  Ok(())
}

#[cfg(target_os = "macos")]
fn random_mountpoint() -> Result<PathBuf> {
  let probe = tempfile::Builder::new()
    .prefix("tenet-protected-")
    .tempdir()
    .context("generate protected volume name")?;
  let name = probe
    .path()
    .file_name()
    .context("temporary name")?
    .to_owned();
  drop(probe);
  Ok(Path::new("/Volumes").join(name))
}

/// A read-only DiskArbitration volume whose backing store is unlinked.
/// The mounted bytes are immutable for every user, the image cannot be
/// rewritten (no reachable path), and the original volume cannot be
/// re-attached after a detach, so an unchanged `statfs` identity across the
/// run proves the verifier observed exactly these bytes.
#[cfg(target_os = "macos")]
struct ReadonlyVolume {
  project_root: PathBuf,
  image: PathBuf,
  mount: PathBuf,
  candidate: PathBuf,
  authority: PathBuf,
  expectations: ViewExpectations,
  identity: VolumeIdentity,
}

#[cfg(target_os = "macos")]
impl ReadonlyVolume {
  fn stage(
    store: &ContentStore,
    staging: tempfile::TempDir,
    expectations: ViewExpectations,
  ) -> Result<Self> {
    if staging
      .path()
      .file_name()
      .is_none_or(|name| name.to_string_lossy().contains('.'))
    {
      anyhow::bail!("protected staging directory has an unusable name");
    }
    let image = staging.path().with_extension("dmg");
    let staged = run_hdiutil(&[
      "create",
      "-quiet",
      "-srcfolder",
      &staging.path().to_string_lossy(),
      "-format",
      "UDRW",
      "-o",
      &image.to_string_lossy(),
    ]);
    if let Err(error) = staged {
      let _ = fs::remove_file(&image);
      return Err(error);
    }
    // The image now owns the exact staged bytes; the staging tree is gone,
    // so no host path can feed further changes into the volume.
    let _ = staging.close();
    let mut last_error = None;
    for _ in 0..3 {
      let mount = random_mountpoint()?;
      let attached = run_hdiutil(&[
        "attach",
        "-quiet",
        "-readonly",
        "-nobrowse",
        "-mountpoint",
        &mount.to_string_lossy(),
        &image.to_string_lossy(),
      ]);
      match attached {
        Ok(()) => {
          // From this point the only reference to the backing store is the
          // kernel's mount; no process can reach the image bytes by path.
          if let Err(error) = fs::remove_file(&image) {
            let _ = run_hdiutil(&["detach", "-quiet", &mount.to_string_lossy()]);
            return Err(error).with_context(|| format!("unlink image {}", image.display()));
          }
          let identity = volume_identity(&mount)?.context("mounted volume has no identity")?;
          let candidate = mount.join("candidate");
          let authority = mount.join("authority");
          return Ok(Self {
            project_root: store.project_root().to_path_buf(),
            image,
            mount,
            candidate,
            authority,
            expectations,
            identity,
          });
        }
        Err(error) => last_error = Some(error),
      }
    }
    let _ = fs::remove_file(&image);
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("attach protected volume")))
  }
}

#[cfg(target_os = "macos")]
impl ProtectedView for ReadonlyVolume {
  fn candidate_root(&self) -> &Path {
    &self.candidate
  }

  fn authority_root(&self) -> &Path {
    &self.authority
  }

  fn view_digests(&self) -> &[ViewDigest] {
    &self.expectations.digests
  }

  fn view_directories(&self) -> &[String] {
    &self.expectations.directories
  }

  fn verify_intact(
    &self,
    expected_candidate: &ContentObjectId,
    expected_authority: &ContentObjectId,
  ) -> Result<bool> {
    let Some(current) = volume_identity(&self.mount)? else {
      return Ok(false);
    };
    if current.device != self.identity.device
      || current.inode != self.identity.inode
      || current.mount_from != self.identity.mount_from
    {
      return Ok(false);
    }
    // The mounted volume is the verifier's actual read surface; its bytes
    // must still hash to the exact admitted identities.
    let store = ContentStore::open(&self.project_root)?;
    let candidate = store.capture_selected(&self.candidate, &["**".to_owned()], &[])?;
    if candidate != *expected_candidate {
      return Ok(false);
    }
    let authority = store.capture(&self.authority)?;
    Ok(authority == *expected_authority)
  }
}

#[cfg(target_os = "macos")]
impl Drop for ReadonlyVolume {
  fn drop(&mut self) {
    let _ = run_hdiutil(&["detach", "-quiet", &self.mount.to_string_lossy()]);
    let _ = fs::remove_file(&self.image);
  }
}
