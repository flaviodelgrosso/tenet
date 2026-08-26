//! Transactional SQLite persistence for controller-owned Tenet state.

mod catalog;
mod context;
mod evidence;
mod integration;
mod roadmap;
mod state;

pub use context::WorkUnitContext;

use std::{
  path::{Path, PathBuf},
  str::FromStr,
  sync::OnceLock,
  time::Duration,
};

use sha2::{Digest, Sha256};
use sqlx::{
  migrate::Migrator,
  sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
  },
  Row,
};
use thiserror::Error;

static MIGRATOR: Migrator = sqlx::migrate!();
static CONTROLLER_AUTHORITY_KEY: OnceLock<[u8; 32]> = OnceLock::new();

/// Installs controller-only key material used to authenticate trusted authority across restarts.
pub fn install_controller_authority_key(
  authority_namespace: &str,
  key_material: &[u8],
) -> Result<(), StorageError> {
  if authority_namespace.trim().is_empty() || key_material.is_empty() {
    return Err(StorageError::IntegrityViolation(
      "controller authority namespace and key material cannot be empty".into(),
    ));
  }
  let mut digest = Sha256::new();
  digest.update(b"tenet-controller-authority-v2");
  digest.update((authority_namespace.len() as u64).to_be_bytes());
  digest.update(authority_namespace.as_bytes());
  digest.update((key_material.len() as u64).to_be_bytes());
  digest.update(key_material);
  let derived: [u8; 32] = digest.finalize().into();
  if let Some(current) = CONTROLLER_AUTHORITY_KEY.get() {
    if current != &derived {
      return Err(StorageError::IntegrityViolation(
        "controller authority identity changed within this process".into(),
      ));
    }
    return Ok(());
  }
  match CONTROLLER_AUTHORITY_KEY.set(derived) {
    Ok(()) => Ok(()),
    Err(raced) if CONTROLLER_AUTHORITY_KEY.get() == Some(&raced) => Ok(()),
    Err(_) => Err(StorageError::IntegrityViolation(
      "controller authority identity changed during initialization".into(),
    )),
  }
}

pub(crate) fn controller_authority_key() -> Result<&'static [u8; 32], StorageError> {
  CONTROLLER_AUTHORITY_KEY.get().ok_or_else(|| {
    StorageError::IntegrityViolation(
      "trusted authority requires a controller-only external identity".into(),
    )
  })
}

/// The file name of the authoritative controller database.
pub const DATABASE_FILE: &str = "tenet.db";

/// Result of SQLite's explicit consistency diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseHealth {
  /// SQLite found no consistency errors.
  Ok,
  /// SQLite returned one or more consistency failures.
  Issues(Vec<String>),
}

/// Typed failures from the authoritative persistence boundary.
#[derive(Debug, Error)]
pub enum StorageError {
  /// The database file or its containing directory could not be opened.
  #[error("database unavailable at {path}: {source}")]
  DatabaseUnavailable {
    path: PathBuf,
    #[source]
    source: Box<dyn std::error::Error + Send + Sync>,
  },
  /// SQLite remained locked beyond the configured busy timeout.
  #[error("database remained locked beyond the configured busy timeout: {0}")]
  DatabaseLocked(String),
  /// A foreign-key relationship was rejected.
  #[error("database foreign-key constraint rejected the transition: {0}")]
  ForeignKeyViolation(String),
  /// A database constraint rejected an invalid transition.
  #[error("database integrity constraint rejected the transition: {0}")]
  IntegrityViolation(String),
  /// Embedded migrations could not establish the expected schema.
  #[error("database migration failed: {0}")]
  MigrationFailed(String),
  /// SQLite reported corruption or failed an explicit integrity diagnostic.
  #[error("database integrity diagnostic failed: {0}")]
  CorruptDatabase(String),
  /// A query expected exactly one logical record but observed another cardinality.
  #[error("unexpected database cardinality: {0}")]
  UnexpectedCardinality(String),
  /// A query or transaction failed without a more specific storage classification.
  #[error("database operation failed: {0}")]
  Database(String),
}

impl StorageError {
  fn from_sqlx(error: sqlx::Error) -> Self {
    let message = error.to_string();
    let Some(database) = error.as_database_error() else {
      return Self::Database(message);
    };
    let code = database.code();
    if matches!(code.as_deref(), Some("5" | "6" | "261" | "262")) {
      return Self::DatabaseLocked(message);
    }
    if matches!(code.as_deref(), Some("787")) || message.contains("FOREIGN KEY constraint failed") {
      return Self::ForeignKeyViolation(message);
    }
    if database.is_check_violation()
      || database.is_unique_violation()
      || database.is_foreign_key_violation()
    {
      return Self::IntegrityViolation(message);
    }
    Self::Database(message)
  }
}

/// Cloneable handle to one repository's authoritative controller database.
#[derive(Clone)]
pub struct Storage {
  pool: SqlitePool,
  path: PathBuf,
}

impl Storage {
  /// Opens `.tenet/tenet.db`, applies durable connection options, and runs embedded migrations.
  ///
  /// # Errors
  /// Returns [`StorageError`] if the directory, database, or embedded schema cannot be opened.
  pub async fn open(repository: &Path) -> Result<Self, StorageError> {
    let directory = repository.join(".tenet");
    tokio::fs::create_dir_all(&directory)
      .await
      .map_err(|source| StorageError::DatabaseUnavailable {
        path: directory.clone(),
        source: Box::new(source),
      })?;
    Self::open_path(directory.join(DATABASE_FILE), true).await
  }

