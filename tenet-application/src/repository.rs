use std::{
  collections::BTreeSet,
  fs::{self, OpenOptions},
  io::{BufRead, BufReader, ErrorKind, Read, Write},
  path::{Component, Path, PathBuf},
  process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use tenet_domain::{
  digest::{bytes_digest, canonical_digest},
  policy::{RepositoryConfig, VerificationPolicy, validate_policy},
};

pub const TENET_DIR: &str = ".tenet";
pub const CONFIG_PATH: &str = ".tenet/tenet.toml";
pub const CONTRACT_PATH: &str = ".tenet/contract.json";
pub const STATE_PATH: &str = ".tenet/state.json";
pub const SKILL_PATH: &str = ".agents/skills/tenet/SKILL.md";
pub const MCP_CONFIG_PATH: &str = ".mcp.json";

const DEFAULT_SPECIFICATION: &str = "# Tenet completion specification\n\nDescribe the required behavior and acceptance criteria for this repository.\n";

pub const SKILL: &str = r#"---
name: tenet
description: Use when the user asks to use Tenet or completion is governed by a Tenet contract.
compatibility: Requires Tenet MCP tools and Git.
metadata:
  tenet-skill-version: "1"
---

# Tenet workflow

Tenet judges exact candidate revision R under independently selected authority revision A. The coding agent owns engineering; Tenet owns completion authority. `tenet init` is the only user-facing CLI workflow; after initialization, interact with Tenet only through its semantic MCP tools.

1. Inspect `tenet_status` before relying on a completion condition. Status never runs verifiers.
2. Inspect `tenet_policy_schema` before changing verification policy. Configure only supported fields; never infer hidden capabilities or reverse-engineer the binary.
3. If no contract is admitted, inspect the specification and policy. Preserve every explicit acceptance criterion and Definition of Done item.
4. Map each material obligation only to a verifier that actually observes it. Add small deterministic policy verifiers or authority-owned oracle assets when needed; do not modify product code to accommodate a verifier before authority admission.
5. Obtain the proposal shape from `tenet_contract_schema`, reference verifiers only by ID, then submit the complete typed proposal with `tenet_contract_propose`. Never supply or infer verifier authority.
6. On `pending_approval`, present the exact canonical proposal returned by Tenet, without reconstructing it from agent memory; its proposal ID and digest; every primary and oracle-assurance verifier mapping; the complete verification profile; and every Tenet warning.
7. Ask whether the human explicitly approves that exact proposal ID and digest after reviewing the canonical proposal, verifier mappings, verification profile, and warnings. Never self-approve or infer approval from silence, continuation, generic acknowledgement, or an earlier proposal.
8. Only after explicit approval, call `tenet_contract_approve` with that exact proposal ID and digest. If content, specification, policy, ID, or digest changed, discard the earlier approval and request fresh approval.
9. Freeze the admitted specification, policy, contract, and authority-owned oracle assets in a commit. Present its full SHA and ask the operator to select it explicitly as authority A. Never choose or advance A yourself.
10. Implement and debug normally without changing A's authority surface. Produce immutable candidate commit R descended from A.
11. Call `tenet_gate` with both exact full revisions: `authorityRevision = A` and `revision = R`. Never infer either revision from HEAD, state, or prior calls.
12. Treat `not_done`, contradiction, inconclusive evidence, and infrastructure failure according to their typed blockers. Inspect `tenet_evidence` for exact R and continue engineering; never weaken authority to obtain `done`.
13. Claim completion only when `tenet_gate` returns `done` for the exact reported (A, R) pair.

Proposal admission and operator selection of A are explicit human trust decisions, not security sandboxes.
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
  let config_path = root.join(CONFIG_PATH);
  let created = !config_path.exists();
  let existing = (!created).then(|| load_policy(root)).transpose()?;
  let canonical_spec = match spec.canonicalize() {
    Ok(path) => path,
    Err(error) if error.kind() == ErrorKind::NotFound => {
      let requested_spec_path = specification_path(&canonical_root, &spec)?;
      if let Some(existing) = &existing
        && existing.spec_path != requested_spec_path
      {
        bail!(
          "repository is already initialized for specification `{}`",
          existing.spec_path
        );
      }
      create_default_specification(&canonical_root, &spec)?;
      spec
        .canonicalize()
        .with_context(|| format!("canonicalize created specification {}", spec.display()))?
    }
    Err(error) => {
      return Err(error).with_context(|| format!("read specification {}", spec.display()));
    }
  };
  let spec_path = specification_path(&canonical_root, &canonical_spec)?;

  initialize_mcp_configuration(root)?;
  fs::create_dir_all(root.join(TENET_DIR)).context("create .tenet directory")?;
  fs::create_dir_all(root.join(".agents/skills/tenet")).context("create Tenet Skill directory")?;

  let config = if created {
    let config = RepositoryConfig {
      version: 1,
      spec_path,
      verifiers: Vec::new(),
    };
    atomic_write(&config_path, toml::to_string_pretty(&config)?.as_bytes())?;
    config
  } else {
    let existing = existing.context("load existing repository policy")?;
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
fn initialize_mcp_configuration(root: &Path) -> Result<()> {
  let path = root.join(MCP_CONFIG_PATH);
  let tenet = serde_json::json!({
    "command": "tenet",
    "args": ["mcp"]
  });

  let mut configuration = match fs::read(&path) {
    Ok(bytes) => serde_json::from_slice(&bytes)
      .with_context(|| format!("parse MCP configuration {}", path.display()))?,
    Err(error) if error.kind() == ErrorKind::NotFound => serde_json::json!({}),
    Err(error) => {
      return Err(error).with_context(|| format!("read MCP configuration {}", path.display()));
    }
  };

  let configuration = configuration
    .as_object_mut()
    .context("MCP configuration must be a JSON object")?;
  let servers = configuration
    .entry("mcpServers")
    .or_insert_with(|| serde_json::json!({}))
    .as_object_mut()
    .context("MCP configuration field `mcpServers` must be a JSON object")?;

  match servers.get("tenet") {
    Some(server) if server == &tenet => return Ok(()),
    Some(_) => bail!("conflicting `tenet` MCP server entry in {}", path.display()),
    None => {
      servers.insert("tenet".into(), tenet);
    }
  }

  let encoded = serde_json::to_string_pretty(&configuration)?;
  atomic_write(&path, format!("{encoded}\n").as_bytes())
}

fn specification_path(canonical_root: &Path, spec: &Path) -> Result<String> {
  let relative = spec
    .strip_prefix(canonical_root)
    .context("specification must be inside the Git repository")?;
  let mut normalized = PathBuf::new();
  for component in relative.components() {
    match component {
      Component::CurDir => {}
      Component::Normal(component) => normalized.push(component),
      Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
        bail!("specification must be inside the Git repository")
      }
    }
  }
  if normalized.as_os_str().is_empty() {
    bail!("specification must be a file inside the Git repository");
  }
  normalized
    .to_str()
    .context("specification path must be UTF-8")
    .map(|path| path.replace('\\', "/"))
}

fn create_default_specification(canonical_root: &Path, spec: &Path) -> Result<()> {
  let relative = PathBuf::from(specification_path(canonical_root, spec)?);
  let mut components = relative.components().peekable();
  let mut directory = canonical_root.to_path_buf();
  let file_name = loop {
    match components.next() {
      Some(Component::CurDir) => continue,
      Some(Component::Normal(component)) if components.peek().is_some() => {
        directory.push(component);
        if directory.exists() {
          directory = directory.canonicalize().with_context(|| {
            format!(
              "canonicalize specification directory {}",
              directory.display()
            )
          })?;
          if !directory.starts_with(canonical_root) {
            bail!("specification must be inside the Git repository");
          }
        } else {
          fs::create_dir(&directory)
            .with_context(|| format!("create specification directory {}", directory.display()))?;
        }
      }
      Some(Component::Normal(component)) => break component,
      Some(_) | None => bail!("specification must be a file inside the Git repository"),
    }
  };
  atomic_write(&directory.join(file_name), DEFAULT_SPECIFICATION.as_bytes())
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
    .args([
      "--no-replace-objects",
      "rev-parse",
      "--verify",
      "--end-of-options",
      &expression,
    ])
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

pub fn is_ancestor(root: &Path, ancestor: &str, revision: &str) -> Result<bool> {
  let status = Command::new("git")
    .args([
      "--no-replace-objects",
      "merge-base",
      "--is-ancestor",
      ancestor,
      revision,
    ])
    .current_dir(root)
    .status()
    .context("check authority revision ancestry")?;
  match status.code() {
    Some(0) => Ok(true),
    Some(1) => Ok(false),
    _ => bail!("Git could not determine authority revision ancestry"),
  }
}

pub fn read_revision_file(root: &Path, revision: &str, path: &str) -> Result<Option<Vec<u8>>> {
  let entry = Command::new("git")
    .args([
      "--no-replace-objects",
      "--literal-pathspecs",
      "ls-tree",
      "-z",
      "--full-tree",
      revision,
      "--",
      path,
    ])
    .current_dir(root)
    .output()
    .with_context(|| format!("inspect `{path}` at authority revision"))?;
  if !entry.status.success() {
    bail!(
      "inspect `{path}` at authority revision: {}",
      String::from_utf8_lossy(&entry.stderr).trim()
    );
  }
  if entry.stdout.is_empty() {
    return Ok(None);
  }

  let object = format!("{revision}:{path}");
  let output = Command::new("git")
    .args([
      "--no-replace-objects",
      "show",
      "--no-ext-diff",
      "--no-textconv",
      &object,
    ])
    .current_dir(root)
    .output()
    .with_context(|| format!("read `{path}` at authority revision"))?;
  if !output.status.success() {
    bail!(
      "read `{path}` at authority revision: {}",
      String::from_utf8_lossy(&output.stderr).trim()
    );
  }
  Ok(Some(output.stdout))
}

pub fn revision_directory_object(root: &Path, revision: &str, path: &str) -> Result<String> {
  let output = Command::new("git")
    .args([
      "--no-replace-objects",
      "--literal-pathspecs",
      "ls-tree",
      "-z",
      "--full-tree",
      revision,
      "--",
      path,
    ])
    .current_dir(root)
    .output()
    .with_context(|| format!("inspect authority oracle bundle `{path}`"))?;
  if !output.status.success() {
    bail!(
      "inspect authority oracle bundle `{path}`: {}",
      String::from_utf8_lossy(&output.stderr).trim()
    );
  }
  let record = output
    .stdout
    .strip_suffix(&[0])
    .with_context(|| format!("authority oracle bundle `{path}` does not exist"))?;
  if record.contains(&0) {
    bail!("authority oracle bundle `{path}` is ambiguous");
  }
  let entry = parse_tree_entry(record)?;
  if entry.path != Path::new(path) || entry.mode != "040000" || entry.kind != "tree" {
    bail!("authority oracle bundle `{path}` must be a Git tree");
  }
  Ok(entry.object)
}

pub fn revision_executable_object(root: &Path, revision: &str, path: &str) -> Result<String> {
  let output = Command::new("git")
    .args([
      "--no-replace-objects",
      "--literal-pathspecs",
      "ls-tree",
      "-z",
      "--full-tree",
      revision,
      "--",
      path,
    ])
    .current_dir(root)
    .output()
    .with_context(|| format!("inspect authority oracle executable `{path}`"))?;
  if !output.status.success() {
    bail!(
      "inspect authority oracle executable `{path}`: {}",
      String::from_utf8_lossy(&output.stderr).trim()
    );
  }
  let record = output
    .stdout
    .strip_suffix(&[0])
    .with_context(|| format!("authority oracle executable `{path}` does not exist"))?;
  if record.contains(&0) {
    bail!("authority oracle executable `{path}` is ambiguous");
  }
  let entry = parse_tree_entry(record)?;
  if entry.path != Path::new(path) || entry.mode != "100755" || entry.kind != "blob" {
    bail!("authority oracle executable `{path}` must be an executable Git file");
  }
  Ok(entry.object)
}

pub fn changed_paths(
  root: &Path,
  authority_revision: &str,
  candidate_revision: &str,
  paths: &[&str],
) -> Result<Vec<String>> {
  let mut changed = Vec::new();
  for path in paths {
    let status = Command::new("git")
      .args([
        "--literal-pathspecs",
        "--no-replace-objects",
        "diff",
        "--quiet",
        "--no-ext-diff",
        authority_revision,
        candidate_revision,
        "--",
        path,
      ])
      .current_dir(root)
      .status()
      .with_context(|| format!("compare authority-owned path `{path}`"))?;
    match status.code() {
      Some(0) => {}
      Some(1) => changed.push((*path).to_owned()),
      _ => bail!("Git could not compare authority-owned path `{path}`"),
    }
  }
  Ok(changed)
}

const MAX_TREE_ENTRIES: usize = 1_000_000;
const MAX_TREE_RECORD_BYTES: usize = 1024 * 1024;
const MAX_DIRECTORY_PATH_BYTES: usize = 64 * 1024 * 1024;
const MAX_SYMLINK_TARGET_BYTES: u64 = 4096;

struct TreeEntry {
  mode: String,
  kind: String,
  object: String,
  path: PathBuf,
}

fn materialize_raw_tree(root: &Path, revision: &str, checkout: &Path) -> Result<()> {
  let mut directories = BTreeSet::new();
  let mut directory_path_bytes = 0_usize;
  visit_tree(root, revision, |entry| {
    let mut current = PathBuf::new();
    let mut components = entry.path.components().peekable();
    while let Some(component) = components.next() {
      if components.peek().is_none() {
        break;
      }
      current.push(component.as_os_str());
      create_directory(
        checkout,
        &current,
        &mut directories,
        &mut directory_path_bytes,
      )?;
    }
    match (entry.mode.as_str(), entry.kind.as_str()) {
      ("100644" | "100755" | "120000", "blob") => Ok(()),
      ("160000", "commit") => create_directory(
        checkout,
        &entry.path,
        &mut directories,
        &mut directory_path_bytes,
      ),
      (mode, kind) => {
        bail!("unsupported candidate tree entry mode `{mode}` and type `{kind}`")
      }
    }
  })?;

  let mut child = Command::new("git")
    .args(["--no-replace-objects", "cat-file", "--batch"])
    .current_dir(root)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .context("start candidate blob reader")?;
  let mut requests = child.stdin.take().context("open candidate blob input")?;
  let mut responses = BufReader::new(child.stdout.take().context("open candidate blob output")?);
  visit_tree(root, revision, |entry| {
    if entry.kind == "commit" {
      return Ok(());
    }
    writeln!(requests, "{}", entry.object)?;
    requests.flush()?;
    let mut header = String::new();
    responses.read_line(&mut header)?;
    let mut fields = header.split_whitespace();
    let object = fields
      .next()
      .context("candidate blob response has no object ID")?;
    let kind = fields
      .next()
      .context("candidate blob response has no type")?;
    let size: u64 = fields
      .next()
      .context("candidate blob response has no size")?
      .parse()
      .context("candidate blob response has invalid size")?;
    if object != entry.object || kind != "blob" || fields.next().is_some() {
      bail!("candidate blob response does not match the requested object");
    }
    write_blob(&mut responses, size, checkout, &entry)?;
    let mut terminator = [0_u8; 1];
    responses.read_exact(&mut terminator)?;
    if terminator != *b"\n" {
      bail!("candidate blob response has an invalid terminator");
    }
    Ok(())
  })?;
  drop(requests);
  let status = child.wait().context("finish candidate blob reader")?;
  if !status.success() {
    let mut stderr = String::new();
    if let Some(mut stream) = child.stderr.take() {
      stream.read_to_string(&mut stderr)?;
    }
    bail!("read candidate blobs: {}", stderr.trim());
  }
  Ok(())
}

fn visit_tree(
  root: &Path,
  revision: &str,
  mut visit: impl FnMut(TreeEntry) -> Result<()>,
) -> Result<()> {
  let mut child = Command::new("git")
    .args([
      "--no-replace-objects",
      "ls-tree",
      "-r",
      "-z",
      "--full-tree",
      revision,
    ])
    .current_dir(root)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .context("enumerate candidate tree")?;
  let mut output = BufReader::new(child.stdout.take().context("open candidate tree output")?);
  let mut count = 0;
  loop {
    let mut record = Vec::new();
    let read = output
      .by_ref()
      .take((MAX_TREE_RECORD_BYTES + 1) as u64)
      .read_until(0, &mut record)?;
    if read == 0 {
      break;
    }
    if record.last() != Some(&0) {
      bail!("candidate tree entry exceeds the materialization limit");
    }
    record.pop();
    count += 1;
    if count > MAX_TREE_ENTRIES {
      bail!("candidate tree exceeds the materialization entry limit");
    }
    visit(parse_tree_entry(&record)?)?;
  }
  let status = child.wait().context("finish candidate tree enumeration")?;
  if !status.success() {
    let mut stderr = String::new();
    if let Some(mut stream) = child.stderr.take() {
      stream.read_to_string(&mut stderr)?;
    }
    bail!("enumerate candidate tree: {}", stderr.trim());
  }
  Ok(())
}

fn parse_tree_entry(record: &[u8]) -> Result<TreeEntry> {
  let separator = record
    .iter()
    .position(|byte| *byte == b'\t')
    .context("candidate tree contains a malformed entry")?;
  let metadata = std::str::from_utf8(&record[..separator])?;
  let mut fields = metadata.split_whitespace();
  let mode = fields.next().context("candidate tree entry has no mode")?;
  let kind = fields.next().context("candidate tree entry has no type")?;
  let object = fields
    .next()
    .context("candidate tree entry has no object ID")?;
  if fields.next().is_some() {
    bail!("candidate tree entry has unexpected metadata");
  }
  let path = git_path(record[separator + 1..].to_vec())?;
  validate_materialized_path(&path)?;
  Ok(TreeEntry {
    mode: mode.to_owned(),
    kind: kind.to_owned(),
    object: object.to_owned(),
    path,
  })
}

fn create_directory(
  checkout: &Path,
  path: &Path,
  directories: &mut BTreeSet<PathBuf>,
  total_path_bytes: &mut usize,
) -> Result<()> {
  if directories.insert(path.to_path_buf()) {
    *total_path_bytes = total_path_bytes
      .checked_add(path.as_os_str().as_encoded_bytes().len())
      .context("candidate directory metadata size overflow")?;
    if *total_path_bytes > MAX_DIRECTORY_PATH_BYTES {
      bail!("candidate directory metadata exceeds the materialization limit");
    }
    fs::create_dir(checkout.join(path)).with_context(|| {
      format!(
        "create unique candidate directory {}",
        checkout.join(path).display()
      )
    })?;
  }
  Ok(())
}

fn validate_materialized_path(path: &Path) -> Result<()> {
  let mut components = path.components();
  let Some(Component::Normal(first)) = components.next() else {
    bail!("candidate tree contains an invalid path");
  };
  if first.to_string_lossy().eq_ignore_ascii_case(".git")
    || components.any(|component| !matches!(component, Component::Normal(_)))
  {
    bail!(
      "candidate tree path `{}` is unsafe to materialize",
      path.display()
    );
  }
  Ok(())
}

fn write_blob(
  responses: &mut impl Read,
  size: u64,
  checkout: &Path,
  entry: &TreeEntry,
) -> Result<()> {
  let destination = checkout.join(&entry.path);
  if entry.mode == "120000" {
    if size > MAX_SYMLINK_TARGET_BYTES {
      bail!("candidate symlink target exceeds the materialization limit");
    }
    let mut target = vec![0_u8; size as usize];
    responses.read_exact(&mut target)?;
    create_symlink(&target, &destination)?;
    return Ok(());
  }

  let mut file = OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(&destination)
    .with_context(|| format!("create unique candidate file {}", destination.display()))?;
  let copied = std::io::copy(&mut responses.take(size), &mut file)?;
  if copied != size {
    bail!("candidate blob `{}` ended unexpectedly", entry.object);
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let mode = if entry.mode == "100755" { 0o755 } else { 0o644 };
    file.set_permissions(fs::Permissions::from_mode(mode))?;
  }
  Ok(())
}

#[cfg(unix)]
fn git_path(bytes: Vec<u8>) -> Result<PathBuf> {
  use std::os::unix::ffi::OsStringExt;
  Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(not(unix))]
fn git_path(bytes: Vec<u8>) -> Result<PathBuf> {
  Ok(PathBuf::from(String::from_utf8(bytes)?))
}

#[cfg(unix)]
fn create_symlink(target: &[u8], destination: &Path) -> Result<()> {
  use std::os::unix::{ffi::OsStrExt, fs::symlink};
  symlink(std::ffi::OsStr::from_bytes(target), destination)
    .with_context(|| format!("create unique candidate symlink {}", destination.display()))
}

#[cfg(not(unix))]
fn create_symlink(_target: &[u8], destination: &Path) -> Result<()> {
  bail!(
    "candidate symlink `{}` cannot be materialized on this platform",
    destination.display()
  )
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
      .args([
        "--no-replace-objects",
        "worktree",
        "add",
        "--no-checkout",
        "--detach",
        "--force",
      ])
      .arg(&checkout)
      .arg(revision)
      .current_dir(root)
      .output()
      .context("prepare exact candidate materialization")?;
    if !output.status.success() {
      bail!(
        "prepare exact candidate materialization: {}",
        String::from_utf8_lossy(&output.stderr).trim()
      );
    }
    let materialized = Self {
      root: root.to_path_buf(),
      checkout,
      _temporary: temporary,
    };
    materialize_raw_tree(root, revision, materialized.path())?;
    Ok(materialized)
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
