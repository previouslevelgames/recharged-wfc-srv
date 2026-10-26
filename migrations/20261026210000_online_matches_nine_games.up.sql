-- The game's Series Length goes up to 9 games.
ALTER TABLE online_matches
    DROP CONSTRAINT online_matches_best_of_check,
    ADD CONSTRAINT online_matches_best_of_check CHECK (best_of IN (1, 3, 5, 7, 9));
ALTER TABLE online_match_games
    DROP CONSTRAINT online_match_games_number_check,
    ADD CONSTRAINT online_match_games_number_check CHECK (number BETWEEN 1 AND 9);
