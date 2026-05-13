-- Append-only event log. Source of truth for the sync API's sync_token cursor
-- (`id` is monotonic). `uuid` is the per-command idempotency key from sync callers.
CREATE TABLE IF NOT EXISTS pt_event_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid       TEXT NOT NULL UNIQUE,           -- caller-supplied idempotency key
    task_uuid  TEXT,                            -- FK soft (may reference soft-deleted rows)
    event_type TEXT NOT NULL,                   -- 'task.created', 'task.status.changed', etc.
    payload    TEXT NOT NULL,                   -- JSON
    ts         TEXT NOT NULL                    -- ISO datetime
);

CREATE INDEX IF NOT EXISTS idx_pt_event_log_task ON pt_event_log(task_uuid, ts);
