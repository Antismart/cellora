-- Revert Week 5 slice 5: drop user scoping, restore prefix-keyed schema.
--
-- Down migrations cannot restore the data deleted by the up migration; this
-- reshapes the schema only. Operators reissuing keys after a downgrade do so
-- via the CLI as before.

DELETE FROM api_keys;

DROP INDEX api_keys_active_idx;
DROP INDEX api_keys_user_id_idx;
DROP INDEX api_keys_prefix_uidx;

ALTER TABLE api_keys DROP CONSTRAINT api_keys_pkey;
ALTER TABLE api_keys ADD CONSTRAINT api_keys_pkey PRIMARY KEY (prefix);

ALTER TABLE api_keys
    DROP COLUMN user_id,
    DROP COLUMN id;

CREATE INDEX api_keys_active_idx ON api_keys (prefix) WHERE revoked_at IS NULL;
