-- Settings an owner switches on the admin dashboard's Maintenance page
-- (docs/install.md, "Admin dashboard"): the last switch wins over the
-- environment's starting value.
CREATE TABLE switches (
    name       TEXT        PRIMARY KEY,
    enabled    BOOLEAN     NOT NULL,
    -- The owner's Google address.
    changed_by TEXT        NOT NULL,
    changed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
