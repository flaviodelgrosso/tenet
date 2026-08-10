use std::{
  collections::BTreeSet,
  path::{Path, PathBuf},
  time::Duration,
};

use anyhow::{bail, Context, Result};
use rusqlite::{backup::Backup, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::{config::SkillsConfig, model::WorkerRole};

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
    let target = skills_dir.join(directory).join("SKILL.md");
    let parent = target.parent().context("skill target has no parent")?;
    fs::create_dir_all(parent).await?;
    fs::copy(&skill.path, &target)
      .await
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
  let (name, contents) = match role {
    WorkerRole::Architect | WorkerRole::Reconcile => (
      "spec-analysis",
      include_str!("../skills/spec-analysis/SKILL.md"),
    ),
    WorkerRole::Implement => (
      "implementation",
      include_str!("../skills/implementation/SKILL.md"),
    ),
    WorkerRole::Repair => ("debugging", include_str!("../skills/debugging/SKILL.md")),
    WorkerRole::Assess => (
      "spec-assessment",
      include_str!("../skills/spec-assessment/SKILL.md"),
    ),
  };
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
    path,
    source: SkillSource::User,
  })
}

fn skill_directory_name(skill: &SkillSpec) -> Result<String> {
  if skill.source == SkillSource::BuiltIn {
    return Ok(skill.name.clone());
  }
  skill
    .path
    .parent()
    .and_then(Path::file_name)
    .and_then(|name| name.to_str())
    .map(str::to_owned)
    .context("configured skill directory must have a valid UTF-8 name")
}

fn materialize(name: &str, contents: &'static str) -> Result<PathBuf> {
  let path = std::env::temp_dir()
    .join("loops-built-in-skills")
    .join(name)
    .join("SKILL.md");
  if !path.is_file() {
    std::fs::create_dir_all(path.parent().context("built-in skill has no parent")?)?;
    std::fs::write(&path, contents)?;
  }
  Ok(path)
}

#[cfg(test)]
mod tests {
  use rusqlite::Connection;
  use std::collections::BTreeMap;

  use tempfile::tempdir;

  use super::{prepare_worker_environment, resolve};
  use crate::{config::SkillsConfig, model::WorkerRole};

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
