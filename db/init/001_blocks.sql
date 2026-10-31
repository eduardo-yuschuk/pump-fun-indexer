CREATE TABLE blocks (
    slot        BIGINT PRIMARY KEY,
    parent_slot BIGINT NOT NULL,
    block_time  TIMESTAMPTZ,
    readed_time TIMESTAMPTZ NOT NULL,
    data        JSON COMPRESSION lz4 NOT NULL
);
