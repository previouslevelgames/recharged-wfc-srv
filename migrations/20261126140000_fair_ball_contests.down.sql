UPDATE rule_sets
SET rules = jsonb_set(rules::jsonb, '{tweaks}', (rules::jsonb -> 'tweaks') - 'fair_ball_contests')::text,
    updated_at = now();

UPDATE tournaments
SET rules = jsonb_set(rules::jsonb, '{tweaks}', (rules::jsonb -> 'tweaks') - 'fair_ball_contests')::text
WHERE state IN ('registration', 'running');
