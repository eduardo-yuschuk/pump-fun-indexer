-- block-verifier stores a slot with no block as a row with skipped = true and
-- no block columns. Consumers must only read rows with verified = true.
ALTER TABLE blocks
    ALTER COLUMN parent_slot DROP NOT NULL,
    ALTER COLUMN readed_time DROP NOT NULL,
    ALTER COLUMN data DROP NOT NULL,
    ADD COLUMN skipped  BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN verified BOOLEAN NOT NULL DEFAULT false;
