use std::{
  fs,
  path::{Path, PathBuf},
  process::Command,
};

use anyhow::{bail, Context, Result};
use tenet_domain::{
  digest::{bytes_digest, canonical_digest},
  policy::{validate_policy, RepositoryConfig, VerificationPolicy},
};

pub const TENET_DIR: &str = ".tenet";
pub const CONFIG_PATH: &str = ".tenet/tenet.toml";
pub const CONTRACT_PATH: &str = ".tenet/contract.json";
pub const STATE_PATH: &str = ".tenet/state.json";
pub const SKILL_PATH: &str = ".agents/skills/tenet/SKILL.md";

pub const SKILL: &str = r#"---
name: tenet
description: Use when the user explicitly asks to use Tenet, invokes the Tenet workflow, or requests implementation or completion under this repository's Tenet contract.
compatibility: Requires the tenet CLI and Git.
metadata:
  tenet-skill-version: "1"
---

# Tenet workflow

Tenet determines whether an exact Git revision satisfies this repository's admitted completion contract. It does not perform the engineering.

1. Run `tenet status --json` and use only its current repository state.
2. If the contract is missing, inspect the configured specification, obtain the proposal shape with `tenet contract schema --json`, and submit it with `tenet contract propose --file <path> --json`.
3. When approval is required, report the proposal ID and digest, stop, and ask the user to perform operator admission. Never invoke `tenet contract approve` yourself.
4. Perform the engineering with your normal tools and workflow.
5. Produce an immutable candidate commit and run `tenet gate --revision <sha> --json`.
6. If the verdict is `not_done`, `inconclusive`, or `infrastructure_error`, inspect the typed blockers and `tenet evidence --revision <sha> --json`, then continue or report the blocker as appropriate.
7. Declare completion only when Tenet returns `done` for the exact revision you report.

The CLI is the correctness boundary. Skill discovery and invocation syntax depend on the calling runtime and are optional convenience.
"#;

pub fn discover_root(cwd: &Path) -> Result<PathBuf> {
  let output = Command::new("git")
    .args(["rev-parse", "--show-toplevel"])
    .current_dir(cwd)
    .output()
    .context("execute git repository discovery")?;
  if !output.status.success() {
    bail!(
      "not a Git repository: {}",
      String::from_utf8_lossy(&output.stderr).trim()
    );
  }
  let root = String::from_utf8(output.stdout).context("Git root is not UTF-8")?;
  Ok(PathBuf::from(root.trim()))
}

pub fn initialize(root: &Path, spec: &Path) -> Result<(RepositoryConfig, String, bool)> {
  let spec = if spec.is_absolute() {
    spec.to_path_buf()
  } else {
    root.join(spec)
  };
  let canonical_root = root
    .canonicalize()
    .context("canonicalize repository root")?;
  let canonical_spec = spec
    .canonicalize()
    .with_context(|| format!("read specification {}", spec.display()))?;
  let relative = canonical_spec
    .strip_prefix(&canonical_root)
    .context("specification must be inside the Git repository")?;
  let spec_path = relative
    .to_str()
    .context("specification path must be UTF-8")?
    .replace('\\', "/");

  fs::create_dir_all(root.join(TENET_DIR)).context("create .tenet directory")?;
  fs::create_dir_all(root.join(".agents/skills/tenet")).context("create Tenet Skill directory")?;

  let config_path = root.join(CONFIG_PATH);
  let created = !config_path.exists();
  let config = if created {
    let config = RepositoryConfig {
      version: 1,
      spec_path,
      verifiers: Vec::new(),
    };
    atomic_write(&config_path, toml::to_string_pretty(&config)?.as_bytes())?;
    config
  } else {
    let existing = load_policy(root)?;
    if existing.spec_path != spec_path {
      bail!(
        "repository is already initialized for specification `{}`",
        existing.spec_path
      );
    }
    existing
  };

  atomic_write(
    &root.join(TENET_DIR).join(".gitignore"),
    b"state.json\nproposals/\n",
  )?;
  atomic_write(&root.join(SKILL_PATH), SKILL.as_bytes())?;
  let spec_digest = specification_digest(root, &config)?;
  Ok((config, spec_digest, created))
}

pub fn load_policy(root: &Path) -> Result<VerificationPolicy> {
  let path = root.join(CONFIG_PATH);
  let text = fs::read_to_string(&path)
    .with_context(|| format!("read repository policy {}", path.display()))?;
  let policy: VerificationPolicy = toml::from_str(&text).context("parse repository policy")?;
  validate_policy(&policy).context("validate repository policy")?;
  Ok(policy)
}

pub fn specification_digest(root: &Path, policy: &VerificationPolicy) -> Result<String> {
  let bytes = fs::read(root.join(&policy.spec_path))
    .with_context(|| format!("read specification `{}`", policy.spec_path))?;
  Ok(bytes_digest(&bytes))
}

pub fn policy_digest(policy: &VerificationPolicy) -> Result<String> {
  canonical_digest(policy).context("hash verification policy")
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
  let parent = path.parent().context("output path has no parent")?;
  fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
  let temporary = parent.join(format!(
    ".{}.tmp-{}",
    path
      .file_name()
      .and_then(|item| item.to_str())
      .unwrap_or("tenet"),
    std::process::id()
  ));
  fs::write(&temporary, bytes).with_context(|| format!("write {}", temporary.display()))?;
  fs::rename(&temporary, path).with_context(|| format!("replace {}", path.display()))?;
  Ok(())
}

pub fn resolve_revision(root: &Path, revision: &str) -> Result<String> {
  let revision = revision.trim();
  if !matches!(revision.len(), 40 | 64) || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    bail!("revision must be a full immutable Git commit ID");
  }
  let expression = format!("{revision}^{{commit}}");
  let output = Command::new("git")
    .args(["rev-parse", "--verify", "--end-of-options", &expression])
    .current_dir(root)
    .output()
    .context("resolve Git revision")?;
  if !output.status.success() {
    bail!(
      "revision is not an exact commit: {}",
      String::from_utf8_lossy(&output.stderr).trim()
    );
  }
  let resolved = String::from_utf8(output.stdout)?.trim().to_owned();
  if !resolved.eq_ignore_ascii_case(revision) {
    bail!("revision identifies an annotated object, not the exact commit ID");
  }
  Ok(resolved)
}

pub struct MaterializedRevision {
  root: PathBuf,
  checkout: PathBuf,
  _temporary: tempfile::TempDir,
}

impl MaterializedRevision {
  pub fn create(root: &Path, revision: &str) -> Result<Self> {
    let temporary = tempfile::Builder::new().prefix("tenet-gate-").tempdir()?;
    let checkout = temporary.path().join("revision");
    let output = Command::new("git")
      .args(["worktree", "add", "--detach", "--force"])
      .arg(&checkout)
      .arg(revision)
      .current_dir(root)
      .output()
      .context("materialize exact revision")?;
    if !output.status.success() {
      bail!(
        "materialize exact revision: {}",
        String::from_utf8_lossy(&output.stderr).trim()
      );
    }
    Ok(Self {
      root: root.to_path_buf(),
      checkout,
      _temporary: temporary,
    })
  }

  pub fn path(&self) -> &Path {
    &self.checkout
  }
}

impl Drop for MaterializedRevision {
  fn drop(&mut self) {
    let _ = Command::new("git")
      .args(["worktree", "remove", "--force"])
      .arg(&self.checkout)
      .current_dir(&self.root)
      .output();
  }
}
