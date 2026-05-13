-- Add `project` column to pt_extensions. Set by quick-add `#project` token.
-- Filter DSL (v0.2.5) keys on this column directly.
ALTER TABLE pt_extensions ADD COLUMN project TEXT;
CREATE INDEX IF NOT EXISTS idx_pt_extensions_project ON pt_extensions(project);
