DROP INDEX players_by_heartbeat;
ALTER TABLE players DROP COLUMN heartbeat_at;
DROP INDEX matches_open;
UPDATE matches SET state = 'cancelled' WHERE state = 'dissolved';
ALTER TABLE matches
    DROP CONSTRAINT matches_state_check,
    ADD CONSTRAINT matches_state_check CHECK (state IN
        ('challenged', 'accepted', 'declined', 'cancelled', 'expired', 'finished', 'disputed'));
ALTER TABLE matches
    DROP COLUMN rematch_of,
    DROP COLUMN playing_at,
    DROP COLUMN go_at,
    DROP COLUMN away_ready_at,
    DROP COLUMN home_ready_at;
