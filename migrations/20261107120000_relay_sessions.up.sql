-- The pairs of games the NAT relay carried (docs/install.md, "NAT relay"),
-- one row per relayed negotiation, kept 30 days: the Matches page marks
-- their matches, a match's page and a player's page show them.
CREATE TABLE relay_sessions (
    relay_id       BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- The server that relayed it, by its host name (as `online_samples`).
    server         TEXT        NOT NULL,
    -- The negotiation's cookie, as the log shows it (8 hex digits).
    cookie         TEXT        NOT NULL,
    -- Why the relay took the pair: `always` (NATNEG_RELAY=always),
    -- `new_port` (a game's router gives it a new port per address) or
    -- `failed_before` (the two public addresses failed without it lately).
    reason         TEXT        NOT NULL CHECK (reason IN ('always', 'new_port', 'failed_before')),
    -- The two games by client index: their game sockets' public addresses,
    -- and the profiles playing there (the master server's session at that
    -- socket, else the only GPCM login from that IP); NULL when unknown.
    first_address  TEXT        NOT NULL,
    second_address TEXT        NOT NULL,
    first_profile  INTEGER     REFERENCES profiles ON DELETE SET NULL,
    second_profile INTEGER     REFERENCES profiles ON DELETE SET NULL,
    started_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The last packet relayed, once the relay let the pair go; NULL while it
    -- carries them.
    ended_at       TIMESTAMPTZ,
    -- `idle` (nothing came for a minute), `replaced` (one of the games began
    -- another relayed negotiation) or `restart` (the server stopped first:
    -- `ended_at` is then when it came back).
    ended_why      TEXT        CHECK (ended_why IN ('idle', 'replaced', 'restart')),
    -- Relayed between the two, either way.
    packets        BIGINT      NOT NULL DEFAULT 0 CHECK (packets >= 0),
    bytes          BIGINT      NOT NULL DEFAULT 0 CHECK (bytes >= 0)
);
-- A profile's relayed games, and a match's.
CREATE INDEX relay_sessions_first ON relay_sessions (first_profile, started_at);
CREATE INDEX relay_sessions_second ON relay_sessions (second_profile, started_at);
-- The ones to let go.
CREATE INDEX relay_sessions_by_time ON relay_sessions (started_at);
