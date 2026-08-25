-- no-transaction
PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;

BEGIN IMMEDIATE;

ALTER TABLE runs RENAME TO runs_before_catalog_approval;

CREATE TABLE runs (
  id TEXT PRIMARY KEY,
  status TEXT NOT NULL CHECK (status IN ('idle','running','review_required','done','blocked','failed','stopped')),
  phase TEXT NOT NULL CHECK (phase IN ('initialized','architecting','reviewing_requirements','reconciling','scheduling','implementing','verifying','repairing','integrating','assessing','complete')),
  cycle INTEGER NOT NULL CHECK (cycle >= 0),
  stagnation_count INTEGER NOT NULL CHECK (stagnation_count >= 0),
  progress_fingerprint TEXT,
  last_summary TEXT NOT NULL,
  blocked_reason TEXT,
  last_error TEXT,
  updated_at TEXT NOT NULL
) STRICT;

INSERT INTO runs(
  id,
  status,
  phase,
  cycle,
  stagnation_count,
  progress_fingerprint,
  last_summary,
  blocked_reason,
  last_error,
  updated_at
)
SELECT
  id,
  status,
  phase,
  cycle,
  stagnation_count,
  progress_fingerprint,
  last_summary,
  blocked_reason,
  last_error,
  updated_at
FROM runs_before_catalog_approval;

DROP TABLE runs_before_catalog_approval;

CREATE TABLE catalog_approvals (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  spec_hash TEXT NOT NULL REFERENCES specification_snapshots(hash) ON DELETE CASCADE,
  catalog_hash TEXT NOT NULL CHECK (length(catalog_hash) = 64),
  approved_at TEXT NOT NULL
) STRICT;

CREATE UNIQUE INDEX catalog_approvals_identity_idx
  ON catalog_approvals(spec_hash, catalog_hash);

COMMIT;

PRAGMA legacy_alter_table = OFF;
PRAGMA foreign_keys = ON;
