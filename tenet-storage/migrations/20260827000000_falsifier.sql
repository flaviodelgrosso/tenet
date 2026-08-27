CREATE TABLE falsifier_executions (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  revision TEXT NOT NULL,
  falsifier_name TEXT NOT NULL,
  spec_hash TEXT NOT NULL,
  isolation_policy_hash TEXT NOT NULL,
  image_digest TEXT NOT NULL,
  input_hash TEXT NOT NULL,
  result TEXT NOT NULL CHECK (result IN ('counterexample_found', 'no_counterexample_found')),
  spec_json TEXT NOT NULL CHECK (json_valid(spec_json)),
  record_json TEXT NOT NULL CHECK (json_valid(record_json)),
  authority_context_hash TEXT NOT NULL,
  authority_mac TEXT NOT NULL
) STRICT;
CREATE INDEX falsifier_executions_revision_idx
  ON falsifier_executions(run_id, revision, falsifier_name);
