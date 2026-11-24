-- Replays of ranked games with a top player (docs/reports.md, "Replays"): a
-- row once the server asked a game's sides for it, in its answer to their
-- reports of the game; the first copy that comes is kept as a file.
CREATE TABLE replays (
    match_id    BIGINT      NOT NULL REFERENCES online_matches ON DELETE CASCADE,
    game        INTEGER     NOT NULL CHECK (game BETWEEN 1 AND 9),
    asked_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Each side's player's best place on the season's board, this season or
    -- the last, when it was in the first REPLAY_TOP; NULL otherwise.
    home_place  INTEGER,
    away_place  INTEGER,
    -- The copy kept: the side that sent it, its file under REPLAY_DIR, its
    -- size, SHA-256 and the game steps it holds. NULL until one came.
    side        TEXT        CHECK (side IN ('home', 'away')),
    file        TEXT,
    bytes       INTEGER,
    sha256      BYTEA,
    steps       INTEGER,
    received_at TIMESTAMPTZ,
    -- The other side sent its copy too (not kept).
    both_sides  BOOLEAN     NOT NULL DEFAULT false,
    PRIMARY KEY (match_id, game),
    CHECK ((file IS NULL) = (side IS NULL))
);
