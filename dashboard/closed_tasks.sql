-- Shared by the Python and Rust cockpit APIs. Determine the whole status
-- history before limiting results: an edit or a repeated done event is not
-- another closure. Never infer a close time from a task's updated_at.
WITH status_events AS (
  SELECT id, task_uuid, ts, actor,
         CASE
           WHEN event_type = 'task.created' THEN 'open'
           WHEN event_type = 'task.completed' THEN 'done'
           WHEN event_type = 'task.updated' AND json_valid(payload) THEN
             CASE json_extract(payload, '$.status')
               WHEN 'done' THEN 'done'
               WHEN 'dismissed' THEN 'dismissed'
               ELSE CASE WHEN json_extract(payload, '$.status') IS NOT NULL
                         THEN 'open' END
             END
         END AS kind
    FROM pt_event_log
   WHERE task_uuid IN (SELECT id FROM tasks WHERE status IN ('done', 'dismissed'))
     AND event_type IN ('task.created', 'task.completed', 'task.updated')
), history AS (
  SELECT *,
         LAG(kind) OVER (PARTITION BY task_uuid ORDER BY julianday(ts), id) AS prev,
         ROW_NUMBER() OVER (
           PARTITION BY task_uuid ORDER BY julianday(ts) DESC, id DESC
         ) AS newest,
         SUM(julianday(ts) IS NULL) OVER (PARTITION BY task_uuid) AS invalid_times
    FROM status_events WHERE kind IS NOT NULL
), closures AS (
  SELECT *, ROW_NUMBER() OVER (
           PARTITION BY task_uuid ORDER BY julianday(ts) DESC, id DESC
         ) AS closure_rank
    FROM history
   WHERE kind IN ('done', 'dismissed') AND (prev IS NULL OR prev = 'open')
), latest AS (
  SELECT c.task_uuid, c.id, c.ts, c.actor, h.kind
    FROM closures c JOIN history h ON h.task_uuid = c.task_uuid AND h.newest = 1
   WHERE c.closure_rank = 1 AND h.kind IN ('done', 'dismissed')
     AND h.invalid_times = 0
)
SELECT {columns}, e.pt_id AS pt_id,
       COALESCE((SELECT json_group_array(l.label) FROM task_labels l
                  WHERE l.task_uuid = t.id), '[]') AS labels,
       strftime('%Y-%m-%dT%H:%M:%fZ', c.ts) AS closed_at, c.actor AS closed_by
  FROM tasks t
  LEFT JOIN pt_extensions e ON e.task_uuid = t.id
  LEFT JOIN latest c ON c.task_uuid = t.id AND c.kind = t.status
 WHERE t.status IN ('done', 'dismissed')
 ORDER BY (julianday(c.ts) IS NULL), julianday(c.ts) DESC, c.id DESC, t.id DESC
 LIMIT ?
