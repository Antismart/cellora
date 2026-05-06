-- Week 5 — dashboard sessions.
--
-- The dashboard authenticates users via GitHub OAuth (wired up in the next
-- slice). This migration adds the persistence for the user record and an
-- opaque server-side session keyed by a hash of the session cookie.
--
-- We deliberately store SHA-256(token), not the token itself. A database
-- compromise must not yield ready-to-use sessions: the cookie value lives
-- only in the user's browser and the response that issued it.

CREATE TABLE users (
    id              UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    github_user_id  BIGINT       NOT NULL UNIQUE,
    github_login    TEXT         NOT NULL,
    email           TEXT         NULL,
    avatar_url      TEXT         NULL,
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE INDEX users_github_login_idx ON users (github_login);

CREATE TABLE sessions (
    -- SHA-256 of the random session token, hex-encoded (64 chars). The
    -- plaintext token is the cookie value and is never persisted.
    token_hash    TEXT          PRIMARY KEY,
    user_id       UUID          NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT now(),
    expires_at    TIMESTAMPTZ   NOT NULL,
    last_seen_at  TIMESTAMPTZ   NOT NULL DEFAULT now(),
    user_agent    TEXT          NULL
);

CREATE INDEX sessions_user_id_idx ON sessions (user_id);
CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);
