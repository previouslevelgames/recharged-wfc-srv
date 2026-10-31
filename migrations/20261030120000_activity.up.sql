-- What a player's Recharged was doing at its last heartbeat (docs/profiles.md,
-- "Activity"): NULL is the launcher, 'game' the game, 'match' a match in it.
ALTER TABLE players ADD COLUMN activity TEXT CHECK (activity IN ('game', 'match'));
