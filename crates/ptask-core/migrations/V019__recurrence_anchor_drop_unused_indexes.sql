-- V019: monthly recurrences keep their anchor; unused indexes go.
--
-- 1. pt_recurrence.anchor is the occurrence the operator set: the deadline
--    at creation or an explicit deadline edit. A fixed "every month" rule
--    counts whole intervals from it, so Jan 31 -> Feb 28 -> Mar 31. Chaining
--    from the clamped Feb 28 instead settled on the 28th for good. NULL
--    (rows from before V019) keeps the chain-from-deadline rule.
--
-- 2. No query reads these indexes; each only taxes its table's writes.
--    - idx_pt_recurrence_next: rows are read by task_uuid, the primary key.
--    - idx_pt_webhook_log_source: the log is read by id and counted by
--      direction.
--    - idx_tasks_kind: the kind filter compares COALESCE(t.kind, 'ship'),
--      which a partial index on kind = 'scout' cannot serve.
--    - idx_goals_status_seq: goals are listed by seq and filtered in Rust.

ALTER TABLE pt_recurrence ADD COLUMN anchor TEXT;

DROP INDEX IF EXISTS idx_pt_recurrence_next;
DROP INDEX IF EXISTS idx_pt_webhook_log_source;
DROP INDEX IF EXISTS idx_tasks_kind;
DROP INDEX IF EXISTS idx_goals_status_seq;
