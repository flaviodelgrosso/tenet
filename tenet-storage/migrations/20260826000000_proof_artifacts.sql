ALTER TABLE verification_obligations
  ADD COLUMN evidence_contract_json TEXT NOT NULL
  DEFAULT '{"type":"human_attestation","statement":"Explicit human attestation required"}'
  CHECK (json_valid(evidence_contract_json));

CREATE TABLE evidence_artifacts (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  revision TEXT NOT NULL,
  authority TEXT NOT NULL CHECK (authority IN ('authoritative','supporting','advisory')),
  validity TEXT NOT NULL CHECK (validity IN ('valid','stale')),
  artifact_json TEXT NOT NULL CHECK (json_valid(artifact_json))
) STRICT;
CREATE INDEX evidence_artifacts_revision_idx ON evidence_artifacts(run_id, revision, validity, authority);

CREATE TABLE artifact_obligations (
  artifact_id TEXT NOT NULL REFERENCES evidence_artifacts(id) ON DELETE CASCADE,
  obligation_id TEXT NOT NULL REFERENCES verification_obligations(id) ON DELETE CASCADE,
  PRIMARY KEY (artifact_id, obligation_id)
) STRICT;

CREATE TABLE assessment_judgments (
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  obligation_id TEXT NOT NULL REFERENCES verification_obligations(id) ON DELETE CASCADE,
  revision TEXT NOT NULL,
  judgment_json TEXT NOT NULL CHECK (json_valid(judgment_json)),
  observed_at TEXT NOT NULL,
  PRIMARY KEY (run_id, obligation_id, revision)
) STRICT;

CREATE TABLE proof_derivations (
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  obligation_id TEXT NOT NULL REFERENCES verification_obligations(id) ON DELETE CASCADE,
  revision TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('proven','contradicted','insufficient','stale')),
  derivation_json TEXT NOT NULL CHECK (json_valid(derivation_json)),
  derived_at TEXT NOT NULL,
  PRIMARY KEY (run_id, obligation_id, revision)
) STRICT;
