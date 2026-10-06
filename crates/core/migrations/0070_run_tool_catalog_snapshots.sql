-- Tool declarations are non-user context and are required to reproduce a
-- consented catalog regression. Prompt text and tool arguments remain hashed.
ALTER TABLE state_run_input_manifests
ADD COLUMN tool_catalog_snapshot_json TEXT;
