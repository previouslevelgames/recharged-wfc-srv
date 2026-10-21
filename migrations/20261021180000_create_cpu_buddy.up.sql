-- The CPU buddy (docs/login.md): a profile kept for a test player, so that a
-- game has a friend code to add before a second console is online. A headless
-- client of the port is to log in as it later. Its ids are below the ones NAS
-- and GPCM give out.
INSERT INTO accounts (user_id) VALUES (1);
INSERT INTO profiles (profile_id, user_id, gsbrcd, unique_nick, ingamesn)
VALUES (999999999, 1, 'R4QEcpu0001', '1R4QEcpu0001', 'CPU');
