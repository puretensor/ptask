-- pTask internal counters (PT-N minting, etc).
CREATE TABLE IF NOT EXISTS pt_counters (
    name  TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);

INSERT OR IGNORE INTO pt_counters (name, value) VALUES ('pt_id', 0);
