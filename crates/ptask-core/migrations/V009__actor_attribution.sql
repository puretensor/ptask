-- V009: actor attribution + scoped API tokens + data hygiene.
--
-- 1. pt_event_log.actor — WHO performed each mutation. Before this, HAL,
--    the operator CLI, puresentinel, the dashboard subprocess, and git
--    webhooks were indistinguishable in the journal, which blocked audit,
--    undo, per-agent permissions, and the cockpit activity feed.
ALTER TABLE pt_event_log ADD COLUMN actor TEXT;

-- 2. Named scoped API tokens. token_hash = hex(sha256(token)); the plain
--    token is shown once at creation and never stored. scopes is one of
--    read|capture|write|admin (ordered, each implies the ones before it).
CREATE TABLE IF NOT EXISTS pt_api_tokens (
    token_hash   TEXT PRIMARY KEY,
    client_id    TEXT NOT NULL,
    scopes       TEXT NOT NULL DEFAULT 'write',
    created_at   TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at   TEXT
);
CREATE INDEX IF NOT EXISTS idx_pt_api_tokens_client ON pt_api_tokens(client_id);

-- 3. Hygiene: pt_extensions.status_category was written 'todo' at create
--    and never updated by done/dismiss — 649 of 743 rows disagreed with
--    tasks.status. Backfill terminal states so every consumer reads truth.
UPDATE pt_extensions
SET status_category = (
    SELECT CASE t.status
        WHEN 'done'      THEN 'done'
        WHEN 'dismissed' THEN 'dismissed'
        WHEN 'blocked'   THEN 'blocked'
        ELSE pt_extensions.status_category
    END
    FROM tasks t WHERE t.id = pt_extensions.task_uuid
)
WHERE EXISTS (
    SELECT 1 FROM tasks t
    WHERE t.id = pt_extensions.task_uuid
      AND t.status IN ('done', 'dismissed', 'blocked')
);

-- 4. Hygiene: interactions rows whose task was hard-deleted before deletes
--    emitted tombstones (CASCADE was per-writer; some writers had FK off).
DELETE FROM interactions
WHERE task_id NOT IN (SELECT id FROM tasks);
