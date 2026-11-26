-- Every WFC profile a player proves first identifies that player
-- (docs/profiles.md, "Coming back"): Discord players' too, and a player with
-- a Mii of each game version has several. Discord and test stay one each.
ALTER TABLE player_identities DROP CONSTRAINT player_identities_player_id_provider_key;
CREATE UNIQUE INDEX player_identities_one_account ON player_identities (player_id, provider)
    WHERE provider <> 'wfc';

-- A WFC profile's friend code, as accounts::friend_code makes it.
CREATE FUNCTION pg_temp.friend_code(profile_id INT, game TEXT) RETURNS TEXT AS $$
DECLARE
    id BIGINT := profile_id::BIGINT & 4294967295;
    crc INT := 0;
    byte INT;
    digits TEXT;
BEGIN
    FOREACH byte IN ARRAY ARRAY[id & 255, (id >> 8) & 255, (id >> 16) & 255, (id >> 24) & 255,
                                ascii(substr(game, 4, 1)), ascii(substr(game, 3, 1)),
                                ascii(substr(game, 2, 1)), ascii(substr(game, 1, 1))] LOOP
        crc := crc # byte;
        FOR bit IN 1..8 LOOP
            IF crc & 128 = 0 THEN crc := (crc << 1) & 255; ELSE crc := ((crc << 1) # 7) & 255; END IF;
        END LOOP;
    END LOOP;
    digits := lpad((((crc & 127)::BIGINT << 32) | id)::TEXT, 12, '0');
    RETURN substr(digits, 1, 4) || '-' || substr(digits, 5, 4) || '-' || substr(digits, 9, 4);
END $$ LANGUAGE plpgsql;

-- The bound profiles that identify nobody identify their player now.
INSERT INTO player_identities (provider, external_id, player_id, username)
SELECT 'wfc', bound.profile_id::TEXT, bound.player_id, pg_temp.friend_code(bound.profile_id, profiles.gsbrcd)
FROM player_wfc_profiles bound
JOIN profiles USING (profile_id)
ON CONFLICT DO NOTHING;
