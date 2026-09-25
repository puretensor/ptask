-- V018: approvals keep a soft task reference; the journal gets an
-- event_type index.
--
-- 1. V016 declared approvals.task_uuid REFERENCES tasks(id) with no ON
--    DELETE action. With foreign_keys=ON, a task any approval named (even a
--    long-decided one) could never be deleted: `pt rm`, sync task_delete and
--    undo-of-create all failed with "FOREIGN KEY constraint failed", and the
--    approvals_lock_after_decision trigger forbids clearing the column on a
--    decided row. An approval is an audit record like pt_event_log, whose
--    task_uuid is soft for the same reason; reads already LEFT JOIN tasks.
--    SQLite cannot drop a column constraint, so the table is rebuilt: same
--    columns and CHECKs, then the V016 indexes and tamper triggers verbatim.
--    INSERT into the new table does not fire the UPDATE triggers.
--
-- 2. /metrics filters pt_event_log by event_type four times per scrape and
--    temporal dedup once per distill candidate; the only index was
--    (task_uuid, ts), so each was a full scan of the largest table.

CREATE TABLE approvals_v18 (
    id              TEXT PRIMARY KEY,
    seq             INTEGER NOT NULL UNIQUE,
    kind            TEXT NOT NULL CHECK (kind IN ('email','ebay','spend','destroy','external','budget','other')),
    title           TEXT NOT NULL,
    request_note    TEXT,
    payload         BLOB,
    payload_kind    TEXT CHECK (payload_kind IS NULL OR payload_kind IN ('file','json')),
    payload_name    TEXT,
    payload_bytes   INTEGER,
    payload_ref     TEXT,
    digest          TEXT NOT NULL CHECK (length(digest) = 64 AND digest NOT GLOB '*[^0-9a-f]*'),
    requester       TEXT NOT NULL,
    task_uuid       TEXT,
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending','approved','rejected','withdrawn','expired')),
    decided_by      TEXT,
    decided_via     TEXT CHECK (decided_via IS NULL OR decided_via IN ('cli','dashboard','telegram','api')),
    decision_note   TEXT,
    created_at      TEXT NOT NULL,
    decided_at      TEXT,
    expires_at      TEXT,
    notified_at     TEXT,
    consumed_at     TEXT,
    consumed_by     TEXT,
    CHECK ((payload IS NULL) = (payload_kind IS NULL))
);

INSERT INTO approvals_v18 SELECT * FROM approvals;
DROP TABLE approvals;
ALTER TABLE approvals_v18 RENAME TO approvals;

CREATE UNIQUE INDEX idx_approvals_pending_digest
    ON approvals(digest) WHERE status = 'pending';

CREATE INDEX idx_approvals_status_seq ON approvals(status, seq);

-- digest / payload / payload_kind are write-once.
CREATE TRIGGER approvals_immutable_payload
BEFORE UPDATE OF digest, payload, payload_kind ON approvals
BEGIN
    SELECT RAISE(ABORT, 'approvals: digest, payload, and payload_kind are immutable');
END;

-- Once decided, the only legal mutation is the one-time consume latch on
-- an approved row (consumed_at/consumed_by NULL → a value). notified_at
-- may still be set while status is pending (this trigger does not fire).
CREATE TRIGGER approvals_lock_after_decision
BEFORE UPDATE ON approvals
WHEN OLD.status IS NOT 'pending'
BEGIN
    SELECT CASE
        WHEN OLD.status = 'approved'
         AND OLD.consumed_at IS NULL
         AND NEW.consumed_at IS NOT NULL
         AND OLD.consumed_by IS NULL
         AND NEW.consumed_by IS NOT NULL
         AND NEW.id IS OLD.id
         AND NEW.seq IS OLD.seq
         AND NEW.kind IS OLD.kind
         AND NEW.title IS OLD.title
         AND NEW.request_note IS OLD.request_note
         AND NEW.payload IS OLD.payload
         AND NEW.payload_kind IS OLD.payload_kind
         AND NEW.payload_name IS OLD.payload_name
         AND NEW.payload_bytes IS OLD.payload_bytes
         AND NEW.payload_ref IS OLD.payload_ref
         AND NEW.digest IS OLD.digest
         AND NEW.requester IS OLD.requester
         AND NEW.task_uuid IS OLD.task_uuid
         AND NEW.status IS OLD.status
         AND NEW.decided_by IS OLD.decided_by
         AND NEW.decided_via IS OLD.decided_via
         AND NEW.decision_note IS OLD.decision_note
         AND NEW.created_at IS OLD.created_at
         AND NEW.decided_at IS OLD.decided_at
         AND NEW.expires_at IS OLD.expires_at
         AND NEW.notified_at IS OLD.notified_at
        THEN NULL
        ELSE RAISE(ABORT, 'approvals: decided row is immutable except one-time consume')
    END;
END;

CREATE INDEX IF NOT EXISTS idx_pt_event_log_type_ts ON pt_event_log(event_type, ts);
