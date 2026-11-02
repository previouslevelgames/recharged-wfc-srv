-- The game a player's Recharged plays now (docs/profiles.md, "Activity"): the
-- first four characters of its id (R4QE, R4QP, R4QJ, R4QK), from the
-- `Recharged-Game` header; NULL before a launcher or game said it. Sets and
-- searches pair only players of the same game: a US and an EU game never
-- meet in RANKED.
ALTER TABLE players ADD COLUMN game TEXT CHECK (game ~ '^[A-Z0-9]{4}$');