  /// Opens an existing authoritative database without creating replacement state.
  pub async fn open_existing(repository: &Path) -> Result<Self, StorageError> {
    let path = repository.join(".tenet").join(DATABASE_FILE);
    match tokio::fs::try_exists(&path).await {
      Ok(true) => Self::open_existing_path(path).await,
      Ok(false) => Err(StorageError::DatabaseUnavailable {
        path: path.clone(),
        source: Box::new(std::io::Error::new(
          std::io::ErrorKind::NotFound,
          "authoritative database does not exist",
        )),
      }),
      Err(source) => Err(StorageError::DatabaseUnavailable {
        path,
        source: Box::new(source),
      }),
    }
  }

  async fn open_path(path: PathBuf, create_if_missing: bool) -> Result<Self, StorageError> {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
      .map_err(StorageError::from_sqlx)?
      .filename(&path)
      .in_memory(false)
      .create_if_missing(create_if_missing)
      .foreign_keys(true)
      .journal_mode(SqliteJournalMode::Wal)
      .synchronous(SqliteSynchronous::Full)
      .busy_timeout(Duration::from_secs(5))
      .shared_cache(false);
    let pool = SqlitePoolOptions::new()
      .max_connections(8)
      .connect_with(options)
      .await
      .map_err(|source| StorageError::DatabaseUnavailable {
        path: path.clone(),
        source: Box::new(source),
      })?;
    MIGRATOR
      .run(&pool)
      .await
      .map_err(|error| StorageError::MigrationFailed(error.to_string()))?;
    Ok(Self { pool, path })
  }

  async fn open_existing_path(path: PathBuf) -> Result<Self, StorageError> {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
      .map_err(StorageError::from_sqlx)?
      .filename(&path)
      .in_memory(false)
      .create_if_missing(false)
      .read_only(true)
      .foreign_keys(true)
      .busy_timeout(Duration::from_secs(5))
      .shared_cache(false);
    let pool = SqlitePoolOptions::new()
      .max_connections(8)
      .connect_with(options)
      .await
      .map_err(|source| StorageError::DatabaseUnavailable {
        path: path.clone(),
        source: Box::new(source),
      })?;
    let schema_kind = sqlx::query_scalar::<_, String>(
      "SELECT value FROM storage_metadata WHERE key = 'schema_kind'",
    )
    .fetch_optional(&pool)
    .await
    .map_err(|error| {
      StorageError::CorruptDatabase(format!("existing file is not a Tenet database: {error}"))
    })?;
    if schema_kind.as_deref() != Some("tenet-controller-state") {
      return Err(StorageError::CorruptDatabase(
        "existing file has no Tenet schema marker".into(),
      ));
    }
    let applied_migrations =
      sqlx::query("SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version")
        .fetch_all(&pool)
        .await
        .map_err(|error| {
          StorageError::CorruptDatabase(format!(
            "existing database has invalid migration metadata: {error}"
          ))
        })?;
    let embedded_migrations: Vec<_> = MIGRATOR.iter().collect();
    if applied_migrations.len() != embedded_migrations.len() {
      return Err(StorageError::MigrationFailed(format!(
        "existing database has {} applied migrations; {} required",
        applied_migrations.len(),
        embedded_migrations.len()
      )));
    }
    for (applied, embedded) in applied_migrations.iter().zip(embedded_migrations) {
      let version = applied.get::<i64, _>("version");
      let checksum = applied.get::<Vec<u8>, _>("checksum");
      let success = applied.get::<bool, _>("success");
      if !success
        || version != embedded.version
        || checksum.as_slice() != embedded.checksum.as_ref()
      {
        return Err(StorageError::MigrationFailed(format!(
          "existing database migration {version} does not match embedded migration {}",
          embedded.version
        )));
      }
    }
    Ok(Self { pool, path })
  }

  /// Returns the authoritative database path.
  pub fn path(&self) -> &Path {
    &self.path
  }

  /// Exposes the pool for storage-crate integration diagnostics and constraint tests.
  #[doc(hidden)]
  pub fn pool(&self) -> &SqlitePool {
    &self.pool
  }

  /// Runs SQLite's bounded `quick_check` diagnostic.
  ///
  /// # Errors
  /// Returns [`StorageError::CorruptDatabase`] when SQLite cannot execute the diagnostic.
  pub async fn quick_check(&self) -> Result<DatabaseHealth, StorageError> {
    self.check("PRAGMA quick_check").await
  }

  /// Runs SQLite's complete `integrity_check` diagnostic.
  ///
  /// # Errors
  /// Returns [`StorageError::CorruptDatabase`] when SQLite cannot execute the diagnostic.
  pub async fn integrity_check(&self) -> Result<DatabaseHealth, StorageError> {
    self.check("PRAGMA integrity_check").await
  }

  /// Runs SQLite's foreign-key consistency diagnostic.
  pub async fn foreign_key_check(&self) -> Result<DatabaseHealth, StorageError> {
    let violations = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pragma_foreign_key_check")
      .fetch_one(&self.pool)
      .await
      .map_err(StorageError::from_sqlx)?;
    if violations == 0 {
      Ok(DatabaseHealth::Ok)
    } else {
      Ok(DatabaseHealth::Issues(vec![format!(
        "{violations} foreign-key violation(s)"
      )]))
    }
  }

  async fn check(&self, statement: &'static str) -> Result<DatabaseHealth, StorageError> {
    let rows = sqlx::query_scalar::<_, String>(statement)
      .fetch_all(&self.pool)
      .await
      .map_err(|error| StorageError::CorruptDatabase(error.to_string()))?;
    if rows.len() == 1 && rows[0] == "ok" {
      Ok(DatabaseHealth::Ok)
    } else {
      Ok(DatabaseHealth::Issues(rows))
    }
  }
}
