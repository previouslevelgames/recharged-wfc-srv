-- Each player keeps its oldest WFC identity.
DELETE FROM player_identities newer
USING player_identities older
WHERE newer.provider = 'wfc' AND older.provider = 'wfc'
  AND newer.player_id = older.player_id
  AND (older.linked_at, older.external_id) < (newer.linked_at, newer.external_id);
DROP INDEX player_identities_one_account;
ALTER TABLE player_identities ADD CONSTRAINT player_identities_player_id_provider_key
    UNIQUE (player_id, provider);
