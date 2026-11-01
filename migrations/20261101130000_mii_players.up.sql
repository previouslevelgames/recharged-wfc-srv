-- Players without Discord (docs/profiles.md, "Signing in"): a Mii player
-- has no account to sign in with at first. The first WFC profile it proves
-- becomes its `wfc` identity, which signs it in again from the game's WFC
-- login. A WFC profile identifies one player at most (the primary key).
-- `username` is the profile's friend code.
ALTER TABLE player_identities
    DROP CONSTRAINT player_identities_provider_check,
    ADD CONSTRAINT player_identities_provider_check
        CHECK (provider IN ('discord', 'test', 'wfc'));
