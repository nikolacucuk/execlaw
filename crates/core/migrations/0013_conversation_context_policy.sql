-- Migration 0013: per-conversation context-window policy override.
--
-- When NULL the system falls back to the global default (FullReplay /
-- empty string parse_policy behaviour). Operator sets via
-- PATCH /api/admin/conversations/:id/context-policy.
ALTER TABLE state_conversations ADD COLUMN context_window_policy TEXT;
