-- What a player's game shows (docs/profiles.md, "Activity"): the screens it
-- is on, from its `Recharged-Screen` header, and in a set the game of the
-- series it plays, from `Recharged-Set-Game`; NULL when it doesn't say.
ALTER TABLE players
    ADD COLUMN screen TEXT CHECK (screen ~ '^[a-z_]{1,24}$'),
    ADD COLUMN set_game SMALLINT CHECK (set_game BETWEEN 1 AND 7);
