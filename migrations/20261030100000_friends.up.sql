-- Each player's friends (docs/profiles.md, "Friends"): their own list,
-- without requests. A challenge adds its opponent, an accepted one its
-- challenger, and the launcher adds and removes players by hand.
CREATE TABLE friends (
    player_id BIGINT      NOT NULL REFERENCES players ON DELETE CASCADE,
    friend_id BIGINT      NOT NULL REFERENCES players ON DELETE CASCADE CHECK (friend_id <> player_id),
    added_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (player_id, friend_id)
);

-- The players of the sets so far: every challenger keeps the challenged, and
-- the challenged keeps a challenger whose challenge they accepted.
INSERT INTO friends (player_id, friend_id, added_at)
SELECT home, away, min(created_at) FROM matches GROUP BY home, away
ON CONFLICT DO NOTHING;
INSERT INTO friends (player_id, friend_id, added_at)
SELECT away, home, min(created_at) FROM matches
WHERE state IN ('accepted', 'finished', 'disputed', 'dissolved')
   OR home_ready_at IS NOT NULL OR away_ready_at IS NOT NULL
GROUP BY away, home
ON CONFLICT DO NOTHING;
