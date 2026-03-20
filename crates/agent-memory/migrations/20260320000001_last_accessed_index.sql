-- Index for efficient decay queries on last_accessed.
CREATE INDEX IF NOT EXISTS idx_natural_memories_last_accessed ON natural_memories(last_accessed);
