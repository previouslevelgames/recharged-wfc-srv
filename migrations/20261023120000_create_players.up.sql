-- Recharged profiles (docs/profiles.md): a player, made the first time
-- someone signs in from the launcher. The game's own identities stay in
-- `accounts` and `profiles`.
CREATE TABLE players (
    player_id    BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- Shown to other players: the account's display name at the last
    -- sign-in.
    display_name TEXT        NOT NULL CHECK (length(display_name) BETWEEN 1 AND 64),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The accounts a player signs in with: `discord`, or `test` on test servers
-- (PROFILES_TEST_SIGN_IN). At most one of each per player.
CREATE TABLE player_identities (
    provider    TEXT        NOT NULL CHECK (provider IN ('discord', 'test')),
    -- Discord's user id, or the test name in lowercase.
    external_id TEXT        NOT NULL,
    player_id   BIGINT      NOT NULL REFERENCES players ON DELETE CASCADE,
    -- The account's unique name, which challenges look players up by:
    -- Discord's username, or the test name.
    username    TEXT        NOT NULL,
    avatar_url  TEXT,
    linked_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, external_id),
    UNIQUE (player_id, provider)
);
CREATE INDEX player_identities_by_username ON player_identities (provider, lower(username));

-- The launchers' sessions. Only the token's SHA-256 is kept.
CREATE TABLE player_sessions (
    token_hash   BYTEA       PRIMARY KEY CHECK (octet_length(token_hash) = 32),
    player_id    BIGINT      NOT NULL REFERENCES players ON DELETE CASCADE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at   TIMESTAMPTZ NOT NULL
);
CREATE INDEX player_sessions_by_player ON player_sessions (player_id);
