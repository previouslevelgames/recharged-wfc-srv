-- The GPCM logins of each node (docs/nodes.md), for the others' HTTP: the
-- profile service, gamestats2, the reports and /admin ask after logins on
-- any node. A node keeps its own rows like its logins, and they go with its
-- lease.
CREATE TABLE logins (
    node            BIGINT      NOT NULL REFERENCES nodes ON DELETE CASCADE,
    profile_id      INTEGER     NOT NULL,
    -- The node's connection: a new login of the profile on the node has
    -- another.
    connection      BIGINT      NOT NULL,
    -- Where the GPCM connection came from, and the NAS login whose token it
    -- presented.
    address         TEXT        NOT NULL,
    nas_address     TEXT        NOT NULL,
    -- SHA-256 of the login ticket (`lt`), which the port shows to prove the
    -- login is its own.
    ticket          BYTEA       NOT NULL CHECK (length(ticket) = 32),
    logged_in_at    TIMESTAMPTZ NOT NULL,
    -- DWC's status from the last `\status\`, and since when.
    status          SMALLINT,
    status_since    TIMESTAMPTZ,
    matchmaking     TEXT        CHECK (matchmaking IN ('anybody', 'friends')),
    room_players    SMALLINT,
    friend_requests INTEGER     NOT NULL DEFAULT 0,
    mutual_friends  INTEGER     NOT NULL DEFAULT 0,
    -- An admin deleted the profile's scores during this login.
    off_boards      BOOLEAN     NOT NULL DEFAULT false,
    PRIMARY KEY (node, profile_id)
);
CREATE INDEX ON logins (profile_id);
