-- Saved Views — named filter+grouping+sort, Linear-style.
CREATE TABLE IF NOT EXISTS pt_views (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT NOT NULL UNIQUE,
    filter_dsl TEXT NOT NULL,                  -- raw DSL string, e.g. '(today | overdue) & p1'
    grouping   TEXT,                            -- status|priority|project|none
    sort_by    TEXT,                            -- created_at|priority_score|deadline
    created_at TEXT NOT NULL
);
