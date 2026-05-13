-- Side table extending the existing Python `tasks` table with pTask-native fields.
-- Existing UUID primary key from `tasks` is the FK; PT-N is the user-facing handle.
CREATE TABLE IF NOT EXISTS pt_extensions (
    task_uuid       TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    pt_id           TEXT UNIQUE NOT NULL,                  -- 'PT-1', 'PT-2', ...
    status_category TEXT NOT NULL DEFAULT 'todo',          -- triage|backlog|todo|in_progress|done|cancelled
    status_label    TEXT,                                  -- free-form per-team status name
    energy          TEXT,                                  -- deep|admin|phone|null
    duration_min    INTEGER,                               -- estimated minutes
    planned_at      TEXT,                                  -- ISO datetime, when operator plans to do it
    actual_min      INTEGER,                               -- actual time spent
    labels          TEXT NOT NULL DEFAULT '[]',            -- JSON array of strings
    created_by_pt   INTEGER NOT NULL DEFAULT 0             -- 1 if Rust created the parent row
);

CREATE INDEX IF NOT EXISTS idx_pt_extensions_pt_id ON pt_extensions(pt_id);
CREATE INDEX IF NOT EXISTS idx_pt_extensions_status_category ON pt_extensions(status_category);
