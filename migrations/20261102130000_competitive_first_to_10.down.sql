UPDATE rule_sets
SET rules = '{"tweaks": ["no_mega_strikes", "fast_terrain", "thunder_island_walls"], "skill": "rookie", "limit": "time", "time": 180, "goals": 5, "games": 3, "stadium": "random"}',
    updated_at = now()
WHERE name = 'Competitive';
