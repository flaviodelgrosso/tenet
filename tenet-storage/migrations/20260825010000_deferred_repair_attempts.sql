ALTER TABLE candidates
ADD COLUMN repair_attempts INTEGER NOT NULL DEFAULT 0 CHECK (repair_attempts >= 0);
