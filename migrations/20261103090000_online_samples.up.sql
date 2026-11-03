-- How many players were online, once a minute (docs/install.md, "Admin
-- dashboard"), for the Monitoring page's chart: logged in to WFC (GPCM; the
-- CPU buddy left out), of them in an online match (DWC status 2), and with
-- Recharged open (a launcher or game asked in the last minute). Kept 30 days.
CREATE TABLE online_samples (
    at        TIMESTAMPTZ PRIMARY KEY,
    wfc       INTEGER     NOT NULL CHECK (wfc >= 0),
    playing   INTEGER     NOT NULL CHECK (playing >= 0),
    recharged INTEGER     NOT NULL CHECK (recharged >= 0)
);
