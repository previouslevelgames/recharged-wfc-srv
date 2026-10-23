-- Which WFC profiles (docs/login.md) belong to which player
-- (docs/profiles.md, "Friend codes"): the port proves its GPCM login with
-- the login's ticket. A profile is one player's at a time; the last to prove
-- it keeps it.
CREATE TABLE player_wfc_profiles (
    profile_id INTEGER     PRIMARY KEY REFERENCES profiles ON DELETE CASCADE,
    player_id  BIGINT      NOT NULL REFERENCES players ON DELETE CASCADE,
    bound_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX player_wfc_profiles_by_player ON player_wfc_profiles (player_id);

-- The rules a match is played by, which the server's admins set
-- (docs/profiles.md, "Rules"): gameplay tweaks, skill level, game length,
-- games in the series, stadium. Both games apply the same copy.
CREATE TABLE rule_sets (
    rule_set_id BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name        TEXT        NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 48),
    -- JSON (docs/profiles.md, "Rules"), checked by the server.
    rules       TEXT        NOT NULL CHECK (rules::jsonb IS NOT NULL),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- The game's own ranked rules, and the usual competitive ones (the
-- launcher's Competitive gameplay card).
INSERT INTO rule_sets (name, rules) VALUES
    ('Retail ranked', '{"tweaks": [], "skill": "rookie", "limit": "time", "time": 180, "goals": 5, "games": 3, "stadium": "random"}'),
    ('Competitive', '{"tweaks": ["no_mega_strikes", "fast_terrain", "thunder_island_walls"], "skill": "rookie", "limit": "time", "time": 180, "goals": 5, "games": 3, "stadium": "random"}');

-- Matches between two players (docs/profiles.md, "Matches"). `test`: one
-- player challenged the other in the launcher.
CREATE TABLE matches (
    match_id    BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind        TEXT        NOT NULL CHECK (kind IN ('test')),
    state       TEXT        NOT NULL CHECK (state IN
        ('challenged', 'accepted', 'declined', 'cancelled', 'expired', 'finished', 'disputed')),
    -- The challenger and the challenged.
    home        BIGINT      NOT NULL REFERENCES players ON DELETE CASCADE,
    away        BIGINT      NOT NULL REFERENCES players ON DELETE CASCADE CHECK (away <> home),
    rule_set    TEXT        NOT NULL,
    -- The rules as they were when the match was made, and their hash, which
    -- both games confirm.
    rules       TEXT        NOT NULL CHECK (rules::jsonb IS NOT NULL),
    rules_hash  TEXT        NOT NULL,
    -- Who won the series: home or away, or NULL for a draw or no result yet.
    winner      BIGINT      REFERENCES players ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX matches_by_home ON matches (home, created_at DESC);
CREATE INDEX matches_by_away ON matches (away, created_at DESC);

-- What each player's game reported of a match: every game's score, captains
-- and stadium, as that player saw it.
CREATE TABLE match_reports (
    match_id    BIGINT      NOT NULL REFERENCES matches ON DELETE CASCADE,
    player_id   BIGINT      NOT NULL REFERENCES players ON DELETE CASCADE,
    -- JSON: one object per game (docs/profiles.md, "Results").
    games       TEXT        NOT NULL CHECK (games::jsonb IS NOT NULL),
    reported_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (match_id, player_id)
);
