-- The matches of a tournament set: its page's games, its READY and calling
-- a tournament off look them up by it.
CREATE INDEX matches_by_tournament_set ON matches (tournament_set_id)
    WHERE tournament_set_id IS NOT NULL;
