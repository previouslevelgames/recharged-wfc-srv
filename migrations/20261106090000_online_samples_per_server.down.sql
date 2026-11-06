ALTER TABLE online_samples DROP CONSTRAINT online_samples_pkey;
WITH samples AS (DELETE FROM online_samples RETURNING *)
INSERT INTO online_samples (at, server, wfc, playing, recharged)
SELECT at, '', sum(wfc)::integer, sum(playing)::integer, max(recharged)
FROM samples GROUP BY at;
ALTER TABLE online_samples DROP COLUMN server;
ALTER TABLE online_samples ADD PRIMARY KEY (at);
