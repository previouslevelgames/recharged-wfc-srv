-- The Mii profiles a player keeps as friends (docs/profiles.md, "Mii
-- profiles"): WFC profiles of the game's own online mode, by profile.
CREATE TABLE friend_profiles (
    player_id  BIGINT      NOT NULL REFERENCES players ON DELETE CASCADE,
    profile_id INTEGER     NOT NULL REFERENCES profiles ON DELETE CASCADE,
    added_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (player_id, profile_id)
);
