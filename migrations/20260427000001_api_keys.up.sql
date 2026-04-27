-- Week 3 — API key authentication.
--
-- Each issued key has shape `cell_<8 hex prefix>_<32 hex secret>`. Only the
-- prefix is stored in plaintext; the secret half is hashed with Argon2id and
-- compared on every authenticated request. The prefix is the natural primary
-- key — it is unique, stable, and fast to look up by.
--
-- Tier rate-limit numbers are NOT stored on the row. They live in
-- environment-driven config so we can tune limits per environment without
-- writing to the database.

CREATE TYPE api_key_tier AS ENUM ('free', 'starter', 'pro');

CREATE TABLE api_keys (
    prefix        TEXT          PRIMARY KEY,
    secret_hash   TEXT          NOT NULL,
    tier          api_key_tier  NOT NULL,
    label         TEXT          NULL,
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT now(),
    revoked_at    TIMESTAMPTZ   NULL,
    last_used_at  TIMESTAMPTZ   NULL
);

CREATE INDEX api_keys_active_idx ON api_keys (prefix) WHERE revoked_at IS NULL;
