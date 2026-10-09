-- V021: claims carry an owner and an optional lease.
--
-- task_claim was a status flip (todo → in_progress) with the claimer only
-- in the task.claimed event: no owner on the row, no expiry, no release
-- verb. A crashed agent's claim stayed in_progress until someone noticed,
-- and the reaper skips in_progress, so nothing recovered it.
--
--   claimed_by        actor holding the claim (task_claim / pt claim /
--                     pt start); NULL = unclaimed
--   claimed_at        when it was taken
--   claim_expires_at  lease end, NULL = no lease (never expires on its own).
--                     A heartbeat from the holder pushes it forward; an
--                     expired lease is what `pt reclaim` (and, only when
--                     PTASK_CLAIM_RECLAIM is on, the hourly scoring run)
--                     returns to todo.
--   claim_token       opaque instance id minted on every claim/start that
--                     takes the holder. Heartbeat and unforced release
--                     compare-and-set on this, so two sessions that share
--                     an actor name cannot renew or hand back each other's
--                     claim.
ALTER TABLE tasks ADD COLUMN claimed_by TEXT;
ALTER TABLE tasks ADD COLUMN claimed_at TEXT;
ALTER TABLE tasks ADD COLUMN claim_expires_at TEXT;
ALTER TABLE tasks ADD COLUMN claim_token TEXT;

CREATE INDEX IF NOT EXISTS idx_tasks_claim_expires
    ON tasks(claim_expires_at) WHERE claim_expires_at IS NOT NULL;

-- A claim only means something while the task is in progress. Every path
-- that moves a task out of in_progress (done, dismiss, snooze, a recurring
-- advance, undo, writers this binary does not know about) drops it here,
-- so no claim outlives the work it describes.
CREATE TRIGGER IF NOT EXISTS tasks_claim_ends_with_in_progress
AFTER UPDATE OF status_v2 ON tasks
WHEN NEW.status_v2 <> 'in_progress'
 AND (NEW.claimed_by IS NOT NULL OR NEW.claimed_at IS NOT NULL
      OR NEW.claim_expires_at IS NOT NULL OR NEW.claim_token IS NOT NULL)
BEGIN
    UPDATE tasks SET claimed_by = NULL, claimed_at = NULL,
                     claim_expires_at = NULL, claim_token = NULL
     WHERE id = NEW.id;
END;

-- Backfill: an in_progress task whose latest status-changing event is its
-- task.claimed is held by that event's actor (no lease: none was asked for).
UPDATE tasks
   SET claimed_by = (SELECT e.actor FROM pt_event_log e
                      WHERE e.task_uuid = tasks.id AND e.event_type = 'task.claimed'
                      ORDER BY e.id DESC LIMIT 1),
       claimed_at = (SELECT e.ts FROM pt_event_log e
                      WHERE e.task_uuid = tasks.id AND e.event_type = 'task.claimed'
                      ORDER BY e.id DESC LIMIT 1)
 WHERE status_v2 = 'in_progress'
   AND (SELECT MAX(e.id) FROM pt_event_log e
         WHERE e.task_uuid = tasks.id AND e.event_type = 'task.claimed')
       > COALESCE((SELECT MAX(e.id) FROM pt_event_log e
                    WHERE e.task_uuid = tasks.id
                      AND (e.event_type = 'task.completed'
                           OR (e.event_type = 'task.updated'
                               AND json_valid(e.payload)
                               AND json_extract(e.payload, '$.status') IS NOT NULL))), 0);
