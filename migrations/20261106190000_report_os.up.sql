-- The operating system each side's game ran on, as its reports name it
-- (docs/reports.md, "Diagnostics"), e.g. `Windows 11 (26100), x86_64`.
ALTER TABLE online_match_sides ADD COLUMN os TEXT;
