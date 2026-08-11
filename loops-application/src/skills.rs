use std::{
  collections::BTreeSet,
  path::{Path, PathBuf},
  sync::{Mutex, OnceLock},
  time::Duration,
};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use include_dir::{include_dir, Dir};
use rusqlite::{backup::Backup, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs;

use loops_domain::{config::SkillsConfig, model::WorkerRole};

static BUILT_IN_SKILLS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../skills");
static MATERIALIZE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
  BuiltIn,
  User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSpec {
  /// Built-in skill name or user-configured project-relative path.
  pub name: String,
  pub path: PathBuf,
  pub source: SkillSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedSkills {
  pub skills: Vec<SkillSpec>,
}
impl ResolvedSkills {
  pub fn names(&self) -> Vec<String> {
    self.skills.iter().map(|skill| skill.name.clone()).collect()
  }

  /// Skill directory names used by OMP's `--skills` filter.
  pub fn omp_names(&self) -> Result<Vec<String>> {
    self.skills.iter().map(skill_directory_name).collect()
  }
}

pub fn resolve(cwd: &Path, config: &SkillsConfig, role: WorkerRole) -> Result<ResolvedSkills> {
  let mut skills = vec![built_in_skill(role)?];
  for configured_path in config.shared.iter().chain(config.role_paths(role)) {
    skills.push(user_skill(cwd, configured_path, role)?);
  }

  let mut seen = BTreeSet::new();
  skills.retain(|skill| seen.insert(skill.name.clone()));
  Ok(ResolvedSkills { skills })
}

pub async fn prepare_worker_environment(
  runtime_dir: &Path,
  role: WorkerRole,
  resolved: &ResolvedSkills,
  global_agent_dir: &Path,
) -> Result<PathBuf> {
  let worker_dir = runtime_dir.join(role.as_str());
  let skills_dir = worker_dir.join("skills");
  if skills_dir.exists() {
    fs::remove_dir_all(&skills_dir).await?;
  }

  let mut directories = BTreeSet::new();
  for skill in &resolved.skills {
    let directory = skill_directory_name(skill)?;
    if !directories.insert(directory.clone()) {
      bail!(
        "{} worker cannot isolate skills with duplicate directory name `{directory}`",
        role.as_str()
      );
    }
    let target = skills_dir.join(&directory);
    let source = skill.path.clone();
    tokio::task::spawn_blocking(move || copy_skill_directory(&source, &target))
      .await
      .context("wait for skill directory copy")?
      .with_context(|| format!("load {} worker skill `{}`", role.as_str(), skill.name))?;
  }
  snapshot_global_agent_database(global_agent_dir, &worker_dir).await?;
  Ok(worker_dir)
}

async fn snapshot_global_agent_database(global_agent_dir: &Path, worker_dir: &Path) -> Result<()> {
  let source = global_agent_dir.join("agent.db");
  if !source.is_file() {
    return Ok(());
  }
  let target = worker_dir.join("agent.db");
  tokio::task::spawn_blocking(move || snapshot_database(&source, &target))
    .await
    .context("wait for global OMP credential snapshot")??;
  Ok(())
}

fn snapshot_database(source: &Path, target: &Path) -> Result<()> {
  let source = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)
    .context("open global OMP agent database")?;
  let mut target = Connection::open(target).context("open worker OMP agent database")?;
  let backup = Backup::new(&source, &mut target).context("copy global OMP agent database")?;
  backup
    .run_to_completion(5, Duration::from_millis(50), None)
    .context("finish global OMP agent database copy")?;
  Ok(())
}

fn built_in_skill(role: WorkerRole) -> Result<SkillSpec> {
  let name = match role {
    WorkerRole::Architect | WorkerRole::Reconcile => "spec-analysis",
    WorkerRole::Implement => "implementation",
    WorkerRole::Repair => "debugging",
    WorkerRole::Assess => "spec-assessment",
  };
  let contents = BUILT_IN_SKILLS
    .get_dir(name)
    .with_context(|| format!("embedded built-in skill `{name}` is missing"))?;
  Ok(SkillSpec {
    name: name.into(),
    path: materialize(name, contents)?,
    source: SkillSource::BuiltIn,
  })
}

fn user_skill(cwd: &Path, configured_path: &str, role: WorkerRole) -> Result<SkillSpec> {
  let root = Path::new(configured_path);
  let root = if root.is_absolute() {
    root.to_path_buf()
  } else {
    cwd.join(root)
  };
  let path = root.join("SKILL.md");
  if !path.is_file() {
    bail!(
      "{} worker configured invalid skill path `{configured_path}`: expected {}",
      role.as_str(),
      path.display()
    );
  }
  Ok(SkillSpec {
    name: configured_path.into(),
    path: root,
    source: SkillSource::User,
  })
}

fn skill_directory_name(skill: &SkillSpec) -> Result<String> {
  skill
    .path
    .file_name()
    .and_then(|name| name.to_str())
    .map(str::to_owned)
    .context("skill directory must have a valid UTF-8 name")
}

fn materialize(name: &str, contents: &Dir<'_>) -> Result<PathBuf> {
  let _guard = MATERIALIZE_LOCK
    .get_or_init(|| Mutex::new(()))
    .lock()
    .map_err(|_| anyhow::anyhow!("built-in skill materialization lock is poisoned"))?;
  let mut hasher = Sha256::new();
  hash_embedded_directory(contents, &mut hasher);
  let path = std::env::temp_dir()
    .join("loops-built-in-skills")
    .join(URL_SAFE_NO_PAD.encode(hasher.finalize()))
    .join(name);
  if !path.is_dir() {
    write_embedded_directory(contents, &path)?;
  }
  Ok(path)
}

fn hash_embedded_directory(source: &Dir<'_>, hasher: &mut Sha256) {
  for file in source.files() {
    hasher.update(file.path().to_string_lossy().as_bytes());
    hasher.update(file.contents());
  }
  for directory in source.dirs() {
    hasher.update(directory.path().to_string_lossy().as_bytes());
    hash_embedded_directory(directory, hasher);
  }
}

fn write_embedded_directory(source: &Dir<'_>, target: &Path) -> Result<()> {
  std::fs::create_dir_all(target)?;
  for file in source.files() {
    let name = file
      .path()
      .file_name()
      .context("embedded skill file has no name")?;
    std::fs::write(target.join(name), file.contents())?;
  }
  for directory in source.dirs() {
    let name = directory
      .path()
      .file_name()
      .context("embedded skill directory has no name")?;
    write_embedded_directory(directory, &target.join(name))?;
  }
  Ok(())
}

fn copy_skill_directory(source: &Path, target: &Path) -> Result<()> {
  std::fs::create_dir_all(target)?;
  for entry in std::fs::read_dir(source)? {
    let entry = entry?;
    let source_path = entry.path();
    let target_path = target.join(entry.file_name());
    let file_type = entry.file_type()?;
    if file_type.is_dir() {
      copy_skill_directory(&source_path, &target_path)?;
    } else if file_type.is_file() {
      std::fs::copy(&source_path, &target_path)?;
    } else {
      bail!(
        "skill contains unsupported non-file entry {}",
        source_path.display()
      );
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use rusqlite::Connection;
  use std::collections::BTreeMap;

  use tempfile::tempdir;

  use super::{prepare_worker_environment, resolve};
  use loops_domain::{config::SkillsConfig, model::WorkerRole};

  #[test]
  fn defaults_are_only_role_specific_built_ins_even_for_rust_and_python_projects() {
    let project = tempdir().unwrap();
    std::fs::write(
      project.path().join("Cargo.toml"),
      "[package]\nname='fixture'\nversion='0.1.0'\n",
    )
    .unwrap();
    std::fs::write(
      project.path().join("pyproject.toml"),
      "[project]\nname='fixture'\n",
    )
    .unwrap();
    let global = project.path().join(".omp/skills/unconfigured-global");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::write(global.join("SKILL.md"), "# Unconfigured").unwrap();
    let config = SkillsConfig::default();

    assert_eq!(
      resolve(project.path(), &config, WorkerRole::Architect)
        .unwrap()
        .names(),
      ["spec-analysis"]
    );
    assert_eq!(
      resolve(project.path(), &config, WorkerRole::Implement)
        .unwrap()
        .names(),
      ["implementation"]
    );
    assert_eq!(
      resolve(project.path(), &config, WorkerRole::Repair)
        .unwrap()
        .names(),
      ["debugging"]
    );
    assert_eq!(
      resolve(project.path(), &config, WorkerRole::Assess)
        .unwrap()
        .names(),
      ["spec-assessment"]
    );
  }

  #[test]
  fn shared_and_role_skills_are_explicit_and_scoped() {
    let project = tempdir().unwrap();
    for name in ["project", "rust"] {
      let directory = project.path().join(".loops/skills").join(name);
      std::fs::create_dir_all(&directory).unwrap();
      std::fs::write(directory.join("SKILL.md"), "# User skill").unwrap();
    }
    let config = SkillsConfig {
      shared: vec![".loops/skills/project".into()],
      roles: BTreeMap::from([("implement".into(), vec![".loops/skills/rust".into()])]),
    };

    assert_eq!(
      resolve(project.path(), &config, WorkerRole::Architect)
        .unwrap()
        .names(),
      ["spec-analysis", ".loops/skills/project"]
    );
    assert_eq!(
      resolve(project.path(), &config, WorkerRole::Implement)
        .unwrap()
        .names(),
      [
        "implementation",
        ".loops/skills/project",
        ".loops/skills/rust"
      ]
    );
    assert_eq!(
      resolve(project.path(), &config, WorkerRole::Implement)
        .unwrap()
        .omp_names()
        .unwrap(),
      ["implementation", "project", "rust"]
    );
    assert_eq!(
      resolve(project.path(), &config, WorkerRole::Repair)
        .unwrap()
        .names(),
      ["debugging", ".loops/skills/project"]
    );
    assert_eq!(
      resolve(project.path(), &config, WorkerRole::Assess)
        .unwrap()
        .names(),
      ["spec-assessment", ".loops/skills/project"]
    );
  }

  #[test]
  fn invalid_user_skill_path_identifies_role_and_path() {
    let project = tempdir().unwrap();
    let config = SkillsConfig {
      shared: Vec::new(),
      roles: BTreeMap::from([("repair".into(), vec![".loops/skills/missing".into()])]),
    };
    let error = resolve(project.path(), &config, WorkerRole::Repair).unwrap_err();
    assert!(error.to_string().contains("repair worker"));
    assert!(error.to_string().contains(".loops/skills/missing"));
  }

  #[tokio::test]
  async fn isolated_environment_contains_exactly_resolved_skills() {
    let project = tempdir().unwrap();
    for name in ["project", "rust"] {
      let directory = project.path().join(".loops/skills").join(name);
      std::fs::create_dir_all(&directory).unwrap();
      std::fs::write(directory.join("SKILL.md"), "# User skill").unwrap();
    }
    let config = SkillsConfig {
      shared: vec![".loops/skills/project".into()],
      roles: BTreeMap::from([("implement".into(), vec![".loops/skills/rust".into()])]),
    };
    let project_references = project.path().join(".loops/skills/project/references");
    std::fs::create_dir_all(&project_references).unwrap();
    std::fs::write(project_references.join("guide.md"), "# Project guide").unwrap();
    let resolved = resolve(project.path(), &config, WorkerRole::Implement).unwrap();
    let worker = prepare_worker_environment(
      project.path(),
      WorkerRole::Implement,
      &resolved,
      &project.path().join("global-agent"),
    )
    .await
    .unwrap();
    let mut names = std::fs::read_dir(worker.join("skills"))
      .unwrap()
      .map(|entry| entry.unwrap().file_name().into_string().unwrap())
      .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["implementation", "project", "rust"]);
    assert_eq!(
      std::fs::read_to_string(worker.join("skills/project/references/guide.md")).unwrap(),
      "# Project guide"
    );
    assert_eq!(
      std::fs::read_to_string(worker.join("skills/implementation/references/quality.md")).unwrap(),
      include_str!("../../skills/implementation/references/quality.md")
    );
    assert_eq!(
      std::fs::read_to_string(worker.join("skills/implementation/references/handoff.md")).unwrap(),
      include_str!("../../skills/implementation/references/handoff.md")
    );
  }

  #[tokio::test]
  async fn isolated_environment_snapshots_global_agent_database() {
    let project = tempdir().unwrap();
    let global_agent_dir = project.path().join("global-agent");
    std::fs::create_dir_all(&global_agent_dir).unwrap();
    let source = Connection::open(global_agent_dir.join("agent.db")).unwrap();
    source
      .execute_batch(
        "CREATE TABLE auth_credentials (provider TEXT NOT NULL);
         INSERT INTO auth_credentials (provider) VALUES ('openai-codex');",
      )
      .unwrap();

    let resolved = resolve(
      project.path(),
      &SkillsConfig::default(),
      WorkerRole::Architect,
    )
    .unwrap();
    let worker = prepare_worker_environment(
      project.path(),
      WorkerRole::Architect,
      &resolved,
      &global_agent_dir,
    )
    .await
    .unwrap();
    let target = Connection::open(worker.join("agent.db")).unwrap();
    let provider: String = target
      .query_row("SELECT provider FROM auth_credentials LIMIT 1", [], |row| {
        row.get(0)
      })
      .unwrap();
    assert_eq!(provider, "openai-codex");
  }
}
