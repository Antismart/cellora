-- Week 5 slice 5 — scope API keys to dashboard users.
--
-- Up to this point an API key existed independently of any user — keys were
-- issued by the operator CLI and identified by their prefix. With dashboard
-- self-service the key now belongs to the GitHub-authenticated user who
-- created it, so we add a `user_id` foreign key and a stable surrogate `id`.
--
-- The surrogate `id` is required because rotation issues a fresh prefix and
-- secret on the same logical key. With `prefix` as the PK the rotated row
-- would lose its identity; with `id` it stays addressable at the same URL.
--
-- Existing rows are pre-prod alpha CLI keys with no owning user, and there
-- is no real user to backfill them to. They are deleted; operators can
-- reissue from the CLI now that `--github-login` is required.

DELETE FROM api_keys;

ALTER TABLE api_keys
    ADD COLUMN id      UUID NOT NULL DEFAULT gen_random_uuid(),
    ADD COLUMN user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE;

ALTER TABLE api_keys DROP CONSTRAINT api_keys_pkey;
ALTER TABLE api_keys ADD CONSTRAINT api_keys_pkey PRIMARY KEY (id);

CREATE UNIQUE INDEX api_keys_prefix_uidx ON api_keys (prefix);
CREATE INDEX api_keys_user_id_idx ON api_keys (user_id);

DROP INDEX api_keys_active_idx;
CREATE INDEX api_keys_active_idx ON api_keys (prefix) WHERE revoked_at IS NULL;
