-- Competitive is first to 10 goals (docs/profiles.md, "Rules"), as the
-- launcher's built-in Competitive preset from 1.2.0 on. Matches keep the
-- rules they were made with.
UPDATE rule_sets
SET rules = '{"tweaks": ["no_mega_strikes", "fast_terrain", "thunder_island_walls"], "skill": "rookie", "limit": "goals", "time": 180, "goals": 10, "games": 3, "stadium": "random"}',
    updated_at = now()
WHERE name = 'Competitive';
