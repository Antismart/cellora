CREATE TABLE api_request_logs (
    id            BIGSERIAL PRIMARY KEY,
    timestamp     TIMESTAMPTZ NOT NULL DEFAULT now(),
    api_key_id    UUID NOT NULL REFERENCES api_keys(id) ON DELETE CASCADE,
    method        TEXT NOT NULL,
    path          TEXT NOT NULL,
    status_code   SMALLINT NOT NULL,
    latency_ms    INTEGER NOT NULL
);

CREATE INDEX api_request_logs_api_key_id_timestamp_idx 
    ON api_request_logs (api_key_id, timestamp DESC);
