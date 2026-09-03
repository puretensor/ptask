-- V015: scout-vs-ship task kind.
--
-- A task is either an INVESTIGATION (kind='scout', deliverable='report') or
-- an IMPLEMENTATION (kind='ship', deliverable='pr'). Promoting a finished
-- investigation into implementation work FLIPS `kind` on the SAME row —
-- it must never close the scout row and open a duplicate ship row, because
-- that inflates the open count and re-tickets work instead of disposing of
-- it (the 2026-08-19 "closing a task must not manufacture more open work"
-- rule, enforced here at schema level).
--
-- Existing rows are `ship` with an unset deliverable: pre-V015 pTask had no
-- notion of an investigation, and stamping a deliverable on 2k historical
-- rows would be an invention, not a backfill.

ALTER TABLE tasks ADD COLUMN kind TEXT NOT NULL DEFAULT 'ship'
    CHECK (kind IN ('scout','ship'));
ALTER TABLE tasks ADD COLUMN deliverable TEXT
    CHECK (deliverable IS NULL OR deliverable IN ('report','pr','none'));

CREATE INDEX IF NOT EXISTS idx_tasks_kind ON tasks(kind) WHERE kind = 'scout';
