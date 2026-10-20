-- gamestats2 leaderboards (docs/gamestats2.md): one row per player and
-- category, replaced by every put2. The client sends its whole running total,
-- so the latest put is the score.
CREATE TABLE scores (
    -- GameSpy profile id, as sent in the request. No foreign key while the
    -- users live in wfc-server.
    profile_id INTEGER     NOT NULL,
    -- NETWORK_PERSISTENT_*: 0 season, 1 striker of the day, 2 friends (EU),
    -- 3 and 4 the EU alternate country group.
    category   INTEGER     NOT NULL CHECK (category BETWEEN 0 AND 4),
    -- DWCRnkRegion of the player's game: JP 1, US 2, EU 4, KR 8.
    region     INTEGER     NOT NULL CHECK (region IN (1, 2, 4, 8)),
    -- The game clamps it to 999999.
    score      INTEGER     NOT NULL CHECK (score BETWEEN 0 AND 999999),
    -- NetworkRankingSubmission, kept byte for byte: readers check its HMAC.
    -- The client walks rows in 4-byte words, so the size is a multiple of 4.
    userdata   BYTEA       NOT NULL CHECK (octet_length(userdata) <= 768
                                       AND octet_length(userdata) % 4 = 0),
    -- Set by the database on every put: get2's `since` filters on it.
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (profile_id, category)
);

-- get2 reads one category, best score first, earlier update first on a tie.
CREATE INDEX scores_by_rank ON scores (category, score DESC, updated_at);
