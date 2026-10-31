-- Holds one row. block-saver only inserts blocks above this slot.
--
-- The indexer must advance it and delete the block in the same transaction,
-- UPDATE first. block-saver reads this row FOR SHARE, so the UPDATE waits for
-- in-flight inserts and those inserts can't miss a committed advance.
CREATE TABLE last_indexed_block (
    slot BIGINT NOT NULL
);

INSERT INTO last_indexed_block (slot) VALUES (0);
