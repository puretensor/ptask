-- V013: poison-pill isolation for the native distill pipeline.
--
-- `fetch_unprocessed` serves `processed = 0` rows oldest-first, and a batch
-- that the provider cannot classify was never marked processed — so the very
-- same rows were re-served on every run, forever, while newer captures piled
-- up behind them. One unprocessable memo wedged all distillation until a
-- human deleted the row.
--
-- `distill_attempts` counts isolated provider/classification failures for a
-- row: the chunk containing it was halved until the row was alone and still
-- failed. Database failures during task creation are NOT charged. Once the
-- count reaches the pipeline's ceiling the row stops being served and the run
-- continues past it; `distill_error` keeps the last reason for triage.
--
-- KNOWN EXPOSURE, stated plainly rather than assumed away: an attempt is
-- charged whether or not anything else succeeded in the same run. A total
-- provider outage — a bad model deploy, a schema regression in the structured
-- output — therefore charges every row it bisects down to. At the current
-- CHUNK/MAX_PROVIDER_CALLS settings that is roughly 31 captures per run, so
-- three consecutive fully-failing hourly runs could quarantine ~31 good rows.
-- Quarantine is recoverable (the rows are retained and countable, and
-- `distill_attempts` can be reset), the run still fails closed, and
-- `pt_distill_quarantined_captures` alerts on it. See the "Poison captures and
-- quarantine" section of docs/operations.md.

ALTER TABLE raw_items ADD COLUMN distill_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE raw_items ADD COLUMN distill_error TEXT DEFAULT NULL;

CREATE INDEX IF NOT EXISTS idx_raw_items_pending
    ON raw_items (processed, distill_attempts, id);
