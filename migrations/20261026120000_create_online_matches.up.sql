-- Online matches as Recharged builds report them (docs/reports.md): every
-- series played online, ranked or with friends. Both games report and the
-- server pairs their sides; a retail game reports nothing, so its side is
-- known by its Mii name only. The profiles' challenges (`matches`) are
-- separate.
CREATE TABLE online_matches (
    match_id       BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- The game code: both games of a match are of one release.
    release        TEXT        NOT NULL CHECK (release IN ('R4QE', 'R4QP', 'R4QJ', 'R4QK')),
    mode           TEXT        NOT NULL CHECK (mode IN ('ranked', 'friends')),
    best_of        INTEGER     NOT NULL CHECK (best_of IN (1, 3, 5, 7)),
    -- Worked out from the sides after every report, and once nothing came
    -- for 30 minutes.
    state          TEXT        NOT NULL DEFAULT 'in_progress'
                               CHECK (state IN ('in_progress', 'finished', 'disputed', 'abandoned')),
    -- Each team's Mii (as its own game named it, else as the other game
    -- showed it) and captain, for the list.
    home_mii       TEXT        NOT NULL,
    away_mii       TEXT        NOT NULL,
    home_captain   TEXT        NOT NULL,
    away_captain   TEXT        NOT NULL,
    -- Games won, of those both sides saw the same or only one reported.
    home_wins      INTEGER     NOT NULL DEFAULT 0,
    away_wins      INTEGER     NOT NULL DEFAULT 0,
    -- Once finished: the side that won more games, NULL for a draw.
    winner         TEXT        CHECK (winner IN ('home', 'away')),
    started_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_report_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at       TIMESTAMPTZ
);
-- The Matches page: in progress first, then the newest.
CREATE INDEX online_matches_listed ON online_matches ((state <> 'in_progress'), started_at DESC);
-- The ones a new side may join, and the ones to abandon.
CREATE INDEX online_matches_in_progress ON online_matches (last_report_at) WHERE state = 'in_progress';

-- A side of a match as its game reported it, one row per game that
-- reported: a side nobody reported (a retail game's) has none.
CREATE TABLE online_match_sides (
    match_id       BIGINT      NOT NULL REFERENCES online_matches ON DELETE CASCADE,
    side           TEXT        NOT NULL CHECK (side IN ('home', 'away')),
    -- The WFC profile the game reported as, and its number for the series:
    -- the series' start time in Unix seconds, by the game's clock.
    profile_id     INTEGER     NOT NULL REFERENCES profiles,
    series         BIGINT      NOT NULL CHECK (series >= 0),
    -- As this side's game showed them.
    mii            TEXT        NOT NULL,
    opponent_mii   TEXT        NOT NULL,
    home_captain   TEXT        NOT NULL,
    away_captain   TEXT        NOT NULL,
    -- Up to three each; fewer, or none, when the game does not know them.
    home_sidekicks TEXT[]      NOT NULL DEFAULT '{}',
    away_sidekicks TEXT[]      NOT NULL DEFAULT '{}',
    -- The end's: `decided` or `abandoned`. NULL until the end came.
    reason         TEXT        CHECK (reason IN ('decided', 'abandoned')),
    opened_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_report_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at       TIMESTAMPTZ,
    PRIMARY KEY (match_id, side),
    -- A report finds its side by the series.
    UNIQUE (profile_id, series)
);
-- A profile's matches, the newest first.
CREATE INDEX online_match_sides_by_profile ON online_match_sides (profile_id, opened_at DESC);

-- A game as one side saw it.
CREATE TABLE online_match_games (
    match_id     BIGINT      NOT NULL,
    side         TEXT        NOT NULL,
    number       INTEGER     NOT NULL CHECK (number BETWEEN 1 AND 7),
    home_score   INTEGER     NOT NULL CHECK (home_score BETWEEN 0 AND 99),
    away_score   INTEGER     NOT NULL CHECK (away_score BETWEEN 0 AND 99),
    home_captain TEXT        NOT NULL,
    away_captain TEXT        NOT NULL,
    -- One of the server's stadiums (src/profiles/rules.rs), or `unknown`.
    stadium      TEXT        NOT NULL,
    -- `normal`, `disconnect` (the score is what this side saw) or `desync`.
    ending       TEXT        NOT NULL CHECK (ending IN ('normal', 'disconnect', 'desync')),
    -- The game's length on its clock.
    seconds      INTEGER     NOT NULL CHECK (seconds >= 0),
    reported_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (match_id, side, number),
    FOREIGN KEY (match_id, side) REFERENCES online_match_sides ON DELETE CASCADE
);

-- The reports as they came, for a match's timeline. One that came before
-- (the same side's same event, for a game the same game) is kept once.
CREATE TABLE online_match_events (
    event_id    BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    match_id    BIGINT      NOT NULL,
    side        TEXT        NOT NULL,
    event       TEXT        NOT NULL CHECK (event IN ('start', 'game', 'end')),
    -- A game's number; NULL for the start and the end.
    game        INTEGER,
    -- The report's JSON as the game sent it.
    body        TEXT        NOT NULL CHECK (body::json IS NOT NULL),
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (match_id, side) REFERENCES online_match_sides ON DELETE CASCADE,
    UNIQUE NULLS NOT DISTINCT (match_id, side, event, game)
);
