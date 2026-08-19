-- V014: make raw-item capture idempotent at the database boundary.
-- Keep the earliest row when older versions have already admitted a race.

DELETE FROM raw_items
WHERE id NOT IN (
    SELECT MIN(id)
    FROM raw_items
    GROUP BY source_file, text
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_raw_items_source_file_text
    ON raw_items (source_file, text);
