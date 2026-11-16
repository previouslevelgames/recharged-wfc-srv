DROP TABLE score_puts;
ALTER TABLE scores
    DROP COLUMN held_score,
    DROP COLUMN held_wins,
    DROP COLUMN held_losses,
    DROP COLUMN held_on,
    DROP COLUMN provisional;
