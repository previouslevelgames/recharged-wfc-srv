-- Searches for a random opponent (docs/profiles.md, "Random opponents"): the
-- searcher's rules and game, the players asked so far, and the random
-- challenge waiting for an answer.
CREATE TABLE searches (
    player_id  BIGINT      PRIMARY KEY REFERENCES players ON DELETE CASCADE,
    rule_set   TEXT        NOT NULL,
    rules      TEXT        NOT NULL CHECK (rules::jsonb IS NOT NULL),
    rules_hash TEXT        NOT NULL,
    -- The searcher's game (its first four characters, R4QE): opponents play the same.
    game       TEXT        NOT NULL,
    asked      BIGINT[]    NOT NULL DEFAULT '{}',
    match_id   BIGINT      REFERENCES matches ON DELETE SET NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A challenge the server sent for a search.
ALTER TABLE matches ADD COLUMN random BOOLEAN NOT NULL DEFAULT false;
