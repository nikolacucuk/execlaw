use serde_json::{Value, json};
use std::io::{BufRead, Write};

const PROTOCOL_VERSION: &str = "2025-06-18";

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line.expect("read MCP fixture request");
        let request: Value = serde_json::from_str(&line).expect("decode MCP fixture request");
        let method = request["method"].as_str().expect("MCP method");
        match method {
            "initialize" => {
                assert_eq!(request["params"]["protocolVersion"], PROTOCOL_VERSION);
                write_frame(
                    &mut output,
                    &json!({
                        "jsonrpc":"2.0",
                        "id":request["id"],
                        "result":{
                            "protocolVersion":PROTOCOL_VERSION,
                            "capabilities":{"tools":{}},
                            "serverInfo":{"name":"pinned-stdio-fixture","version":"1"}
                        }
                    }),
                );
            }
            "notifications/initialized" => assert!(request.get("id").is_none()),
            "tools/list" => {
                write_frame(
                    &mut output,
                    &json!({
                        "jsonrpc":"2.0",
                        "id":request["id"].as_u64().expect("tools/list id") + 100,
                        "result":{"tools":[]}
                    }),
                );
                write_frame(
                    &mut output,
                    &json!({
                        "jsonrpc":"2.0",
                        "id":request["id"],
                        "result":{"tools":[{"name":"fixture_ping","inputSchema":{"type":"object"}}]}
                    }),
                );
            }
            "tools/call" => write_frame(
                &mut output,
                &json!({
                    "jsonrpc":"2.0",
                    "id":request["id"],
                    "result":{"content":[{"type":"text","text":"pong"}]}
                }),
            ),
            _ => panic!("unexpected MCP stdio fixture method: {method}"),
        }
    }
}

fn write_frame(output: &mut impl Write, value: &Value) {
    serde_json::to_writer(&mut *output, value).expect("encode MCP fixture response");
    output
        .write_all(b"\n")
        .expect("write MCP fixture delimiter");
    output.flush().expect("flush MCP fixture response");
}
