-- When each side's game last said it is ready for the set, even if the server
-- refused it (docs/profiles.md, "Meeting in RANKED"). A playing set with a
-- game gone quiet before a report was broken off: it ends without a result
-- ("Sets").
ALTER TABLE matches
    ADD COLUMN home_game_ready_at TIMESTAMPTZ,
    ADD COLUMN away_game_ready_at TIMESTAMPTZ;
-- The sets playing now: their games say it again once the server is back.
UPDATE matches SET home_game_ready_at = now(), away_game_ready_at = now()
WHERE state = 'accepted' AND playing_at IS NOT NULL;
