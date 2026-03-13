-- Add soft-delete flag to the tasks index table (es-entity pattern).
ALTER TABLE tasks ADD COLUMN deleted INTEGER DEFAULT 0;
