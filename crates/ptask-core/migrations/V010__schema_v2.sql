-- V010: schema v2 — the v2.0.0 consolidation.
--
-- 1. pt_extensions merges INTO tasks (the side-table was a v0.x migration
--    tactic that became permanent architecture). The physical table is
--    renamed to pt_extensions_legacy and replaced by a compat VIEW so the
--    dashboard sidecar's `LEFT JOIN pt_extensions e ... e.pt_id` keeps
--    working until Phase 7 absorbs it.
-- 2. tasks.status_v2 — the real 8-state model. The legacy tasks.status
--    column REMAINS the compat surface (kept in sync by every Rust write
--    path) for older readers that still inspect tasks.status directly.
--    status: pending|done|delayed|dismissed|
--    blocked ⇄ status_v2: triage|backlog|todo|in_progress|snoozed|done|
--    dismissed|blocked.
-- 3. task_links replaces the depends_on/blocks_tasks JSON blobs (0
--    non-empty in prod); task_labels replaces pt_extensions.labels JSON.
-- 4. due_at (scheduled date) joins deadline (hard date); snoozed_until
--    powers pt snooze; parent_uuid anchors promoted subtasks.
-- 5. interactions history is copied one-way into pt_event_log (actor NULL,
--    uuid 'legacy-int:<id>') so the journal is the one complete record;
--    live writers keep writing interactions until Phase 6 retires the
--    neglect-score read.
-- 6. Timestamps normalize to UTC RFC3339 (millisecond precision).
-- 7. tasks_fts (FTS5, external content) + sync triggers for pt search.
--
-- Subtask promotion (JSON blobs → child rows) happens in the Rust
-- converter (`pt backfill`), not here — it needs PT-N minting and
-- attributed events. Only NON-terminal parents are promoted; terminal
-- parents keep their JSON as history.

-- ---- 1+2+4: tasks gains the merged + v2 columns --------------------------
ALTER TABLE tasks ADD COLUMN status_v2 TEXT NOT NULL DEFAULT 'todo'
    CHECK (status_v2 IN ('triage','backlog','todo','in_progress','snoozed','done','dismissed','blocked'));
ALTER TABLE tasks ADD COLUMN pt_id TEXT;
ALTER TABLE tasks ADD COLUMN status_label TEXT;
ALTER TABLE tasks ADD COLUMN energy TEXT;
ALTER TABLE tasks ADD COLUMN duration_min INTEGER;
ALTER TABLE tasks ADD COLUMN planned_at TEXT;
ALTER TABLE tasks ADD COLUMN actual_min INTEGER;
ALTER TABLE tasks ADD COLUMN project TEXT;
ALTER TABLE tasks ADD COLUMN created_by_pt INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN due_at TEXT;
ALTER TABLE tasks ADD COLUMN snoozed_until TEXT;
ALTER TABLE tasks ADD COLUMN parent_uuid TEXT;

-- Backfill merged columns from the side table.
UPDATE tasks SET
    pt_id         = (SELECT e.pt_id         FROM pt_extensions e WHERE e.task_uuid = tasks.id),
    status_label  = (SELECT e.status_label  FROM pt_extensions e WHERE e.task_uuid = tasks.id),
    energy        = (SELECT e.energy        FROM pt_extensions e WHERE e.task_uuid = tasks.id),
    duration_min  = (SELECT e.duration_min  FROM pt_extensions e WHERE e.task_uuid = tasks.id),
    planned_at    = (SELECT e.planned_at    FROM pt_extensions e WHERE e.task_uuid = tasks.id),
    actual_min    = (SELECT e.actual_min    FROM pt_extensions e WHERE e.task_uuid = tasks.id),
    project       = (SELECT e.project       FROM pt_extensions e WHERE e.task_uuid = tasks.id),
    created_by_pt = COALESCE((SELECT e.created_by_pt FROM pt_extensions e WHERE e.task_uuid = tasks.id), 0)
WHERE EXISTS (SELECT 1 FROM pt_extensions e WHERE e.task_uuid = tasks.id);

-- status_v2 from the legacy vocabulary.
UPDATE tasks SET status_v2 = CASE status
    WHEN 'pending'   THEN 'todo'
    WHEN 'delayed'   THEN 'snoozed'
    WHEN 'done'      THEN 'done'
    WHEN 'dismissed' THEN 'dismissed'
    WHEN 'blocked'   THEN 'blocked'
    ELSE 'todo'
END;

CREATE UNIQUE INDEX IF NOT EXISTS idx_tasks_pt_id ON tasks(pt_id) WHERE pt_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_tasks_status_v2 ON tasks(status_v2);
CREATE INDEX IF NOT EXISTS idx_tasks_parent ON tasks(parent_uuid) WHERE parent_uuid IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project) WHERE project IS NOT NULL;

-- ---- 3: relations as rows ------------------------------------------------
CREATE TABLE IF NOT EXISTS task_links (
    from_uuid  TEXT NOT NULL,
    to_uuid    TEXT NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('depends_on','blocks','discovered_from','subtask_of')),
    created_at TEXT NOT NULL,
    PRIMARY KEY (from_uuid, to_uuid, kind)
);
CREATE INDEX IF NOT EXISTS idx_task_links_to ON task_links(to_uuid, kind);

INSERT OR IGNORE INTO task_links (from_uuid, to_uuid, kind, created_at)
SELECT t.id, j.value, 'depends_on', strftime('%Y-%m-%dT%H:%M:%f', 'now') || '+00:00'
FROM tasks t, json_each(COALESCE(t.depends_on, '[]')) j
WHERE json_valid(COALESCE(t.depends_on, '[]'));

