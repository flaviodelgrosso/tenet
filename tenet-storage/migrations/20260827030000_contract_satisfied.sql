CREATE TABLE proof_derivations_contract_satisfied (
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  obligation_id TEXT NOT NULL REFERENCES verification_obligations(id) ON DELETE CASCADE,
  revision TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('contract_satisfied','contradicted','insufficient','stale')),
  derivation_json TEXT NOT NULL CHECK (json_valid(derivation_json)),
  derived_at TEXT NOT NULL,
  PRIMARY KEY (run_id, obligation_id, revision)
) STRICT;

INSERT INTO proof_derivations_contract_satisfied(
  run_id,
  obligation_id,
  revision,
  state,
  derivation_json,
  derived_at
)
SELECT
  run_id,
  obligation_id,
  revision,
  CASE state WHEN 'proven' THEN 'contract_satisfied' ELSE state END,
  CASE state
    WHEN 'proven' THEN json_set(derivation_json, '$.state', 'contract_satisfied')
    ELSE derivation_json
  END,
  derived_at
FROM proof_derivations;

DROP TABLE proof_derivations;
ALTER TABLE proof_derivations_contract_satisfied RENAME TO proof_derivations;
