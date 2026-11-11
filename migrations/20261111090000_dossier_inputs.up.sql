-- Desync dossiers of version 2 (Recharged 1.1.7) add the game as the side's
-- rollback played it and the inputs of 32 steps (docs/reports.md, "Desync
-- dossiers"): up to 57 232 bytes.
ALTER TABLE desync_dossiers
    DROP CONSTRAINT desync_dossiers_body_check,
    ADD CONSTRAINT desync_dossiers_body_check CHECK (octet_length(body) <= 57232);
