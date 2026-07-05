-- V012: signal-intelligence support (v2.5.0).
--
-- capture_key: deterministic client key (e.g. puresentinel signature_for =
-- probe:resource:kind) persisted on fast-lane incident tasks so (a) exact
-- re-captures refresh the existing task instead of minting a duplicate and
-- (b) /capture/resolve can close the task when the source reports recovery.
ALTER TABLE tasks ADD COLUMN capture_key TEXT;
ALTER TABLE tasks ADD COLUMN capture_count INTEGER NOT NULL DEFAULT 1;
ALTER TABLE tasks ADD COLUMN last_captured_at TEXT;
CREATE INDEX idx_tasks_capture_key ON tasks(capture_key) WHERE capture_key IS NOT NULL;

-- task_links gains a 'duplicate_of' kind so dedup merges are recorded
-- non-destructively. SQLite CHECK constraints are immutable — rebuild.
CREATE TABLE task_links_new (
    from_uuid  TEXT NOT NULL,
    to_uuid    TEXT NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('depends_on','blocks','discovered_from','subtask_of','duplicate_of')),
    created_at TEXT NOT NULL,
    PRIMARY KEY (from_uuid, to_uuid, kind)
);
INSERT INTO task_links_new SELECT from_uuid, to_uuid, kind, created_at FROM task_links;
DROP TABLE task_links;
ALTER TABLE task_links_new RENAME TO task_links;
CREATE INDEX idx_task_links_to ON task_links(to_uuid, kind);
