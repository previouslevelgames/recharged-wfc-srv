-- Version 2's dossiers also list the steps whose checksums the side sent
-- (docs/reports.md, "Desync dossiers"): up to 16 steps, 258 104 bytes.
ALTER TABLE desync_dossiers
    DROP CONSTRAINT desync_dossiers_body_check,
    ADD CONSTRAINT desync_dossiers_body_check CHECK (octet_length(body) <= 258104);
