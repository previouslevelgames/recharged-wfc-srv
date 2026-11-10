DROP INDEX online_matches_forfeited;
ALTER TABLE online_matches
    DROP COLUMN forfeited_by,
    DROP COLUMN forfeited_at;
ALTER TABLE online_match_sides DROP COLUMN forfeited_at;
ALTER TABLE matches
    DROP COLUMN forfeited_by,
    DROP COLUMN forfeited_at;
