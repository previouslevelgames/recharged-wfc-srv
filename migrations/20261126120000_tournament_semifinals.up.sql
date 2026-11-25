-- The semifinals' sets may be played to their own number of games, as the
-- finals' are; a tournament made before plays them as its other sets.
ALTER TABLE tournaments ADD COLUMN semifinals_best_of SMALLINT;
UPDATE tournaments SET semifinals_best_of = best_of;
ALTER TABLE tournaments ALTER COLUMN semifinals_best_of SET NOT NULL;
ALTER TABLE tournaments ADD CONSTRAINT tournaments_semifinals_best_of_check
    CHECK (semifinals_best_of IN (1, 3, 5, 7));
