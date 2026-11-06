-- The nodes of the pool (docs/nodes.md): each running server's lease, a new
-- row each time it starts, renewed every few seconds with what it carries.
CREATE TABLE nodes (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name TEXT NOT NULL,
    revision TEXT NOT NULL,
    version TEXT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    players INTEGER NOT NULL DEFAULT 0,
    games INTEGER NOT NULL DEFAULT 0
);

-- Keys every node uses alike, made by the first that needs one: NAS signs its
-- login tokens with one, so a token from any node logs in to any node.
CREATE TABLE shared_keys (
    name TEXT PRIMARY KEY,
    key BYTEA NOT NULL CHECK (length(key) = 32),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
