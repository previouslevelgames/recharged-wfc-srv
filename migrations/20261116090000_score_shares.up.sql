-- A put counts the change from what its game held (docs/gamestats2.md, "What
-- a put counts"): a row of `scores` is its profile's share of its player's
-- total, no longer the game's whole running total, and the game is shown the
-- player's total. What each profile's game holds, the last total it was
-- shown (a NEAR answer, which the game takes as its own) or put, is kept
-- with the row; NULL until the server sees one.
ALTER TABLE scores
    ADD COLUMN held_score  INTEGER,
    ADD COLUMN held_wins   INTEGER,
    ADD COLUMN held_losses INTEGER,
    -- The date the game held it on, by the game's own clock: a put dated in
    -- a later season (or day) by that clock starts from nothing.
    ADD COLUMN held_on     DATE,
    -- The points a provisional loss took at kickoff
    -- (`ReportDefaultDisconnectLoss`), not counted until the series shows
    -- whether its result gives them back; NULL without one.
    ADD COLUMN provisional INTEGER;

-- Every game puts and reads the season's (0) and the day's (1) boards: the EU
-- alternate country group's rows (3 and 4) join them, where their profile has
-- none there yet. There were none when this was written.
UPDATE scores alternate SET category = alternate.category - 3
WHERE category IN (3, 4)
  AND NOT EXISTS (SELECT 1 FROM scores main
                  WHERE main.profile_id = alternate.profile_id
                    AND main.category = alternate.category - 3);

-- Every put and what the server made of it, so the boards can be checked and
-- counted again without the logs: `counted` on the boards, `set` (a
-- Recharged set's game, kept off them) or `deleted` (its scores were
-- deleted while its game was logged in).
CREATE TABLE score_puts (
    put_id        BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    profile_id    INTEGER     NOT NULL,
    -- As the game put it, before the EU categories map to the main boards.
    category      INTEGER     NOT NULL,
    at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    outcome       TEXT        NOT NULL CHECK (outcome IN ('counted', 'set', 'deleted')),
    -- The game's total as put; NULL wins and losses when its userdata has
    -- no record.
    score         INTEGER     NOT NULL,
    wins          INTEGER,
    losses        INTEGER,
    -- What it added to its player's total; NULL unless counted.
    score_change  INTEGER,
    wins_change   INTEGER,
    losses_change INTEGER
);
CREATE INDEX score_puts_by_profile ON score_puts (profile_id, at);
