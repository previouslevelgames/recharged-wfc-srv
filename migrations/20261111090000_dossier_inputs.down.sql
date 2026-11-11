-- Version 2's dossiers do not fit the old bound.
DELETE FROM desync_dossiers WHERE octet_length(body) > 28856;
ALTER TABLE desync_dossiers
    DROP CONSTRAINT desync_dossiers_body_check,
    ADD CONSTRAINT desync_dossiers_body_check CHECK (octet_length(body) <= 28856);
