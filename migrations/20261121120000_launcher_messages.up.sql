-- The message the launcher shows on its Play page (docs/profiles.md,
-- "Launcher message"), as owners post it on the admin dashboard's
-- Maintenance page. Every post is a new row with a new id; only the newest
-- one shows, until it ends or is removed, and the older ones stay as
-- history.
CREATE TABLE launcher_messages (
    id         BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- As the owner wrote it; the launcher renders simple Markdown.
    markdown   TEXT        NOT NULL,
    tone       TEXT        NOT NULL CHECK (tone IN ('info', 'warning')),
    -- NULL: until it is removed.
    ends_at    TIMESTAMPTZ,
    -- The owners' Google addresses.
    posted_by  TEXT        NOT NULL,
    posted_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    removed_at TIMESTAMPTZ,
    removed_by TEXT
);
