-- Incoming + outgoing webhook envelopes for audit.
CREATE TABLE IF NOT EXISTS pt_webhook_log (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    direction    TEXT NOT NULL,                 -- 'in' | 'out'
    source       TEXT NOT NULL,                 -- gitea|github|hal|telegram|email
    payload      TEXT NOT NULL,                 -- JSON envelope
    signature_ok INTEGER NOT NULL,              -- 0|1
    ts           TEXT NOT NULL,                 -- ISO datetime
    CHECK (direction IN ('in', 'out')),
    CHECK (signature_ok IN (0, 1))
);

CREATE INDEX IF NOT EXISTS idx_pt_webhook_log_source ON pt_webhook_log(source, ts);
