-- Track the display sender of a task message (e.g. "ExO", a PM, another
-- task) separately from its content, so `clat log` can render proper
-- attribution instead of always labeling inbound messages "YOU".
-- NULL preserves existing behavior (renders as "YOU").
ALTER TABLE task_messages ADD COLUMN sender TEXT;
