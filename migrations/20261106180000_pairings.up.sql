-- The games kept for a set in RANKED (docs/profiles.md, "Meeting in
-- RANKED"): each game that said it is ready, by set and side, and when it
-- last did. Any node takes a game's ready call and any node's server browser
-- lists by them (docs/nodes.md). A game that another of its player's games
-- replaced stays, marked, until it stops saying it is ready.
CREATE TABLE pairings (
    match_id   BIGINT      NOT NULL,
    side       SMALLINT    NOT NULL CHECK (side IN (0, 1)),
    profile_id INTEGER     NOT NULL,
    -- The release, R4QE or R4QP.
    game       TEXT        NOT NULL,
    ready_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    replaced   BOOLEAN     NOT NULL DEFAULT false,
    PRIMARY KEY (match_id, side, profile_id)
);
CREATE INDEX ON pairings (profile_id);
-- A side has one game in the set.
CREATE UNIQUE INDEX ON pairings (match_id, side) WHERE NOT replaced;
