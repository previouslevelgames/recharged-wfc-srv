-- Players available for random games (docs/profiles.md, "Available for random
-- games"): while their launcher is open, other available players see them.
ALTER TABLE players ADD COLUMN available BOOLEAN NOT NULL DEFAULT false;
