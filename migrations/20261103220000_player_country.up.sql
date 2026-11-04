-- A player's country (docs/profiles.md, "Countries"): the one their launcher
-- or game last connected from, and their choice: `auto` shows it, `none`
-- shows no flag, or two letters of their own.
ALTER TABLE players
    ADD COLUMN country TEXT CHECK (country ~ '^[A-Z]{2}$'),
    ADD COLUMN country_choice TEXT NOT NULL DEFAULT 'auto'
        CHECK (country_choice IN ('auto', 'none') OR country_choice ~ '^[A-Z]{2}$');
