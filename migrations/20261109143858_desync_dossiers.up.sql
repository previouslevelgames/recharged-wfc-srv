CREATE TABLE desync_dossiers (
    match_id    BIGINT  NOT NULL REFERENCES online_matches ON DELETE CASCADE,
    side        TEXT    NOT NULL CHECK (side IN ('home','away')),
    game        INTEGER NOT NULL CHECK (game BETWEEN 1 AND 9),
    profile_id  INTEGER NOT NULL,
    release     TEXT NOT NULL, build TEXT NOT NULL,
    -- The game module's kind (linux x86_64, windows x86_64...); '' unknown.
    platform    TEXT NOT NULL,
    frame       INTEGER NOT NULL,
    written_at  TIMESTAMPTZ NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    body        BYTEA NOT NULL CHECK (octet_length(body) <= 28856),
    PRIMARY KEY (match_id, game, side)
);
-- Filled once both sides are in.
CREATE TABLE desync_divergences (
    match_id    BIGINT  NOT NULL,
    game        INTEGER NOT NULL,
    kind        TEXT    NOT NULL CHECK (kind IN ('path','value','count','none','missing')),
    frame       INTEGER,          -- the step where it diverges
    field_index INTEGER,
    home_caller BIGINT, away_caller BIGINT,
    home_size   BIGINT, away_size   BIGINT,
    home_hash   BIGINT, away_hash   BIGINT,
    home_bytes  BYTEA,  away_bytes  BYTEA,
    PRIMARY KEY (match_id, game),
    FOREIGN KEY (match_id) REFERENCES online_matches ON DELETE CASCADE
);
