UPDATE online_match_sides SET left_how = 'closed' WHERE left_how = 'banned';
ALTER TABLE online_match_sides
    DROP CONSTRAINT online_match_sides_left_how_check,
    ADD CONSTRAINT online_match_sides_left_how_check
        CHECK (left_how IN ('logout', 'closed', 'lost', 'replaced'));
DROP TABLE banned_identities;
DROP TABLE banned_accounts;
DROP TABLE bans;
