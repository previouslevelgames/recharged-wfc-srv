-- An entrant whose player is deleted stays, without the player, so that the
-- seeds and the bracket of their tournament stay as they were (the admin
-- refuses to delete one still playing a tournament: docs/tournaments.md).
ALTER TABLE tournament_entrants DROP CONSTRAINT tournament_entrants_pkey;
ALTER TABLE tournament_entrants ALTER COLUMN player_id DROP NOT NULL;
ALTER TABLE tournament_entrants DROP CONSTRAINT tournament_entrants_player_id_fkey;
ALTER TABLE tournament_entrants ADD CONSTRAINT tournament_entrants_player_id_fkey
    FOREIGN KEY (player_id) REFERENCES players ON DELETE SET NULL;
ALTER TABLE tournament_entrants ADD CONSTRAINT tournament_entrants_player
    UNIQUE (tournament_id, player_id);
