-- V017: goal tree (mission ancestry).
--
-- Goals sit above tasks. A task may link to a goal directly (`tasks.goal_id`);
-- otherwise it inherits the parent task's effective goal by walking
-- `parent_uuid`. Human ids are `G-<n>`, minted from `pt_counters.goal_id`
-- the same way tasks mint `PT-<n>` and approvals mint `AP-<n>`.
--
-- `project` is free text and too fragile to key a default goal on — there
-- are no per-project default goals.

INSERT OR IGNORE INTO pt_counters (name, value) VALUES ('goal_id', 0);

CREATE TABLE goals (
    id          TEXT PRIMARY KEY,
    seq         INTEGER NOT NULL UNIQUE,
    title       TEXT NOT NULL,
    why         TEXT,
    parent_id   TEXT REFERENCES goals(id),
    status      TEXT NOT NULL DEFAULT 'active'
                CHECK (status IN ('active', 'achieved', 'abandoned')),
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE INDEX idx_goals_parent ON goals(parent_id);
CREATE INDEX idx_goals_status_seq ON goals(status, seq);

ALTER TABLE tasks ADD COLUMN goal_id TEXT REFERENCES goals(id);
CREATE INDEX idx_tasks_goal ON tasks(goal_id) WHERE goal_id IS NOT NULL;
