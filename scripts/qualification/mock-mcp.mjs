// Disposable Streamable HTTP MCP endpoint with a second request-counting
// listener for connection-time policy and redirect qualification.
import http from "node:http";

const bind = process.env.EXECLAW_MCP_BIND ?? "0.0.0.0";
const originPort = Number(process.env.EXECLAW_MCP_PORT ?? "30072");
const forbiddenPort = Number(process.env.EXECLAW_MCP_FORBIDDEN_PORT ?? "30073");
const redirectHost = process.env.EXECLAW_MCP_REDIRECT_HOST ?? "127.0.0.1";
let originRequests = 0;
let forbiddenRequests = 0;
const methods = new Map();

function json(response, status, value) {
  response.writeHead(status, { "content-type": "application/json" });
  response.end(JSON.stringify(value));
}

const origin = http.createServer(async (request, response) => {
  if (request.url === "/__harness/state") {
    json(response, 200, { originRequests, forbiddenRequests, methods: Object.fromEntries(methods) });
    return;
  }
  originRequests += 1;
  if (request.url === "/redirect") {
    response.writeHead(302, { location: `http://${redirectHost}:${forbiddenPort}/private` });
    response.end();
    return;
  }
  if (request.url !== "/mcp" || request.method !== "POST") {
    json(response, 404, { error: "not found" });
    return;
  }
  let encoded = "";
  for await (const chunk of request) {
    encoded += chunk;
    if (encoded.length > 1_000_000) {
      json(response, 413, { error: "too large" });
      return;
    }
  }
  let call;
  try {
    call = JSON.parse(encoded);
  } catch {
    json(response, 400, { error: "invalid JSON" });
    return;
  }
  methods.set(call.method, (methods.get(call.method) ?? 0) + 1);
  if (call.method === "notifications/initialized") {
    response.writeHead(202);
    response.end();
    return;
  }
  const result =
    call.method === "initialize"
      ? { protocolVersion: "2025-06-18", capabilities: { tools: {} }, serverInfo: { name: "qualification-mcp", version: "1" } }
      : call.method === "tools/list"
        ? { tools: [{ name: "qualification_ping", description: "Return pong", inputSchema: { type: "object", properties: {}, additionalProperties: false } }] }
        : call.method === "tools/call"
          ? { content: [{ type: "text", text: "pong" }] }
          : null;
  if (result === null) {
    json(response, 200, { jsonrpc: "2.0", id: call.id, error: { code: -32601, message: "unknown method" } });
  } else {
    json(response, 200, { jsonrpc: "2.0", id: call.id, result });
  }
});

const forbidden = http.createServer((_request, response) => {
  forbiddenRequests += 1;
  json(response, 200, { forbidden: true });
});

origin.listen(originPort, bind);
forbidden.listen(forbiddenPort, bind);
