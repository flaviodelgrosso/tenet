CREATE TABLE human_attestations (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  attestor_id TEXT NOT NULL,
  statement_hash TEXT NOT NULL,
  obligation_id TEXT NOT NULL REFERENCES verification_obligations(id) ON DELETE CASCADE,
  catalog_hash TEXT NOT NULL,
  revision TEXT NOT NULL,
  issued_at TEXT NOT NULL,
  algorithm TEXT NOT NULL CHECK (algorithm = 'ed25519'),
  public_key TEXT NOT NULL,
  signature TEXT NOT NULL,
  record_json TEXT NOT NULL CHECK (json_valid(record_json))
) STRICT;
CREATE INDEX human_attestations_binding_idx
  ON human_attestations(obligation_id, revision, attestor_id);

CREATE TABLE evidence_artifact_dependencies (
 artifact_id TEXT PRIMARY KEY REFERENCES evidence_artifacts(id) ON DELETE CASCADE,
 dependency_json TEXT NOT NULL CHECK (json_valid(dependency_json))
) STRICT;
