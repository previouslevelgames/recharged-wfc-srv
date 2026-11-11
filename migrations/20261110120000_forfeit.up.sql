-- A forfeit: a player leaves the match on purpose, and the opponent wins
-- (docs/profiles.md, "Forfeit"; docs/reports.md, "Forfeit").

-- A set the player of `forfeited_by` gave up while it was playing: it is
-- `finished` with the other player as `winner`.
ALTER TABLE matches
    ADD COLUMN forfeited_by BIGINT REFERENCES players ON DELETE SET NULL,
    ADD COLUMN forfeited_at TIMESTAMPTZ;

-- When this side's end said `cause: forfeit` (`cause` keeps its four
-- values).
ALTER TABLE online_match_sides ADD COLUMN forfeited_at TIMESTAMPTZ;

-- The profile whose forfeit decided the match, and when its end came: the
-- match is `finished` with the other side as `winner`.
ALTER TABLE online_matches
    ADD COLUMN forfeited_by INTEGER REFERENCES profiles ON DELETE SET NULL,
    ADD COLUMN forfeited_at TIMESTAMPTZ;
-- The opponent's game asks for the latest forfeit against it.
CREATE INDEX online_matches_forfeited ON online_matches (forfeited_at)
    WHERE forfeited_at IS NOT NULL;
