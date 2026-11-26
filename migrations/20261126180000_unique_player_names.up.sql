-- One name, one player (docs/profiles.md, "Names"): nobody can take another
-- player's name, in other letter case either. Players who share a name today
-- keep it in the oldest one; the later ones get a number after it ("Mario 2")
-- and can choose another name in the launcher.
DO $$
DECLARE
    later RECORD;
    number INT;
    candidate TEXT;
BEGIN
    FOR later IN
        SELECT player_id, display_name
        FROM (SELECT player_id, display_name,
                     row_number() OVER (PARTITION BY lower(display_name) ORDER BY player_id) AS place
              FROM players) named
        WHERE place > 1
        ORDER BY player_id
    LOOP
        number := 2;
        LOOP
            candidate := left(later.display_name, 32 - length(' ' || number)) || ' ' || number;
            EXIT WHEN NOT EXISTS (SELECT 1 FROM players WHERE lower(display_name) = lower(candidate));
            number := number + 1;
        END LOOP;
        UPDATE players SET display_name = candidate WHERE player_id = later.player_id;
    END LOOP;
END $$;

CREATE UNIQUE INDEX players_by_name ON players (lower(display_name));