INSERT OR IGNORE INTO task_links (from_uuid, to_uuid, kind, created_at)
SELECT t.id, j.value, 'blocks', strftime('%Y-%m-%dT%H:%M:%f', 'now') || '+00:00'
FROM tasks t, json_each(COALESCE(t.blocks_tasks, '[]')) j
WHERE json_valid(COALESCE(t.blocks_tasks, '[]'));

CREATE TABLE IF NOT EXISTS task_labels (
    task_uuid TEXT NOT NULL,
    label     TEXT NOT NULL,
    PRIMARY KEY (task_uuid, label)
);

INSERT OR IGNORE INTO task_labels (task_uuid, label)
SELECT e.task_uuid, j.value
FROM pt_extensions e, json_each(COALESCE(e.labels, '[]')) j
WHERE json_valid(COALESCE(e.labels, '[]'));

-- ---- 1: side table → compat view ----------------------------------------
ALTER TABLE pt_extensions RENAME TO pt_extensions_legacy;

CREATE VIEW pt_extensions AS
SELECT
    t.id            AS task_uuid,
    t.pt_id         AS pt_id,
    CASE t.status_v2
        WHEN 'in_progress' THEN 'in_progress'
        WHEN 'snoozed'     THEN 'snoozed'
        WHEN 'triage'      THEN 'triage'
        WHEN 'backlog'     THEN 'backlog'
        WHEN 'done'        THEN 'done'
        WHEN 'dismissed'   THEN 'dismissed'
        WHEN 'blocked'     THEN 'blocked'
        ELSE 'todo'
    END             AS status_category,
    t.status_label  AS status_label,
    t.energy        AS energy,
    t.duration_min  AS duration_min,
    t.planned_at    AS planned_at,
    t.actual_min    AS actual_min,
    COALESCE((SELECT json_group_array(l.label) FROM task_labels l
              WHERE l.task_uuid = t.id), '[]') AS labels,
    t.created_by_pt AS created_by_pt,
    t.project       AS project
FROM tasks t
WHERE t.pt_id IS NOT NULL;

-- ---- 5: fold interactions history into the journal -----------------------
INSERT OR IGNORE INTO pt_event_log (uuid, task_uuid, event_type, payload, ts, actor)
SELECT
    'legacy-int:' || i.id,
    i.task_id,
    'interaction.' || i.action,
    json_object('details', i.details, 'legacy', 1),
    CASE WHEN i.ts IS NOT NULL AND i.ts != ''
         THEN strftime('%Y-%m-%dT%H:%M:%f', i.ts) || '+00:00'
         ELSE i.ts END,
    NULL
FROM interactions i
WHERE EXISTS (SELECT 1 FROM tasks t WHERE t.id = i.task_id);

-- ---- 6: UTC-normalize task + event timestamps -----------------------------
UPDATE tasks SET created_at = strftime('%Y-%m-%dT%H:%M:%f', created_at) || '+00:00'
WHERE created_at IS NOT NULL AND created_at != ''
  AND strftime('%Y-%m-%dT%H:%M:%f', created_at) IS NOT NULL;
UPDATE tasks SET updated_at = strftime('%Y-%m-%dT%H:%M:%f', updated_at) || '+00:00'
WHERE updated_at IS NOT NULL AND updated_at != ''
  AND strftime('%Y-%m-%dT%H:%M:%f', updated_at) IS NOT NULL;
UPDATE tasks SET deadline = strftime('%Y-%m-%dT%H:%M:%f', deadline) || '+00:00'
WHERE deadline IS NOT NULL AND deadline != ''
  AND strftime('%Y-%m-%dT%H:%M:%f', deadline) IS NOT NULL;
UPDATE tasks SET last_reminded = strftime('%Y-%m-%dT%H:%M:%f', last_reminded) || '+00:00'
WHERE last_reminded IS NOT NULL AND last_reminded != ''
  AND strftime('%Y-%m-%dT%H:%M:%f', last_reminded) IS NOT NULL;
UPDATE tasks SET next_reminder = strftime('%Y-%m-%dT%H:%M:%f', next_reminder) || '+00:00'
WHERE next_reminder IS NOT NULL AND next_reminder != ''
  AND strftime('%Y-%m-%dT%H:%M:%f', next_reminder) IS NOT NULL;
UPDATE pt_event_log SET ts = strftime('%Y-%m-%dT%H:%M:%f', ts) || '+00:00'
WHERE ts IS NOT NULL AND ts != ''
  AND strftime('%Y-%m-%dT%H:%M:%f', ts) IS NOT NULL;

-- ---- 7: FTS5 search --------------------------------------------------------
CREATE VIRTUAL TABLE IF NOT EXISTS tasks_fts USING fts5(
    title, description,
    content='tasks', content_rowid='rowid'
);

INSERT INTO tasks_fts(rowid, title, description)
SELECT rowid, title, description FROM tasks;

CREATE TRIGGER IF NOT EXISTS tasks_fts_ai AFTER INSERT ON tasks BEGIN
    INSERT INTO tasks_fts(rowid, title, description)
    VALUES (new.rowid, new.title, new.description);
END;
CREATE TRIGGER IF NOT EXISTS tasks_fts_ad AFTER DELETE ON tasks BEGIN
    INSERT INTO tasks_fts(tasks_fts, rowid, title, description)
    VALUES ('delete', old.rowid, old.title, old.description);
END;
CREATE TRIGGER IF NOT EXISTS tasks_fts_au AFTER UPDATE OF title, description ON tasks BEGIN
    INSERT INTO tasks_fts(tasks_fts, rowid, title, description)
    VALUES ('delete', old.rowid, old.title, old.description);
    INSERT INTO tasks_fts(rowid, title, description)
    VALUES (new.rowid, new.title, new.description);
END;
