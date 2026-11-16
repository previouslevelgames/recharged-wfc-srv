-- Tournaments the server runs (docs/tournaments.md): a bracket of single or
-- double elimination whose sets are played as matches between two players
-- who signed in with Discord.

CREATE TABLE tournaments (
    tournament_id  BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name           TEXT        NOT NULL CHECK (length(name) BETWEEN 1 AND 64),
    -- The player who runs it; NULL: the server's admins.
    organizer      BIGINT      REFERENCES players ON DELETE SET NULL,
    format         TEXT        NOT NULL CHECK (format IN ('single', 'double')),
    state          TEXT        NOT NULL DEFAULT 'registration'
                               CHECK (state IN ('registration', 'running', 'finished', 'cancelled')),
    -- The rules its matches are played by (docs/profiles.md, "Rules"), and
    -- the name of the rule set they come from; each set plays its own
    -- number of games.
    rule_set       TEXT        NOT NULL,
    rules          TEXT        NOT NULL CHECK (rules::jsonb IS NOT NULL),
    -- Games of a set, and of a final (the winners', the losers', the grand
    -- final and its reset).
    best_of        SMALLINT    NOT NULL CHECK (best_of IN (1, 3, 5, 7)),
    finals_best_of SMALLINT    NOT NULL CHECK (finals_best_of IN (1, 3, 5, 7)),
    max_entrants   INTEGER     CHECK (max_entrants BETWEEN 2 AND 256),
    -- When it is meant to start, to show; the organizer starts it.
    starts_at      TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at     TIMESTAMPTZ,
    ended_at       TIMESTAMPTZ
);
CREATE INDEX tournaments_by_state ON tournaments (state, created_at DESC);

CREATE TABLE tournament_entrants (
    tournament_id BIGINT      NOT NULL REFERENCES tournaments ON DELETE CASCADE,
    player_id     BIGINT      NOT NULL REFERENCES players ON DELETE CASCADE,
    -- From 0, given when the tournament starts.
    seed          INTEGER     CHECK (seed >= 0),
    joined_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- They left, or the organizer disqualified them: they lose their open
    -- sets.
    dropped_at    TIMESTAMPTZ,
    -- From 1, once the tournament is over.
    place         INTEGER,
    PRIMARY KEY (tournament_id, player_id),
    UNIQUE (tournament_id, seed)
);
CREATE INDEX tournament_entrants_by_player ON tournament_entrants (player_id);

-- The sets of a started tournament's bracket, in its order. Where their
-- players come from follows from the format and the number of entrants;
-- `result` (and who left) is what decides the rest, which is kept here as
-- it was worked out last.
CREATE TABLE tournament_sets (
    tournament_set_id BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tournament_id     BIGINT      NOT NULL REFERENCES tournaments ON DELETE CASCADE,
    index             INTEGER     NOT NULL CHECK (index >= 0),
    side              TEXT        NOT NULL
                                  CHECK (side IN ('winners', 'losers', 'grand_final', 'grand_final_reset')),
    round             INTEGER     NOT NULL CHECK (round >= 1),
    position          INTEGER     NOT NULL CHECK (position >= 0),
    games             SMALLINT    NOT NULL CHECK (games IN (1, 3, 5, 7)),
    -- `pending`: waiting for its players; `ready`: to be played; `playing`:
    -- its match is open; `finished`; `disputed`: the organizer decides;
    -- `void`: nobody plays it.
    state             TEXT        NOT NULL DEFAULT 'pending'
                                  CHECK (state IN ('pending', 'ready', 'playing', 'finished', 'disputed', 'void')),
    home              BIGINT      REFERENCES players ON DELETE SET NULL,
    away              BIGINT      REFERENCES players ON DELETE SET NULL,
    -- The place (0 home, 1 away) of the winner of a set played or decided
    -- by the organizer.
    result            SMALLINT    CHECK (result IN (0, 1)),
    -- How it was decided: `match`, `organizer`, or `walkover` (nobody came,
    -- or the other left).
    decided_by        TEXT        CHECK (decided_by IN ('match', 'organizer', 'walkover')),
    winner            BIGINT      REFERENCES players ON DELETE SET NULL,
    -- Games won by each.
    home_wins         SMALLINT,
    away_wins         SMALLINT,
    -- The match it is played by now, or was last.
    match_id          BIGINT      REFERENCES matches ON DELETE SET NULL,
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tournament_id, index)
);
CREATE INDEX tournament_sets_playing ON tournament_sets (match_id) WHERE state = 'playing';
CREATE INDEX tournament_sets_by_home ON tournament_sets (home) WHERE state IN ('ready', 'playing');
CREATE INDEX tournament_sets_by_away ON tournament_sets (away) WHERE state IN ('ready', 'playing');

-- The tournament set a match plays; NULL for a challenge or a search's.
ALTER TABLE matches
    ADD COLUMN tournament_set_id BIGINT REFERENCES tournament_sets ON DELETE SET NULL;
