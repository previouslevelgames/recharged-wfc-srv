-- The profile's own face (docs/profiles.md, "Miis"): the Mii the player chose
-- in the launcher, the same on every computer and in every game version. A
-- player without one shows by the Mii of their WFC profile that logged in last.
ALTER TABLE players
    ADD COLUMN mii_data      BYTEA CHECK (octet_length(mii_data) = 74),
    ADD COLUMN mii_stored_at TIMESTAMPTZ;
