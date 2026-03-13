-- Drop columns that are only needed for event replay, not for index lookups.
-- Their data lives in the event log; the index table shouldn't duplicate it.
ALTER TABLE tasks DROP COLUMN skill_name;
ALTER TABLE tasks DROP COLUMN params_json;
