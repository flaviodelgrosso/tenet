CREATE TABLE trusted_verifier_executions (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  revision TEXT NOT NULL,
  verifier_name TEXT NOT NULL,
  spec_hash TEXT NOT NULL,
  isolation_policy_hash TEXT NOT NULL,
  result TEXT NOT NULL CHECK (result IN ('supports', 'contradicts')),
  spec_json TEXT NOT NULL CHECK (json_valid(spec_json)),
  record_json TEXT NOT NULL CHECK (json_valid(record_json)),
  authority_context_hash TEXT NOT NULL,
  authority_mac TEXT NOT NULL
) STRICT;
CREATE INDEX trusted_verifier_executions_revision_idx
  ON trusted_verifier_executions(run_id, revision, verifier_name);

ALTER TABLE evidence_artifacts
  ADD COLUMN authority_mac TEXT;

ALTER TABLE evidence_artifacts
  ADD COLUMN authority_context_hash TEXT;
