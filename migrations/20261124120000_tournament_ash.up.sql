-- Single elimination with an ash bracket: the losers of the first round play
-- a single elimination of their own, whose sets are on the side 'ash'.
ALTER TABLE tournaments DROP CONSTRAINT tournaments_format_check;
ALTER TABLE tournaments ADD CONSTRAINT tournaments_format_check
    CHECK (format IN ('single', 'double', 'single_ash'));
ALTER TABLE tournament_sets DROP CONSTRAINT tournament_sets_side_check;
ALTER TABLE tournament_sets ADD CONSTRAINT tournament_sets_side_check
    CHECK (side IN ('winners', 'losers', 'grand_final', 'grand_final_reset', 'ash'));
