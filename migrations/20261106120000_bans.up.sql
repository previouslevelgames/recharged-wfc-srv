-- An admin's ban, given when they delete a player (docs/profiles.md,
-- "Deleting a player") and lifted on a profile's page.
CREATE TABLE bans (
    ban_id    BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- Whom it was for, as the admin page names them: the Recharged player's
    -- name, else the profile's Mii name.
    name      TEXT        NOT NULL,
    -- The admin's email.
    banned_by TEXT        NOT NULL,
    banned_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Consoles (DWC userids) a ban covers: NAS and GPCM refuse the logins of
-- every Mii on them.
CREATE TABLE banned_accounts (
    user_id BIGINT PRIMARY KEY REFERENCES accounts ON DELETE CASCADE,
    ban_id  BIGINT NOT NULL REFERENCES bans ON DELETE CASCADE
);
CREATE INDEX ON banned_accounts (ban_id);

-- Sign-ins (Discord's) of the deleted player: they make no new player.
CREATE TABLE banned_identities (
    provider    TEXT   NOT NULL,
    external_id TEXT   NOT NULL,
    ban_id      BIGINT NOT NULL REFERENCES bans ON DELETE CASCADE,
    PRIMARY KEY (provider, external_id)
);
CREATE INDEX ON banned_identities (ban_id);

-- A ban ends a game's GPCM login during a match too.
ALTER TABLE online_match_sides
    DROP CONSTRAINT online_match_sides_left_how_check,
    ADD CONSTRAINT online_match_sides_left_how_check
        CHECK (left_how IN ('logout', 'closed', 'lost', 'replaced', 'banned'));
