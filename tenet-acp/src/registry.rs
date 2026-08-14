use std::{
  collections::BTreeMap,
  env, fs as std_fs,
  io::{self, Cursor, Read},
  path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::Archive;
use tokio::fs;
use uuid::Uuid;
use zip::ZipArchive;

pub use tenet_runtime::backend::LaunchMetadata;

const DEFAULT_INDEX_URL: &str =
  "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryIndex {
  pub version: String,
  pub agents: Vec<RegistryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
  pub id: String,
  pub name: String,
  pub version: String,
  pub description: String,
  pub distribution: RegistryDistribution,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegistryDistribution {
  #[serde(default)]
  pub npx: Option<PackageDistribution>,
  #[serde(default)]
  pub uvx: Option<PackageDistribution>,
  #[serde(default)]
  pub binary: BTreeMap<String, BinaryDistribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageDistribution {
  pub package: String,
  #[serde(default)]
  pub args: Vec<String>,
  #[serde(default)]
  pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryDistribution {
  pub archive: String,
  pub cmd: String,
  #[serde(default)]
  pub args: Vec<String>,
  #[serde(default)]
  pub env: BTreeMap<String, String>,
  #[serde(default)]
  pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedAgent {
  pub id: String,
  pub name: String,
  pub display_name: String,
  pub version: String,
  pub description: String,
  pub launch: LaunchMetadata,
}

#[derive(Debug, Clone)]
pub struct RegistryClient {
  source: String,
}

impl Default for RegistryClient {
  fn default() -> Self {
    Self {
      source: env::var("TENET_REGISTRY_URL").unwrap_or_else(|_| DEFAULT_INDEX_URL.into()),
    }
  }
}

impl RegistryClient {
  pub fn from_source(source: impl Into<String>) -> Self {
    Self {
      source: source.into(),
    }
  }

  pub async fn refresh(&self, cache_dir: &Path) -> Result<RegistryIndex> {
    let bytes = read_source(&self.source).await.with_context(|| {
      format!(
        "Registry unavailable at {}; next action: restore Registry access or use cached metadata/custom ACP command",
        self.source
      )
    })?;

    let index: RegistryIndex = serde_json::from_slice(&bytes).context(
      "malformed Registry v1 index; next action: repair the Registry source or configure a custom ACP command",
    )?;

    validate_index(&index)?;
    fs::create_dir_all(cache_dir).await?;
    fs::write(cache_dir.join("index.json"), &bytes).await?;
    Ok(index)
  }

  pub async fn load_index(&self, cache_dir: &Path) -> Result<RegistryIndex> {
    match self.refresh(cache_dir).await {
      Ok(index) => Ok(index),
      Err(refresh_error) => {
        let bytes = fs::read(cache_dir.join("index.json")).await.map_err(|cache_error| {
          anyhow::anyhow!(
            "Registry refresh failed ({refresh_error}); cached index unavailable ({cache_error}); next action: restore Registry access or configure a custom ACP command"
          )
        })?;

        let index: RegistryIndex = serde_json::from_slice(&bytes).with_context(|| {
          format!(
            "cached Registry index is malformed ({refresh_error}); next action: refresh the Registry or configure a custom ACP command"
          )
        })?;

        validate_index(&index)?;
        Ok(index)
      }
    }
  }

  pub async fn resolve(&self, cache_dir: &Path, id: &str) -> Result<ResolvedAgent> {
    match self.load_index(cache_dir).await {
      Ok(index) => {
        let entry = index.agents.iter().find(|entry| entry.id == id).ok_or_else(|| {
          anyhow::anyhow!(
            "unknown Registry agent {id:?}; next action: list Registry agents or configure a custom ACP command"
          )
        })?;

        let resolved = resolve_entry(cache_dir, entry)?;
        cache_resolved(cache_dir, &resolved).await?;
        Ok(resolved)
      }
      Err(index_error) => load_cached_resolved(cache_dir, id).await.with_context(|| {
        format!(
          "Registry resolution failed ({index_error}); next action: restore Registry access or configure a custom ACP command"
        )
      }),
    }
  }

  /// Downloads and verifies the current platform's declared binary distribution.
  ///
  /// This is the explicit-consent path for machine-changing Registry setup;
  /// [`Self::resolve`] only uses a binary already verified in the cache.
  pub async fn setup_binary(&self, cache_dir: &Path, id: &str) -> Result<ResolvedAgent> {
    let index = self.load_index(cache_dir).await?;
    let entry = index.agents.iter().find(|entry| entry.id == id).ok_or_else(|| {
      anyhow::anyhow!(
        "unknown Registry agent {id:?}; next action: list Registry agents or configure a custom ACP command"
      )
    })?;

    let (platform, distribution) = binary_distribution(entry)?;
    let executable = binary_install_path(cache_dir, entry, &platform, &distribution.cmd)?;

    if verified_binary(&executable, distribution) {
      let resolved = resolved_binary_entry(cache_dir, entry)?;
      cache_resolved(cache_dir, &resolved).await?;
      return Ok(resolved);
    }

    let archive = read_source(&distribution.archive).await.with_context(|| {
      format!(
        "could not download Registry binary distribution for {id:?}; next action: restore access to {} or configure a custom ACP command",
        distribution.archive
      )
    })?;

    verify_archive_checksum(&archive, distribution)?;

    let install_root = binary_install_root(cache_dir, entry, &platform);
    let install_parent = install_root.parent().ok_or_else(|| {
      anyhow::anyhow!(
        "Registry binary cache path is invalid; next action: choose a writable Registry cache directory"
      )
    })?;

    fs::create_dir_all(install_parent).await?;

    let staging = install_parent.join(format!(".setup-{}", Uuid::new_v4()));
    let setup = (|| {
      extract_archive(&archive, &distribution.archive, &staging)?;
      let staged_executable = staging.join(normalized_binary_command(&distribution.cmd)?);
      if !staged_executable.is_file() {
        anyhow::bail!(
          "binary archive does not contain declared command {:?}; next action: repair the Registry distribution or configure a custom ACP command",
          distribution.cmd
        )
      }

      std_fs::write(
        staging.join(".registry-sha256"),
        distribution.sha256.as_deref().unwrap_or_default(),
      )?;
      Ok(())
    })();

    if let Err(error) = setup {
      let _ = fs::remove_dir_all(&staging).await;
      return Err(error).with_context(|| {
        format!(
          "Registry binary setup for {id:?} failed; next action: repair the Registry distribution or configure a custom ACP command"
        )
      });
    }

    if install_root.exists() {
      fs::remove_dir_all(&install_root).await?;
    }

    fs::rename(&staging, &install_root).await?;
    let resolved = resolved_binary_entry(cache_dir, entry)?;
    cache_resolved(cache_dir, &resolved).await?;
    Ok(resolved)
  }
}

fn validate_index(index: &RegistryIndex) -> Result<()> {
  if !index.version.starts_with("1.") {
    anyhow::bail!(
      "unsupported Registry index version {:?}; next action: use a Registry v1 source or update Tenet",
      index.version
    )
  }

  for entry in &index.agents {
    if entry.id.trim().is_empty()
      || entry.name.trim().is_empty()
      || entry.version.trim().is_empty()
      || entry.description.trim().is_empty()
    {
      anyhow::bail!(
        "malformed Registry entry; next action: refresh the Registry or configure a custom ACP command"
      )
    }

    validate_distribution(entry)?;
  }
  Ok(())
}

fn resolve_entry(cache_dir: &Path, entry: &RegistryEntry) -> Result<ResolvedAgent> {
  let launch = if let Some(distribution) = &entry.distribution.npx {
    package_launch("npx", distribution, entry, "@")
  } else if let Some(distribution) = &entry.distribution.uvx {
    package_launch("uvx", distribution, entry, "==")
  } else if entry.distribution.binary.is_empty() {
    anyhow::bail!(
      "Registry agent {:?} has no supported distribution; next action: select an agent with an npx, uvx, or binary distribution",
      entry.id
    )
  } else {
    binary_launch(cache_dir, entry)?
  };

  Ok(ResolvedAgent {
    id: entry.id.clone(),
    name: entry.name.clone(),
    display_name: entry.name.clone(),
    version: entry.version.clone(),
    description: entry.description.clone(),
    launch,
  })
}

fn package_launch(
  runner: &str,
  distribution: &PackageDistribution,
  entry: &RegistryEntry,
  version_separator: &str,
) -> LaunchMetadata {
  let package = package_with_version(&distribution.package, &entry.version, version_separator);
  let mut args = Vec::with_capacity(distribution.args.len() + 1);
  args.push(package.clone());
  args.extend(distribution.args.iter().cloned());
  LaunchMetadata {
    command: runner.into(),
    args,
    env: distribution.env.clone(),
    provenance: format!(
      "Registry {runner} distribution for {} {} ({package})",
      entry.id, entry.version
    ),
  }
}

fn package_with_version(package: &str, version: &str, separator: &str) -> String {
  let has_version = match separator {
    "@" => package
      .rsplit_once('@')
      .is_some_and(|(name, version)| !name.is_empty() && !version.is_empty()),
    "==" => {
      package.contains("==")
        || package
          .rsplit_once('@')
          .is_some_and(|(name, version)| !name.is_empty() && !version.is_empty())
    }
    _ => false,
  };
  if has_version {
    package.into()
  } else {
    format!("{package}{separator}{version}")
  }
}

fn binary_launch(cache_dir: &Path, entry: &RegistryEntry) -> Result<LaunchMetadata> {
  let (platform, distribution) = binary_distribution(entry)?;
  let executable = binary_install_path(cache_dir, entry, &platform, &distribution.cmd)?;
  if !verified_binary(&executable, distribution) {
    anyhow::bail!(
      "Registry binary distribution for {} {} ({platform}) is not installed and checksum-verified; provenance: {}; next action: use setup_binary to install it or configure a custom ACP command",
      entry.id,
      entry.version,
      distribution.archive,
    )
  }
  Ok(LaunchMetadata {
    command: executable.to_string_lossy().into_owned(),
    args: distribution.args.clone(),
    env: distribution.env.clone(),
    provenance: format!(
      "Registry binary distribution for {} {} ({platform}); archive {}",
      entry.id, entry.version, distribution.archive
    ),
  })
}

fn current_platform_key() -> Result<String> {
  let os = match env::consts::OS {
    "macos" => "darwin",
    "linux" => "linux",
    "windows" => "windows",
    current => anyhow::bail!(
      "Registry binary distributions do not support {current}; next action: use an npx or uvx distribution, or configure a custom ACP command"
    ),
  };
  let architecture = match env::consts::ARCH {
    "aarch64" => "aarch64",
    "x86_64" => "x86_64",
    current => anyhow::bail!(
      "Registry binary distributions do not support architecture {current}; next action: use an npx or uvx distribution, or configure a custom ACP command"
    ),
  };
  Ok(format!("{os}-{architecture}"))
}

fn binary_distribution(entry: &RegistryEntry) -> Result<(String, &BinaryDistribution)> {
  let platform = current_platform_key()?;
  let distribution = entry.distribution.binary.get(&platform).ok_or_else(|| {
    anyhow::anyhow!(
      "Registry agent {:?} has no compatible binary distribution for {platform}; next action: install a compatible distribution or configure a custom ACP command",
      entry.id
    )
  })?;
  Ok((platform, distribution))
}

fn binary_install_root(cache_dir: &Path, entry: &RegistryEntry, platform: &str) -> PathBuf {
  cache_dir
    .join("installed")
    .join(safe_id(&entry.id))
    .join(safe_id(&entry.version))
    .join(platform)
}

fn binary_install_path(
  cache_dir: &Path,
  entry: &RegistryEntry,
  platform: &str,
  command: &str,
) -> Result<PathBuf> {
  let command = normalized_binary_command(command).map_err(|_| {
    anyhow::anyhow!(
      "Registry binary distribution for {:?} has an unsafe command path; next action: refresh the Registry or configure a custom ACP command",
      entry.id
    )
  })?;
  Ok(binary_install_root(cache_dir, entry, platform).join(command))
}

fn verified_binary(executable: &Path, distribution: &BinaryDistribution) -> bool {
  let Ok(command) = normalized_binary_command(&distribution.cmd) else {
    return false;
  };
  if !std_fs::symlink_metadata(executable).is_ok_and(|metadata| metadata.file_type().is_file()) {
    return false;
  }
  let Some(expected_checksum) = distribution.sha256.as_deref() else {
    return false;
  };
  let Some(install_root) = executable.ancestors().nth(command.components().count()) else {
    return false;
  };
  std_fs::read_to_string(install_root.join(".registry-sha256")).is_ok_and(|stored_checksum| {
    stored_checksum
      .trim()
      .eq_ignore_ascii_case(expected_checksum)
  })
}

fn normalized_binary_command(command: &str) -> Result<PathBuf> {
  let path = Path::new(command);
  let mut normalized = PathBuf::new();
  for component in path.components() {
    match component {
      Component::CurDir => {}
      Component::Normal(part) => normalized.push(part),
      Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
        anyhow::bail!("binary command must be a relative path")
      }
    }
  }
  if normalized.as_os_str().is_empty() {
    anyhow::bail!("binary command must not be blank")
  }
  Ok(normalized)
}

fn verify_archive_checksum(archive: &[u8], distribution: &BinaryDistribution) -> Result<()> {
  let expected_checksum = distribution.sha256.as_deref().filter(|checksum| !checksum.trim().is_empty()).ok_or_else(|| {
    anyhow::anyhow!(
      "Registry binary distribution has no SHA-256 checksum; next action: repair the Registry distribution or configure a custom ACP command"
    )
  })?;
  let actual_checksum = Sha256::digest(archive)
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect::<String>();
  if !actual_checksum.eq_ignore_ascii_case(expected_checksum) {
    anyhow::bail!(
      "Registry binary archive checksum does not match its declared SHA-256; next action: retry with a trusted Registry source or configure a custom ACP command"
    )
  }
  Ok(())
}

fn extract_archive(archive: &[u8], source: &str, destination: &Path) -> Result<()> {
  std_fs::create_dir(destination).with_context(|| {
    format!(
      "could not create Registry binary staging directory {}; next action: choose a writable Registry cache directory",
      destination.display()
    )
  })?;
  let source = source
    .split(['?', '#'])
    .next()
    .unwrap_or(source)
    .to_ascii_lowercase();
  if source.ends_with(".zip") {
    extract_zip_archive(archive, destination)
  } else if source.ends_with(".tar.gz") || source.ends_with(".tgz") {
    extract_tar_archive(GzDecoder::new(Cursor::new(archive)), destination)
  } else if source.ends_with(".tar") {
    extract_tar_archive(Cursor::new(archive), destination)
  } else {
    anyhow::bail!(
      "unsupported Registry binary archive format for {source:?}; supported formats are .zip, .tar.gz, .tgz, and .tar; next action: repair the Registry distribution or configure a custom ACP command"
    )
  }
}

fn extract_zip_archive(archive: &[u8], destination: &Path) -> Result<()> {
  let mut archive = ZipArchive::new(Cursor::new(archive))
    .context("Registry binary archive is not a valid ZIP file")?;
  for index in 0..archive.len() {
    let mut entry = archive
      .by_index(index)
      .with_context(|| format!("could not read ZIP entry {index}"))?;
    if entry.is_symlink() {
      anyhow::bail!(
        "Registry binary ZIP archive contains a symbolic link; next action: repair the Registry distribution or configure a custom ACP command"
      )
    }
    let path = entry.enclosed_name().ok_or_else(|| {
      anyhow::anyhow!(
        "Registry binary ZIP archive contains an unsafe path; next action: repair the Registry distribution or configure a custom ACP command"
      )
    })?;
    if !is_safe_relative_path(&path) {
      anyhow::bail!(
        "Registry binary ZIP archive contains an unsafe path; next action: repair the Registry distribution or configure a custom ACP command"
      )
    }
    let output = destination.join(path);
    if entry.is_dir() {
      std_fs::create_dir_all(&output)?;
    } else {
      let parent = output
        .parent()
        .ok_or_else(|| anyhow::anyhow!("ZIP entry has no parent"))?;
      std_fs::create_dir_all(parent)?;
      let mut file = std_fs::File::create(&output)?;
      io::copy(&mut entry, &mut file)?;
    }
  }
  Ok(())
}

fn extract_tar_archive<R: Read>(archive: R, destination: &Path) -> Result<()> {
  let mut archive = Archive::new(archive);
  let entries = archive
    .entries()
    .context("Registry binary archive is not a valid TAR file")?;
  for entry in entries {
    let mut entry = entry.context("could not read TAR archive entry")?;
    let path = entry
      .path()
      .context("could not read TAR archive entry path")?
      .into_owned();
    if !is_safe_relative_path(&path) {
      anyhow::bail!(
        "Registry binary TAR archive contains an unsafe path; next action: repair the Registry distribution or configure a custom ACP command"
      )
    }
    let output = destination.join(path);
    let entry_type = entry.header().entry_type();
    if entry_type.is_dir() {
      std_fs::create_dir_all(&output)?;
    } else if entry_type.is_file() {
      let parent = output
        .parent()
        .ok_or_else(|| anyhow::anyhow!("TAR entry has no parent"))?;
      std_fs::create_dir_all(parent)?;
      let mut file = std_fs::File::create(&output)?;
      io::copy(&mut entry, &mut file)?;
    } else {
      anyhow::bail!(
        "Registry binary TAR archive contains an unsafe non-file entry; next action: repair the Registry distribution or configure a custom ACP command"
      )
    }
  }
  Ok(())
}

fn is_safe_relative_path(path: &Path) -> bool {
  !path.as_os_str().is_empty()
    && path
      .components()
      .all(|component| matches!(component, Component::Normal(_)))
}

fn resolved_binary_entry(cache_dir: &Path, entry: &RegistryEntry) -> Result<ResolvedAgent> {
  Ok(ResolvedAgent {
    id: entry.id.clone(),
    name: entry.name.clone(),
    display_name: entry.name.clone(),
    version: entry.version.clone(),
    description: entry.description.clone(),
    launch: binary_launch(cache_dir, entry)?,
  })
}

fn validate_distribution(entry: &RegistryEntry) -> Result<()> {
  for distribution in [&entry.distribution.npx, &entry.distribution.uvx]
    .into_iter()
    .flatten()
  {
    if distribution.package.trim().is_empty() {
      anyhow::bail!(
        "Registry agent {:?} has a distribution with a blank package; next action: refresh the Registry or configure a custom ACP command",
        entry.id
      )
    }
  }
  for (platform, distribution) in &entry.distribution.binary {
    if platform.trim().is_empty()
      || distribution.archive.trim().is_empty()
      || distribution.cmd.trim().is_empty()
      || normalized_binary_command(&distribution.cmd).is_err()
    {
      anyhow::bail!(
        "Registry agent {:?} has a malformed binary distribution for {platform:?}; next action: refresh the Registry or configure a custom ACP command",
        entry.id
      )
    }
  }
  Ok(())
}

async fn cache_resolved(cache_dir: &Path, resolved: &ResolvedAgent) -> Result<()> {
  fs::create_dir_all(cache_dir).await?;
  fs::write(
    cache_dir.join(format!("resolved-{}.json", safe_id(&resolved.id))),
    serde_json::to_vec_pretty(resolved)?,
  )
  .await?;
  Ok(())
}

async fn load_cached_resolved(cache_dir: &Path, id: &str) -> Result<ResolvedAgent> {
  let path = cache_dir.join(format!("resolved-{}.json", safe_id(id)));
  let bytes = fs::read(&path).await.map_err(|_| {
    anyhow::anyhow!(
      "Registry agent {id:?} is unavailable and not cached; next action: restore Registry access or configure a custom ACP command"
    )
  })?;
  let resolved: ResolvedAgent = serde_json::from_slice(&bytes).with_context(|| {
    format!(
      "cached Registry metadata for {id:?} is malformed; next action: refresh the Registry or configure a custom ACP command"
    )
  })?;
  if resolved.id != id {
    anyhow::bail!(
      "cached Registry metadata does not match {id:?}; next action: refresh the Registry or configure a custom ACP command"
    )
  }
  Ok(resolved)
}

async fn read_source(source: &str) -> Result<Vec<u8>> {
  let path = source.strip_prefix("file://").unwrap_or(source);
  if source.starts_with("http://") || source.starts_with("https://") {
    let output = tokio::process::Command::new("curl")
      .args(["--fail", "--location", "--silent", "--show-error", source])
      .output()
      .await
      .context("start Registry HTTP transport")?;
    if !output.status.success() {
      anyhow::bail!(
        "Registry HTTP request failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
      )
    }
    return Ok(output.stdout);
  }
  Ok(fs::read(PathBuf::from(path)).await?)
}

fn safe_id(value: &str) -> String {
  value
    .chars()
    .map(|character| {
      if character.is_ascii_alphanumeric() {
        character
      } else {
        '_'
      }
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Write;
  use tempfile::tempdir;
  use zip::{write::SimpleFileOptions, ZipWriter};

  fn official_fixture(distribution: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
      "version": "1.0.0",
      "agents": [{
        "id": "fixture",
        "name": "Fixture Agent",
        "version": "1.2.3",
        "description": "Official-format fixture agent",
        "distribution": distribution,
      }],
    })
  }

  #[tokio::test]
  async fn resolves_an_official_format_npx_distribution() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("registry.json");
    let index = official_fixture(serde_json::json!({
      "npx": {
        "package": "fixture-acp",
        "args": ["--acp"],
        "env": {"FIXTURE_MODE": "1"},
      },
    }));
    fs::write(&source, serde_json::to_vec(&index).unwrap())
      .await
      .unwrap();

    let resolved = RegistryClient::from_source(source.to_str().unwrap())
      .resolve(&dir.path().join("cache"), "fixture")
      .await
      .unwrap();

    assert_eq!(resolved.name, "Fixture Agent");
    assert_eq!(resolved.launch.command, "npx");
    assert_eq!(resolved.launch.args, ["fixture-acp@1.2.3", "--acp"]);
    assert_eq!(resolved.launch.env.get("FIXTURE_MODE"), Some(&"1".into()));
  }

  #[tokio::test]
  async fn reports_an_incompatible_binary_platform() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("registry.json");
    let index = official_fixture(serde_json::json!({
      "binary": {
        "not-a-supported-platform": {
          "archive": "https://example.test/fixture.tar.gz",
          "cmd": "fixture-acp",
          "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        },
      },
    }));
    fs::write(&source, serde_json::to_vec(&index).unwrap())
      .await
      .unwrap();

    let error = RegistryClient::from_source(source.to_str().unwrap())
      .resolve(&dir.path().join("cache"), "fixture")
      .await
      .unwrap_err()
      .to_string();

    assert!(error.contains("no compatible binary distribution"));
    assert!(error.contains("next action"));
  }

  #[tokio::test]
  async fn reuses_resolved_metadata_when_registry_and_index_are_offline() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("registry.json");
    let cache = dir.path().join("cache");
    let index = official_fixture(serde_json::json!({
      "uvx": {"package": "fixture-acp"},
    }));
    fs::write(&source, serde_json::to_vec(&index).unwrap())
      .await
      .unwrap();
    let client = RegistryClient::from_source(source.to_str().unwrap());
    let first = client.resolve(&cache, "fixture").await.unwrap();

    fs::remove_file(source).await.unwrap();
    fs::remove_file(cache.join("index.json")).await.unwrap();
    let cached = client.resolve(&cache, "fixture").await.unwrap();

    assert_eq!(cached.id, first.id);
    assert_eq!(cached.launch.args, ["fixture-acp==1.2.3"]);
  }

  #[tokio::test]
  async fn sets_up_a_local_zip_binary_and_resolves_after_index_removal() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("registry.json");
    let archive = dir.path().join("fixture.zip");
    let cache = dir.path().join("cache");
    let mut zip = ZipWriter::new(std_fs::File::create(&archive).unwrap());
    zip
      .start_file("bin/fixture-acp", SimpleFileOptions::default())
      .unwrap();
    zip.write_all(b"fixture executable").unwrap();
    zip.finish().unwrap();
    let checksum = Sha256::digest(std_fs::read(&archive).unwrap())
      .iter()
      .map(|byte| format!("{byte:02x}"))
      .collect::<String>();
    let platform = current_platform_key().unwrap();
    let index = official_fixture(serde_json::json!({
      "binary": {
        (platform): {
          "archive": archive.to_string_lossy().into_owned(),
          "cmd": "./bin/fixture-acp",
          "args": ["serve"],
          "sha256": checksum,
        },
      },
    }));
    fs::write(&source, serde_json::to_vec(&index).unwrap())
      .await
      .unwrap();

    let client = RegistryClient::from_source(source.to_str().unwrap());
    let installed = client.setup_binary(&cache, "fixture").await.unwrap();
    assert!(Path::new(&installed.launch.command).is_file());

    fs::remove_file(source).await.unwrap();
    fs::remove_file(cache.join("index.json")).await.unwrap();
    let resolved = client.resolve(&cache, "fixture").await.unwrap();

    assert_eq!(resolved.launch.command, installed.launch.command);
    assert_eq!(resolved.launch.args, ["serve"]);
  }

  #[tokio::test]
  async fn selects_cached_binary_for_current_platform() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("registry.json");
    let cache = dir.path().join("cache");
    let platform = current_platform_key().unwrap();
    let checksum = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let index = official_fixture(serde_json::json!({
      "binary": {
        (platform.clone()): {
          "archive": "https://example.test/fixture.tar.gz",
          "cmd": "bin/fixture-acp",
          "args": ["serve"],
          "sha256": checksum,
        },
      },
    }));
    fs::write(&source, serde_json::to_vec(&index).unwrap())
      .await
      .unwrap();
    let entry: RegistryEntry = serde_json::from_value(index["agents"][0].clone()).unwrap();
    let executable = binary_install_path(&cache, &entry, &platform, "bin/fixture-acp").unwrap();
    fs::create_dir_all(executable.parent().unwrap())
      .await
      .unwrap();
    fs::write(&executable, b"fixture").await.unwrap();
    fs::write(
      binary_install_root(&cache, &entry, &platform).join(".registry-sha256"),
      checksum,
    )
    .await
    .unwrap();

    let resolved = RegistryClient::from_source(source.to_str().unwrap())
      .resolve(&cache, "fixture")
      .await
      .unwrap();

    assert_eq!(resolved.launch.command, executable.to_string_lossy());
  }
}
