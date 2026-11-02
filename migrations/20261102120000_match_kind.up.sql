-- The kind of match a player's game was in at its last heartbeat
-- (docs/profiles.md, "Activity"); NULL outside a match and from games that
-- don't say.
ALTER TABLE players ADD COLUMN match_kind TEXT CHECK (
    match_kind IN ('ranked', 'friends', 'tournament', 'cpu', 'local', 'cup', 'challenge', 'training'));
