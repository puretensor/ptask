-- V020: the pending-digest dedupe is scoped to the requester.
--
-- idx_approvals_pending_digest was UNIQUE on digest alone, so a second
-- agent requesting the same payload got the first agent's AP-n back (and
-- could withdraw-check or consume against it) instead of its own request.
-- Dedupe is per requester: the same actor re-requesting a pending digest
-- is idempotent; a different actor gets a fresh row. lower() is ASCII
-- case folding, matching approvals::same_actor ("HAL" is "hal").
-- Existing data cannot violate the new index: it is strictly looser.

DROP INDEX IF EXISTS idx_approvals_pending_digest;

CREATE UNIQUE INDEX idx_approvals_pending_requester_digest
    ON approvals(lower(requester), digest) WHERE status = 'pending';
