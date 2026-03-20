-- Drop legacy pre-sqlx tables if they exist (empty, superseded by natural_memories)
DROP TRIGGER IF EXISTS memories_fts_insert;
DROP TRIGGER IF EXISTS memories_fts_delete;
DROP TRIGGER IF EXISTS memories_fts_update;
DROP TABLE IF EXISTS memories_fts;
DROP TABLE IF EXISTS memory_vectors;
DROP TABLE IF EXISTS memories;

-- Rename tables: natural_memories → memories, research_reports → reports
ALTER TABLE natural_memories RENAME TO memories;
ALTER TABLE research_reports RENAME TO reports;
ALTER TABLE research_report_events RENAME TO report_events;

-- Update FTS memory_type from 'natural' to 'memory'
UPDATE memory_fts SET memory_type = 'memory' WHERE memory_type = 'natural';
