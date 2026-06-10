-- Legacy base schema bootstrap.
--
-- Until now `pt` assumed the Python `init_db` had already created the legacy
-- tables (tasks, interactions, ...): a fresh `pt --db new.db add ...` failed
-- with "no such table: tasks". Every statement is IF NOT EXISTS, so this is
-- a no-op on the live fleet DB and a full bootstrap on a greenfield install.
--
-- DDL is byte-equivalent (modulo whitespace) to the live tensor-core
-- tasks.db schema as of 2026-06-10, including the columns Python added via
-- later ALTERs (raw_items.classification*, tasks.subtasks/task_type/
-- cluster_keywords) inlined at their live positions.

CREATE TABLE IF NOT EXISTS tasks (
    id               TEXT PRIMARY KEY,
    title            TEXT NOT NULL,
    description      TEXT DEFAULT '',
    priority         INTEGER DEFAULT 2,
    status           TEXT DEFAULT 'pending',
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    deadline         TEXT,

    source_type      TEXT DEFAULT 'manual',
    source_files     TEXT DEFAULT '[]',
    ai_confidence    REAL DEFAULT 1.0,
    ai_reasoning     TEXT DEFAULT '',

    depends_on       TEXT DEFAULT '[]',
    blocks_tasks     TEXT DEFAULT '[]',

    escalation_level INTEGER DEFAULT 0,
    dismissal_count  INTEGER DEFAULT 0,
    last_reminded    TEXT,
    next_reminder    TEXT,

    priority_score   REAL DEFAULT 0.0,
    score_urgency    REAL DEFAULT 0.0,
    score_dependency REAL DEFAULT 0.0,
    score_neglect    REAL DEFAULT 0.0,
    subtasks         TEXT DEFAULT '[]',
    task_type        TEXT DEFAULT 'operational',
    cluster_keywords TEXT DEFAULT '[]'
);

CREATE TABLE IF NOT EXISTS interactions (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id  TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    action   TEXT NOT NULL,
    ts       TEXT NOT NULL,
    details  TEXT DEFAULT ''
);

CREATE TABLE IF NOT EXISTS notifications (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id          TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    channel          TEXT NOT NULL,
    sent_at          TEXT NOT NULL,
    escalation_level INTEGER,
    message_text     TEXT,
    dismissed        INTEGER DEFAULT 0,
    dismissed_at     TEXT
);

CREATE TABLE IF NOT EXISTS raw_items (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    text             TEXT NOT NULL,
    source_type      TEXT NOT NULL,
    source_file      TEXT NOT NULL,
    source_date      TEXT NOT NULL,
    commitment_score REAL DEFAULT 0.0,
    processed        INTEGER DEFAULT 0,
    created_at       TEXT NOT NULL,
    classification   TEXT DEFAULT NULL,
    classification_confidence REAL DEFAULT 0.0,
    classification_reasoning  TEXT DEFAULT ''
);

CREATE TABLE IF NOT EXISTS canonical_tasks (
    id               TEXT PRIMARY KEY,
    task_id          TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    cluster_id       INTEGER,
    cluster_keywords TEXT DEFAULT '[]',
    raw_item_ids     TEXT DEFAULT '[]',
    source_files     TEXT DEFAULT '[]',
    created_at       TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS ingested_files (
    path        TEXT PRIMARY KEY,
    checksum    TEXT NOT NULL,
    ingested_at TEXT NOT NULL,
    task_count  INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS daily_budget (
    date                TEXT PRIMARY KEY,
    notifications_sent  INTEGER DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_tasks_status   ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_priority ON tasks(priority DESC);
CREATE INDEX IF NOT EXISTS idx_tasks_score    ON tasks(priority_score DESC);
CREATE INDEX IF NOT EXISTS idx_interactions_tid ON interactions(task_id, ts);
CREATE INDEX IF NOT EXISTS idx_raw_items_processed ON raw_items(processed, created_at);
