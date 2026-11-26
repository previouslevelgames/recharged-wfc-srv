-- A WFC profile's slot of the game's online save (docs/profiles.md,
-- "Restoring"): the Mii's whole online self (its WFC login id, the game's own
-- friend roster, its records), which only the launcher of the player the
-- profile is bound to sends, and which brings it back on a fresh install.
ALTER TABLE profiles
    ADD COLUMN slot_data      BYTEA CHECK (octet_length(slot_data) = 2832),
    ADD COLUMN slot_stored_at TIMESTAMPTZ;
