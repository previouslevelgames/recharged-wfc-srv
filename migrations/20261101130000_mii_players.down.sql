DELETE FROM player_identities WHERE provider = 'wfc';
ALTER TABLE player_identities
    DROP CONSTRAINT player_identities_provider_check,
    ADD CONSTRAINT player_identities_provider_check
        CHECK (provider IN ('discord', 'test'));
