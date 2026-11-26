-- A set's game that broke off (a disconnect) goes on from where it was: the
-- next game of the set starts from its score, clock, sides, teams and
-- stadium. NULL while there is nothing to go on from.
ALTER TABLE tournament_sets ADD COLUMN resume JSONB;
