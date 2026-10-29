-- Sets (docs/profiles.md, "Sets"): a match's two players press Ready to play!
-- in their launchers, the server says go, the games meet in RANKED and play.
-- These are stages of an accepted match, so older launchers and games still
-- see `accepted`.
ALTER TABLE matches
    -- When each player pressed Ready to play!; NULL: not ready.
    ADD COLUMN home_ready_at TIMESTAMPTZ,
    ADD COLUMN away_ready_at TIMESTAMPTZ,
    -- Both were ready: the launchers start the games.
    ADD COLUMN go_at         TIMESTAMPTZ,
    -- The two games found each other in RANKED.
    ADD COLUMN playing_at    TIMESTAMPTZ,
    -- The set this one plays again.
    ADD COLUMN rematch_of    BIGINT REFERENCES matches ON DELETE SET NULL;
-- `dissolved`: a player's Recharged went away (no heartbeat) before a result.
ALTER TABLE matches
    DROP CONSTRAINT matches_state_check,
    ADD CONSTRAINT matches_state_check CHECK (state IN
        ('challenged', 'accepted', 'declined', 'cancelled', 'expired', 'dissolved', 'finished', 'disputed'));
-- The sets still to be played, which every request sweeps.
CREATE INDEX matches_open ON matches (match_id) WHERE state IN ('challenged', 'accepted');

-- The last request of the player's launcher or game: who has Recharged open
-- ("Online now"), and whose sets end when it closes.
ALTER TABLE players ADD COLUMN heartbeat_at TIMESTAMPTZ;
CREATE INDEX players_by_heartbeat ON players (heartbeat_at);
