CREATE TABLE storage_metadata (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;

CREATE TABLE runs (
  id TEXT PRIMARY KEY,
  status TEXT NOT NULL CHECK (status IN ('idle','running','done','blocked','failed','stopped')),
  phase TEXT NOT NULL CHECK (phase IN ('initialized','architecting','reconciling','scheduling','implementing','verifying','repairing','integrating','assessing','complete')),
  cycle INTEGER NOT NULL CHECK (cycle >= 0),
  stagnation_count INTEGER NOT NULL CHECK (stagnation_count >= 0),
  progress_fingerprint TEXT,
  last_summary TEXT NOT NULL,
  blocked_reason TEXT,
  last_error TEXT,
  updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE current_run (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE
) STRICT;

CREATE TABLE run_projection_cache (
  run_id TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,
  requirement_counts_json TEXT NOT NULL CHECK (json_valid(requirement_counts_json)),
  verification_layers_json TEXT NOT NULL CHECK (json_valid(verification_layers_json))
) STRICT;

CREATE TABLE specification_snapshots (
  hash TEXT PRIMARY KEY,
  source_path TEXT NOT NULL,
  observed_at TEXT NOT NULL
) STRICT;

CREATE TABLE spec_fragments (
  id TEXT PRIMARY KEY,
  spec_hash TEXT NOT NULL REFERENCES specification_snapshots(hash) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  section TEXT,
  text TEXT NOT NULL,
  text_hash TEXT NOT NULL,
  UNIQUE (spec_hash, ordinal)
) STRICT;
CREATE INDEX spec_fragments_spec_hash_idx ON spec_fragments(spec_hash);

CREATE TABLE requirements (
  id TEXT PRIMARY KEY,
  spec_hash TEXT NOT NULL REFERENCES specification_snapshots(hash) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  title TEXT NOT NULL,
  description TEXT NOT NULL,
  required INTEGER NOT NULL CHECK (required IN (0, 1)),
  UNIQUE (spec_hash, ordinal),
  UNIQUE (id, spec_hash)
) STRICT;
CREATE INDEX requirements_spec_hash_idx ON requirements(spec_hash);

CREATE TABLE requirement_source_fragments (
  requirement_id TEXT NOT NULL REFERENCES requirements(id) ON DELETE CASCADE,
  fragment_id TEXT NOT NULL REFERENCES spec_fragments(id) ON DELETE RESTRICT,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  PRIMARY KEY (requirement_id, fragment_id),
  UNIQUE (requirement_id, ordinal)
) STRICT;
CREATE INDEX requirement_source_fragments_fragment_idx ON requirement_source_fragments(fragment_id);

CREATE TABLE uncovered_spec_fragments (
  spec_hash TEXT NOT NULL REFERENCES specification_snapshots(hash) ON DELETE CASCADE,
  fragment_id TEXT NOT NULL REFERENCES spec_fragments(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  PRIMARY KEY (spec_hash, fragment_id),
  UNIQUE (spec_hash, ordinal)
) STRICT;

CREATE TABLE acceptance_criteria (
  id TEXT PRIMARY KEY,
  requirement_id TEXT NOT NULL REFERENCES requirements(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  description TEXT NOT NULL,
  mandatory INTEGER NOT NULL CHECK (mandatory IN (0, 1)),
  UNIQUE (requirement_id, ordinal),
  UNIQUE (id, requirement_id)
) STRICT;
CREATE INDEX acceptance_criteria_requirement_idx ON acceptance_criteria(requirement_id);

CREATE TABLE verification_obligations (
  id TEXT PRIMARY KEY,
  criterion_id TEXT NOT NULL REFERENCES acceptance_criteria(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  description TEXT NOT NULL,
  required INTEGER NOT NULL CHECK (required IN (0, 1)),
  UNIQUE (criterion_id, ordinal),
  UNIQUE (id, criterion_id)
) STRICT;
CREATE INDEX verification_obligations_criterion_idx ON verification_obligations(criterion_id);

CREATE TABLE reconcile_rounds (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  cycle INTEGER NOT NULL CHECK (cycle >= 0),
  repository_revision TEXT NOT NULL,
  catalog_hash TEXT NOT NULL REFERENCES specification_snapshots(hash),
  summary TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE (run_id, cycle)
) STRICT;
CREATE INDEX reconcile_rounds_run_idx ON reconcile_rounds(run_id, cycle DESC);

CREATE TABLE requirement_assessments (
  reconcile_round_id TEXT NOT NULL REFERENCES reconcile_rounds(id) ON DELETE CASCADE,
  requirement_id TEXT NOT NULL REFERENCES requirements(id),
  implementation_state TEXT NOT NULL CHECK (implementation_state IN ('present','partial','absent','unknown')),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  PRIMARY KEY (reconcile_round_id, requirement_id),
  UNIQUE (reconcile_round_id, ordinal)
) STRICT;
CREATE INDEX requirement_assessments_requirement_idx ON requirement_assessments(requirement_id);

CREATE TABLE requirement_assessment_observations (
  reconcile_round_id TEXT NOT NULL,
  requirement_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  observation TEXT NOT NULL,
  PRIMARY KEY (reconcile_round_id, requirement_id, ordinal),
  FOREIGN KEY (reconcile_round_id, requirement_id) REFERENCES requirement_assessments(reconcile_round_id, requirement_id) ON DELETE CASCADE
) STRICT;

CREATE TABLE requirement_assessment_missing_implementation (
  reconcile_round_id TEXT NOT NULL,
  requirement_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  description TEXT NOT NULL,
  PRIMARY KEY (reconcile_round_id, requirement_id, ordinal),
  FOREIGN KEY (reconcile_round_id, requirement_id) REFERENCES requirement_assessments(reconcile_round_id, requirement_id) ON DELETE CASCADE
) STRICT;

CREATE TABLE requirement_assessment_missing_evidence (
  reconcile_round_id TEXT NOT NULL,
  requirement_id TEXT NOT NULL,
  obligation_id TEXT NOT NULL REFERENCES verification_obligations(id),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  PRIMARY KEY (reconcile_round_id, requirement_id, obligation_id),
  UNIQUE (reconcile_round_id, requirement_id, ordinal),
  FOREIGN KEY (reconcile_round_id, requirement_id) REFERENCES requirement_assessments(reconcile_round_id, requirement_id) ON DELETE CASCADE
) STRICT;
CREATE INDEX requirement_assessment_missing_evidence_obligation_idx ON requirement_assessment_missing_evidence(obligation_id);

CREATE TABLE work_units (
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  id TEXT NOT NULL,
  reconcile_round_id TEXT NOT NULL REFERENCES reconcile_rounds(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  title TEXT NOT NULL,
  objective TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('pending','ready','running','candidate','integrating','completed','failed','blocked','invalidated')),
  PRIMARY KEY (run_id, id),
  UNIQUE (reconcile_round_id, ordinal),
  UNIQUE (reconcile_round_id, id)
) STRICT;
CREATE INDEX work_units_round_idx ON work_units(reconcile_round_id);
CREATE INDEX work_units_status_idx ON work_units(run_id, status);

CREATE TABLE work_unit_requirements (
  run_id TEXT NOT NULL,
  work_unit_id TEXT NOT NULL,
  requirement_id TEXT NOT NULL REFERENCES requirements(id),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  PRIMARY KEY (run_id, work_unit_id, requirement_id),
  UNIQUE (run_id, work_unit_id, ordinal),
  FOREIGN KEY (run_id, work_unit_id) REFERENCES work_units(run_id, id) ON DELETE CASCADE
) STRICT;
CREATE INDEX work_unit_requirements_requirement_idx ON work_unit_requirements(requirement_id);

CREATE TABLE work_unit_criteria (
  run_id TEXT NOT NULL,
  work_unit_id TEXT NOT NULL,
  criterion_id TEXT NOT NULL REFERENCES acceptance_criteria(id),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  PRIMARY KEY (run_id, work_unit_id, criterion_id),
  UNIQUE (run_id, work_unit_id, ordinal),
  FOREIGN KEY (run_id, work_unit_id) REFERENCES work_units(run_id, id) ON DELETE CASCADE
) STRICT;
CREATE INDEX work_unit_criteria_criterion_idx ON work_unit_criteria(criterion_id);

CREATE TABLE work_unit_obligations (
  run_id TEXT NOT NULL,
  work_unit_id TEXT NOT NULL,
  obligation_id TEXT NOT NULL REFERENCES verification_obligations(id),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  PRIMARY KEY (run_id, work_unit_id, obligation_id),
  UNIQUE (run_id, work_unit_id, ordinal),
  FOREIGN KEY (run_id, work_unit_id) REFERENCES work_units(run_id, id) ON DELETE CASCADE
) STRICT;
CREATE INDEX work_unit_obligations_obligation_idx ON work_unit_obligations(obligation_id);

CREATE TABLE work_unit_dependencies (
  run_id TEXT NOT NULL,
  work_unit_id TEXT NOT NULL,
  dependency_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  PRIMARY KEY (run_id, work_unit_id, dependency_id),
  UNIQUE (run_id, work_unit_id, ordinal),
  CHECK (work_unit_id <> dependency_id),
  FOREIGN KEY (run_id, work_unit_id) REFERENCES work_units(run_id, id) ON DELETE CASCADE,
  FOREIGN KEY (run_id, dependency_id) REFERENCES work_units(run_id, id) ON DELETE CASCADE
) STRICT;
CREATE INDEX work_unit_dependencies_dependency_idx ON work_unit_dependencies(run_id, dependency_id);

CREATE TABLE work_unit_scope_paths (
  run_id TEXT NOT NULL,
  work_unit_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  path TEXT NOT NULL,
  PRIMARY KEY (run_id, work_unit_id, ordinal),
  UNIQUE (run_id, work_unit_id, path),
  FOREIGN KEY (run_id, work_unit_id) REFERENCES work_units(run_id, id) ON DELETE CASCADE
) STRICT;

CREATE TABLE work_unit_suggested_checks (
  run_id TEXT NOT NULL,
  work_unit_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  obligation_id TEXT NOT NULL REFERENCES verification_obligations(id),
  command TEXT NOT NULL,
  PRIMARY KEY (run_id, work_unit_id, ordinal),
  UNIQUE (run_id, work_unit_id, obligation_id, command),
  FOREIGN KEY (run_id, work_unit_id) REFERENCES work_units(run_id, id) ON DELETE CASCADE
) STRICT;
CREATE INDEX work_unit_suggested_checks_obligation_idx ON work_unit_suggested_checks(obligation_id);

CREATE TABLE repair_progress (
  run_id TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,
  work_unit_id TEXT NOT NULL,
  attempt INTEGER NOT NULL CHECK (attempt >= 0),
  FOREIGN KEY (run_id, work_unit_id) REFERENCES work_units(run_id, id)
) STRICT;

CREATE TABLE leases (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL,
  work_unit_id TEXT NOT NULL,
  work_unit_json TEXT NOT NULL CHECK (json_valid(work_unit_json)),
  worker_id TEXT NOT NULL,
  base_revision TEXT NOT NULL,
  workspace TEXT NOT NULL,
  issued_at TEXT NOT NULL,
  active INTEGER NOT NULL CHECK (active IN (0, 1)),
  FOREIGN KEY (run_id, work_unit_id) REFERENCES work_units(run_id, id),
  UNIQUE (id, run_id)
) STRICT;
CREATE INDEX leases_active_idx ON leases(run_id, active);

CREATE TABLE candidates (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  lease_id TEXT NOT NULL,
  base_revision TEXT NOT NULL,
  candidate_revision TEXT NOT NULL,
  catalog_hash TEXT NOT NULL REFERENCES specification_snapshots(hash),
  git_ref TEXT,
  state TEXT NOT NULL CHECK (state IN ('candidate','deferred','integrating','consumed','invalidated')),
  worker_summary_json TEXT NOT NULL CHECK (json_valid(worker_summary_json)),
  verification_report_json TEXT CHECK (verification_report_json IS NULL OR json_valid(verification_report_json)),
  created_at TEXT NOT NULL,
  UNIQUE (candidate_revision),
  FOREIGN KEY (lease_id, run_id) REFERENCES leases(id, run_id)
) STRICT;
CREATE INDEX candidates_run_state_idx ON candidates(run_id, state);

CREATE TABLE candidate_changed_paths (
  candidate_id TEXT NOT NULL REFERENCES candidates(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  path TEXT NOT NULL,
  PRIMARY KEY (candidate_id, ordinal),
  UNIQUE (candidate_id, path)
) STRICT;

CREATE TABLE candidate_discoveries (
  candidate_id TEXT NOT NULL REFERENCES candidates(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  discovery_json TEXT NOT NULL CHECK (json_valid(discovery_json)),
  PRIMARY KEY (candidate_id, ordinal)
) STRICT;

CREATE TABLE discoveries (
  fingerprint TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  catalog_hash TEXT NOT NULL REFERENCES specification_snapshots(hash),
  repository_revision TEXT NOT NULL,
  work_unit_id TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('architect','reconcile','implement','repair','assess')),
  cycle INTEGER NOT NULL CHECK (cycle >= 0),
  kind TEXT NOT NULL CHECK (kind IN ('dependency','blocker','verification_blocker','scope_expansion')),
  status TEXT NOT NULL CHECK (status IN ('active','consumed','invalidated')),
  reason TEXT,
  payload_json TEXT CHECK (payload_json IS NULL OR json_valid(payload_json))
) STRICT;
CREATE INDEX discoveries_active_idx ON discoveries(run_id, status);

CREATE TABLE project_verification_runs (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  revision TEXT NOT NULL,
  suite_hash TEXT NOT NULL,
  passed INTEGER NOT NULL CHECK (passed IN (0, 1)),
  started_at TEXT NOT NULL,
  finished_at TEXT NOT NULL,
  UNIQUE (id, run_id)
) STRICT;
CREATE INDEX project_verification_runs_revision_idx ON project_verification_runs(run_id, revision, suite_hash, finished_at DESC);

CREATE TABLE project_verification_checks (
  verification_run_id TEXT NOT NULL REFERENCES project_verification_runs(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  name TEXT NOT NULL,
  program TEXT NOT NULL,
  working_directory TEXT NOT NULL,
  timeout_secs INTEGER NOT NULL CHECK (timeout_secs >= 0),
  command_display TEXT NOT NULL,
  exit_code INTEGER,
  timed_out INTEGER NOT NULL CHECK (timed_out IN (0, 1)),
  duration_ms INTEGER NOT NULL CHECK (duration_ms >= 0),
  stdout TEXT NOT NULL,
  stderr TEXT NOT NULL,
  PRIMARY KEY (verification_run_id, ordinal)
) STRICT;

CREATE TABLE project_verification_check_args (
  verification_run_id TEXT NOT NULL,
  check_ordinal INTEGER NOT NULL,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  argument TEXT NOT NULL,
  PRIMARY KEY (verification_run_id, check_ordinal, ordinal),
  FOREIGN KEY (verification_run_id, check_ordinal) REFERENCES project_verification_checks(verification_run_id, ordinal) ON DELETE CASCADE
) STRICT;

CREATE TABLE project_verification_check_environment (
  verification_run_id TEXT NOT NULL,
  check_ordinal INTEGER NOT NULL,
  name TEXT NOT NULL,
  value TEXT NOT NULL,
  PRIMARY KEY (verification_run_id, check_ordinal, name),
  FOREIGN KEY (verification_run_id, check_ordinal) REFERENCES project_verification_checks(verification_run_id, ordinal) ON DELETE CASCADE
) STRICT;

CREATE TABLE semantic_evidence (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  requirement_id TEXT NOT NULL,
  criterion_id TEXT NOT NULL,
  obligation_id TEXT NOT NULL,
  source TEXT NOT NULL CHECK (source IN ('project_verification','semantic_assessment','agent_suggestion')),
  result TEXT NOT NULL CHECK (result IN ('passed','failed','inconclusive')),
  revision TEXT NOT NULL,
  observed_at TEXT NOT NULL,
  provenance_kind TEXT NOT NULL CHECK (provenance_kind IN ('independent_assessment','agent_proposal')),
  worker_id TEXT,
  worker_role TEXT,
  rationale TEXT NOT NULL,
  validity TEXT NOT NULL CHECK (validity IN ('valid','stale')),
  invalidated_at TEXT,
  superseded_by_revision TEXT,
  CHECK ((provenance_kind = 'independent_assessment' AND worker_id IS NOT NULL AND worker_role IS NULL) OR (provenance_kind = 'agent_proposal' AND worker_id IS NULL AND worker_role IS NOT NULL)),
  CHECK ((validity = 'valid' AND invalidated_at IS NULL AND superseded_by_revision IS NULL) OR (validity = 'stale' AND invalidated_at IS NOT NULL AND superseded_by_revision IS NOT NULL)),
  FOREIGN KEY (criterion_id, requirement_id) REFERENCES acceptance_criteria(id, requirement_id),
  FOREIGN KEY (obligation_id, criterion_id) REFERENCES verification_obligations(id, criterion_id)
) STRICT;
CREATE INDEX semantic_evidence_obligation_revision_idx ON semantic_evidence(obligation_id, revision, validity);
CREATE INDEX semantic_evidence_requirement_idx ON semantic_evidence(requirement_id, revision, validity);

CREATE TABLE evidence_refs (
  evidence_id TEXT NOT NULL REFERENCES semantic_evidence(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  reference TEXT NOT NULL,
  PRIMARY KEY (evidence_id, ordinal)
) STRICT;

CREATE TABLE integration_transactions (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  work_unit_id TEXT NOT NULL,
  candidate_revision TEXT NOT NULL,
  old_head TEXT NOT NULL,
  new_head TEXT NOT NULL,
  phase TEXT NOT NULL CHECK (phase IN ('prepared','git_committed','state_committed')),
  verification_run_id TEXT NOT NULL,
  verification_hash TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (run_id, work_unit_id) REFERENCES work_units(run_id, id),
  FOREIGN KEY (verification_run_id, run_id) REFERENCES project_verification_runs(id, run_id)
) STRICT;
CREATE UNIQUE INDEX one_active_integration_global ON integration_transactions((1)) WHERE phase <> 'state_committed';

CREATE TABLE completed_work_units (
  run_id TEXT NOT NULL REFERENCES runs(id),
  work_unit_id TEXT NOT NULL,
  work_unit_json TEXT NOT NULL CHECK (json_valid(work_unit_json)),
  completed_at TEXT NOT NULL,
  verification_run_id TEXT NOT NULL,
  PRIMARY KEY (run_id, work_unit_id, verification_run_id),
  FOREIGN KEY (verification_run_id, run_id) REFERENCES project_verification_runs(id, run_id)
) STRICT;

INSERT INTO storage_metadata(key, value) VALUES ('schema_kind', 'tenet-controller-state');
