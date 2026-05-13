-- Recurrence rules for tasks. Modes: 'fixed' (Todoist 'every'), 'completion' ('every!').
CREATE TABLE IF NOT EXISTS pt_recurrence (
    task_uuid       TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
    rrule           TEXT NOT NULL,            -- RFC 5545 RRULE string
    mode            TEXT NOT NULL,            -- 'fixed' | 'completion'
    original_input  TEXT NOT NULL,            -- e.g. 'every monday at 9am'
    next_occurrence TEXT NOT NULL,            -- ISO datetime
    CHECK (mode IN ('fixed', 'completion'))
);

CREATE INDEX IF NOT EXISTS idx_pt_recurrence_next ON pt_recurrence(next_occurrence);
