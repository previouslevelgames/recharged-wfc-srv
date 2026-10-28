-- What Recharged builds tell of how their online matches went wrong
-- (docs/reports.md, "Diagnostics"), and what the server sees itself: a game
-- that left WFC in the middle of a match, and the reports it refused.
ALTER TABLE online_match_sides
    -- The build that reported the side, e.g. `1.4.2 (abc1234)`.
    ADD COLUMN build    TEXT,
    -- Why its series ended, as its end said: `reason` tells only whether it
    -- was decided.
    ADD COLUMN cause    TEXT CHECK (cause IN ('decided', 'desync', 'disconnect', 'offline')),
    -- When its game's GPCM login ended before the side's end came (within
    -- 30 minutes of its last report), and how:
    -- `logout` (the game left online play), `closed` (the connection was
    -- closed without a logout: the game was closed or crashed), `lost` (it
    -- broke off) or `replaced` (another login of the profile took over).
    ADD COLUMN left_at  TIMESTAMPTZ,
    ADD COLUMN left_how TEXT CHECK (left_how IN ('logout', 'closed', 'lost', 'replaced'));

ALTER TABLE online_match_games
    -- For a `desync`, the first of the game's checks that failed (`frame`,
    -- `checksum` or `seed`) and on which frame.
    ADD COLUMN sync_check TEXT CHECK (sync_check IN ('frame', 'checksum', 'seed')),
    ADD COLUMN sync_frame INTEGER CHECK (sync_frame >= 0);

-- Reports the server refused, by profile: one row per profile and kind,
-- `elsewhere` (not from the address the profile is logged in from) or
-- `invalid` (a field missing or out of range). Only for profiles that exist.
CREATE TABLE report_refusals (
    profile_id   INTEGER     NOT NULL REFERENCES profiles ON DELETE CASCADE,
    kind         TEXT        NOT NULL CHECK (kind IN ('elsewhere', 'invalid')),
    count        BIGINT      NOT NULL DEFAULT 1,
    -- The last one's problem, and the address it came from.
    problem      TEXT        NOT NULL,
    address      TEXT        NOT NULL,
    first_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (profile_id, kind)
);

-- Error codes the games showed their players (docs/reports.md, "Errors"),
-- kept 30 days. Anyone may send one: `profile_id` is set only when the
-- profile it named was logged in to GPCM from `address`.
CREATE TABLE client_errors (
    error_id    BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The popup's number, e.g. 86420: DWC's codes tell the sequence (login,
    -- friends, matchmaking), the GameSpy part and the kind of failure.
    code        INTEGER     NOT NULL CHECK (code BETWEEN 1 AND 999999),
    -- The game's session state when it happened.
    phase       TEXT        CHECK (phase IN ('none', 'login', 'matchmaking', 'prestart',
                                             'loading', 'in_game', 'game_end')),
    release     TEXT        NOT NULL CHECK (release IN ('R4QE', 'R4QP', 'R4QJ', 'R4QK')),
    build       TEXT,
    profile_id  INTEGER     REFERENCES profiles ON DELETE SET NULL,
    address     TEXT        NOT NULL
);
CREATE INDEX client_errors_by_time ON client_errors (received_at);
