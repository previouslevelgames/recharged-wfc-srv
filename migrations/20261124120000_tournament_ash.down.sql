DELETE FROM tournaments WHERE format = 'single_ash';
ALTER TABLE tournament_sets DROP CONSTRAINT tournament_sets_side_check;
ALTER TABLE tournament_sets ADD CONSTRAINT tournament_sets_side_check
    CHECK (side IN ('winners', 'losers', 'grand_final', 'grand_final_reset'));
ALTER TABLE tournaments DROP CONSTRAINT tournaments_format_check;
ALTER TABLE tournaments ADD CONSTRAINT tournaments_format_check
    CHECK (format IN ('single', 'double'));
