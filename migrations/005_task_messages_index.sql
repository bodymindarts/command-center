-- Accelerate message lookups by task_id (previously full table scans).
CREATE INDEX IF NOT EXISTS idx_task_messages_task_id ON task_messages(task_id, created_at);
