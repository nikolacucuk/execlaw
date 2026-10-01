use execlaw_mcp_client::{McpClient, StdioSpec};
use std::{collections::HashMap, sync::Arc};

#[tokio::test]
async fn stdio_negotiates_pinned_version_and_correlates_tool_responses() {
    let spec = StdioSpec {
        command: env!("CARGO_BIN_EXE_mcp_stdio_fixture").into(),
        args: Vec::new(),
        env: HashMap::new(),
        cwd: None,
    };
    let shutdown = Arc::new(tokio::sync::Notify::new());
    let client = McpClient::stdio(&spec, shutdown.clone()).await.unwrap();
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "fixture_ping");
    let result = client
        .call_tool("fixture_ping", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(result.content[0]["text"].as_str(), Some("pong"));
    shutdown.notify_one();
}
