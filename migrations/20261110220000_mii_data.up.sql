-- A WFC profile's Mii (docs/profiles.md, "Miis"): its RFLCharData, the 74
-- bytes the game keeps for it, which Recharged shows players by. Only the
-- launcher of the player the profile is bound to (`player_wfc_profiles`)
-- sends it; a Wii's profile never has one.
ALTER TABLE profiles
    ADD COLUMN mii_data      BYTEA CHECK (octet_length(mii_data) = 74),
    -- When it was last sent.
    ADD COLUMN mii_stored_at TIMESTAMPTZ;
