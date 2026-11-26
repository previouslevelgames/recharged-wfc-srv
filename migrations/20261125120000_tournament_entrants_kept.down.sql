DELETE FROM tournament_entrants WHERE player_id IS NULL;
ALTER TABLE tournament_entrants DROP CONSTRAINT tournament_entrants_player;
ALTER TABLE tournament_entrants DROP CONSTRAINT tournament_entrants_player_id_fkey;
ALTER TABLE tournament_entrants ADD CONSTRAINT tournament_entrants_player_id_fkey
    FOREIGN KEY (player_id) REFERENCES players ON DELETE CASCADE;
ALTER TABLE tournament_entrants ALTER COLUMN player_id SET NOT NULL;
ALTER TABLE tournament_entrants ADD PRIMARY KEY (tournament_id, player_id);
