-- A tournament's schedule: entries close at `registration_ends_at`, and
-- its sets are played from `starts_at` (until then only meant to show).
-- Either may be left out; entries close before the games begin.
ALTER TABLE tournaments ADD COLUMN registration_ends_at TIMESTAMPTZ;
ALTER TABLE tournaments ADD CONSTRAINT tournaments_schedule_check
    CHECK (registration_ends_at IS NULL OR starts_at IS NULL OR registration_ends_at <= starts_at);
