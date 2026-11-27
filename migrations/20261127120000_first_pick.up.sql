-- A set's turn (docs/profiles.md, "Sets"): the player who drafts first in
-- its games, and whose side follows from it. A challenge's challenger, and
-- in each rematch the other player than in the set it plays again, who also
-- picks its stadium. NULL: sets from before turns, and tournament sets
-- (their bracket gives the home side).
ALTER TABLE matches ADD COLUMN first_pick BIGINT REFERENCES players ON DELETE SET NULL;
