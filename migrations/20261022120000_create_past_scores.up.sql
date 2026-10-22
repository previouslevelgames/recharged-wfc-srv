-- Each player's last row of a season, or of a day on striker of the day, that
-- has ended (docs/gamestats2.md, "Seasons"). The game puts a score of 0 when a
-- new one starts, which replaces its row in `scores`: the row is kept here
-- first.
CREATE TABLE past_scores (
    profile_id INTEGER     NOT NULL,
    category   INTEGER     NOT NULL CHECK (category BETWEEN 0 AND 4),
    -- The season's first day, or the day, by the game's clock: the date in
    -- the userdata.
    first_day  DATE        NOT NULL,
    region     INTEGER     NOT NULL CHECK (region IN (1, 2, 4, 8)),
    score      INTEGER     NOT NULL CHECK (score BETWEEN 0 AND 999999),
    userdata   BYTEA       NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (category, first_day, profile_id)
);
