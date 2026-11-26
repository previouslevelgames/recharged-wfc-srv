-- Fair ball contests became a rule (docs/profiles.md, "Rules"): online games
-- always played with them, so the rule sets, and the tournaments not over,
-- keep them. Matches keep the rules they were made with.
UPDATE rule_sets
SET rules = jsonb_set(rules::jsonb, '{tweaks}', (rules::jsonb -> 'tweaks') || '["fair_ball_contests"]')::text,
    updated_at = now()
WHERE NOT (rules::jsonb -> 'tweaks') ? 'fair_ball_contests';

UPDATE tournaments
SET rules = jsonb_set(rules::jsonb, '{tweaks}', (rules::jsonb -> 'tweaks') || '["fair_ball_contests"]')::text
WHERE state IN ('registration', 'running')
  AND NOT (rules::jsonb -> 'tweaks') ? 'fair_ball_contests';
