-- Week 4 — audit log for chain reorgs.
--
-- One row per detected reorg. Inserted as `in_progress` at detection
-- time, transitioned to `completed` (or `failed`) inside the same
-- database transaction that performs the rollback, so the log can never
-- disagree with the data.

CREATE TYPE reorg_status AS ENUM ('in_progress', 'completed', 'failed');

CREATE TABLE reorg_log (
    id                       BIGSERIAL    PRIMARY KEY,
    detected_at              TIMESTAMPTZ  NOT NULL DEFAULT now(),
    divergence_block_number  BIGINT       NOT NULL,
    divergence_node_hash     BYTEA        NOT NULL,
    divergence_indexed_hash  BYTEA        NOT NULL,
    depth                    INTEGER      NOT NULL,
    completed_at             TIMESTAMPTZ  NULL,
    status                   reorg_status NOT NULL DEFAULT 'in_progress',
    error                    TEXT         NULL
);

CREATE INDEX reorg_log_detected_idx ON reorg_log (detected_at DESC);
