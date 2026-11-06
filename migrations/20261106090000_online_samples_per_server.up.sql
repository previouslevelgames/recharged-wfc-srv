-- Each server samples the players logged in to it (docs/install.md, "Several
-- servers on one database"): one row per minute and server, by its host name,
-- which the Monitoring page adds up. The rows from before have server ''.
ALTER TABLE online_samples ADD COLUMN server TEXT NOT NULL DEFAULT '';
ALTER TABLE online_samples ALTER COLUMN server DROP DEFAULT;
ALTER TABLE online_samples DROP CONSTRAINT online_samples_pkey;
ALTER TABLE online_samples ADD PRIMARY KEY (at, server);
