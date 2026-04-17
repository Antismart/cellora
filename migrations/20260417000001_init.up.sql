-- Initial schema for the Cellora CKB indexer (Week 1).
--
-- Hashes are stored raw as BYTEA (32 bytes) rather than hex strings so that
-- equality lookups and joins read from narrow fixed-width columns.
-- Script args / cell data are stored raw as BYTEA for the same reason.
-- `timestamp_ms` keeps CKB's millisecond epoch; callers convert at the edge.

CREATE TABLE blocks (
    number              BIGINT        PRIMARY KEY,
    hash                BYTEA         NOT NULL UNIQUE,
    parent_hash         BYTEA         NOT NULL,
    timestamp_ms        BIGINT        NOT NULL,
    epoch               BIGINT        NOT NULL,
    transactions_count  INTEGER       NOT NULL,
    proposals_count     INTEGER       NOT NULL,
    uncles_count        INTEGER       NOT NULL,
    nonce               NUMERIC(40)   NOT NULL,
    dao                 BYTEA         NOT NULL,
    indexed_at          TIMESTAMPTZ   NOT NULL DEFAULT now()
);

CREATE INDEX blocks_hash_idx ON blocks (hash);

CREATE TABLE transactions (
    hash           BYTEA        PRIMARY KEY,
    block_number   BIGINT       NOT NULL REFERENCES blocks(number) ON DELETE CASCADE,
    tx_index       INTEGER      NOT NULL,
    version        INTEGER      NOT NULL,
    cell_deps      JSONB        NOT NULL,
    header_deps    JSONB        NOT NULL,
    witnesses      JSONB        NOT NULL,
    inputs_count   INTEGER      NOT NULL,
    outputs_count  INTEGER      NOT NULL,
    indexed_at     TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE INDEX transactions_block_idx ON transactions (block_number, tx_index);

CREATE TABLE cells (
    tx_hash                   BYTEA     NOT NULL,
    output_index              INTEGER   NOT NULL,
    block_number              BIGINT    NOT NULL REFERENCES blocks(number) ON DELETE CASCADE,
    capacity_shannons         BIGINT    NOT NULL,
    lock_code_hash            BYTEA     NOT NULL,
    lock_hash_type            SMALLINT  NOT NULL,
    lock_args                 BYTEA     NOT NULL,
    lock_hash                 BYTEA     NOT NULL,
    type_code_hash            BYTEA     NULL,
    type_hash_type            SMALLINT  NULL,
    type_args                 BYTEA     NULL,
    type_hash                 BYTEA     NULL,
    data                      BYTEA     NOT NULL,
    consumed_by_tx_hash       BYTEA     NULL,
    consumed_by_input_index   INTEGER   NULL,
    consumed_at_block_number  BIGINT    NULL,
    PRIMARY KEY (tx_hash, output_index)
);

CREATE INDEX cells_lock_hash_idx ON cells (lock_hash);
CREATE INDEX cells_type_hash_idx ON cells (type_hash)           WHERE type_hash IS NOT NULL;
CREATE INDEX cells_block_idx     ON cells (block_number);
CREATE INDEX cells_consumed_idx  ON cells (consumed_by_tx_hash) WHERE consumed_by_tx_hash IS NOT NULL;

CREATE TABLE indexer_state (
    id                  SMALLINT     PRIMARY KEY CHECK (id = 1),
    last_indexed_block  BIGINT       NOT NULL,
    last_indexed_hash   BYTEA        NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT now()
);
