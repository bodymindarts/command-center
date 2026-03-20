-- Rename tables: natural_memories → memories, research_reports → reports
ALTER TABLE natural_memories RENAME TO memories;
ALTER TABLE research_reports RENAME TO reports;
ALTER TABLE research_report_events RENAME TO report_events;

-- Update FTS memory_type from 'natural' to 'memory'
UPDATE memory_fts SET memory_type = 'memory' WHERE memory_type = 'natural';
