-- Catalogs created before explicit proof contracts received an indiscriminate
-- human-attestation default. Remove only their active identity so the controller
-- regenerates and requires approval of real contracts on the next run.
DELETE FROM catalog_approvals;
DELETE FROM storage_metadata
WHERE key IN ('active_spec_hash', 'active_catalog_hash');
