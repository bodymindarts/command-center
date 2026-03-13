-- Add context column expected by es-entity's es_query! macro.
ALTER TABLE task_events ADD COLUMN context TEXT DEFAULT NULL;
