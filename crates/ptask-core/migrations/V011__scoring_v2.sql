-- V011: scoring v2 + accountability v2 support columns (Phase 6).
--
-- score_llm: bounded LLM triage adjustment (±0.15), a SEPARATE signal added
-- after the deterministic composite — never an overwrite. Written by the
-- triage pass (Phase 8 wiring); read by scoring v2 now.
ALTER TABLE tasks ADD COLUMN score_llm REAL NOT NULL DEFAULT 0.0;
ALTER TABLE tasks ADD COLUMN triage_reason TEXT;
ALTER TABLE tasks ADD COLUMN triage_at TEXT;

-- level_changed_at: when escalation_level last advanced. Accountability v2
-- replaces the dismissal_count gates (which had NO writer — levels 2-3 were
-- unreachable for a year) with time-at-level transitions.
ALTER TABLE tasks ADD COLUMN level_changed_at TEXT;

-- Deferred hygiene from the master plan: the idea graveyard. Old, low-
-- priority, never-touched items are ideas, not operational work — mark them
-- so accountability stops nagging about them and reviews can filter.
UPDATE tasks SET task_type = 'idea'
WHERE priority = 1
  AND status_v2 IN ('triage','backlog','todo')
  AND task_type = 'operational'
  AND updated_at < strftime('%Y-%m-%dT%H:%M:%f', 'now', '-60 days') || '+00:00';
