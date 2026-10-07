-- Bind MCP authentication secrets to the server account that consumes them.
-- Existing agent-managed rows already use an `mcp:<id>/auth_token` name;
-- migrate only references reachable from a configured MCP server.
UPDATE vault_secrets
SET plugin_id = 'mcp:' || substr(
    name,
    5,
    instr(substr(name, 5), '/auth_token') - 1
)
WHERE plugin_id IS NULL
  AND name LIKE 'mcp:%/auth_token'
  AND EXISTS (
      SELECT 1
      FROM config_mcp_servers
      WHERE config_mcp_servers.auth_secret_ref = vault_secrets.name
  );
