-- Add persistent column to memories table.
-- Persistent memories are exempt from decay (replaces report semantics).
ALTER TABLE memories ADD COLUMN persistent INTEGER NOT NULL DEFAULT 0;

-- Drop report tables (event-sourced reports are replaced by persistent memories).
-- Any existing report data can be re-imported via reindex from markdown files.
DROP TABLE IF EXISTS report_events;
DROP TABLE IF EXISTS reports;

-- Clean up FTS entries that referenced reports.
DELETE FROM memory_fts WHERE memory_type = 'report';
