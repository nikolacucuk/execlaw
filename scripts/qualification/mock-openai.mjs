// Local-only inference fixture for disposable process-kill qualification.
import http from "node:http";

const port = Number(process.env.EXECLAW_MOCK_PORT ?? "30069");
const attempts = new Map();
let requests = 0;
let held = 0;

function sendJson(response, status, body) {
  response.writeHead(status, { "content-type": "application/json" });
  response.end(JSON.stringify(body));
}

function messageText(messages) {
  return JSON.stringify(messages ?? []);
}

const server = http.createServer(async (request, response) => {
  if (request.method === "GET" && request.url === "/__harness/state") {
    sendJson(response, 200, { requests, held, attempts: Object.fromEntries(attempts) });
    return;
  }
  if (request.method === "GET" && request.url === "/v1/models") {
    sendJson(response, 200, { object: "list", data: [{ id: "mock-local", object: "model" }] });
    return;
  }
  if (request.method !== "POST" || request.url !== "/v1/chat/completions") {
    sendJson(response, 404, { error: "not found" });
    return;
  }
  let encoded = "";
  for await (const chunk of request) {
    encoded += chunk.toString();
    if (encoded.length > 1_000_000) {
      sendJson(response, 413, { error: "request too large" });
      return;
    }
  }
  let body;
  try {
    body = JSON.parse(encoded);
  } catch {
    sendJson(response, 400, { error: "invalid JSON" });
    return;
  }
  requests += 1;
  const text = messageText(body.messages);
  const scenario = text.includes("H023_TOOL_HOLD")
    ? "H023_TOOL_HOLD"
    : text.includes("H023_HOLD") ? "H023_HOLD" : "default";
  const attempt = (attempts.get(scenario) ?? 0) + 1;
  attempts.set(scenario, attempt);
  if (scenario === "H023_HOLD" && attempt === 1) {
    held += 1;
    request.socket.on("close", () => { held -= 1; });
    return;
  }
  if (scenario === "H023_TOOL_HOLD" && attempt === 2) {
    held += 1;
    request.socket.on("close", () => { held -= 1; });
    return;
  }
  if (scenario === "H023_TOOL_HOLD" && attempt === 1 && body.stream) {
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.write(`data: ${JSON.stringify({ id: "mock-tool", model: "mock-local", choices: [{ index: 0, delta: {
      tool_calls: [{ index: 0, id: "call-qualification-artifact", type: "function",
        function: { name: "qualification_artifact.create", arguments: "{}" } }],
    } }] })}\n\n`);
    response.write(`data: ${JSON.stringify({ id: "mock-tool", model: "mock-local", choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] })}\n\n`);
    response.end("data: [DONE]\n\n");
    return;
  }
  const schema = body.response_format?.json_schema?.schema;
  if (schema) {
    const content = schema.type === "array"
      ? '["local"]'
      : schema.properties?.status ? '{"status":"READY"}' : '{"count":2}';
    sendJson(response, 200, {
      id: "mock-schema", model: "mock-local",
      choices: [{ index: 0, message: { role: "assistant", content }, finish_reason: "stop" }],
    });
    return;
  }
  if (JSON.stringify(body.tools ?? []).includes("probe_noop")) {
    sendJson(response, 200, {
      id: "mock-tool", model: "mock-local",
      choices: [{ index: 0, message: { role: "assistant", content: null,
        tool_calls: [{ id: "call-probe", type: "function", function: { name: "probe_noop", arguments: "{}" } }] },
        finish_reason: "tool_calls" }],
    });
    return;
  }
  if (body.stream) {
    response.writeHead(200, {
      "content-type": "text/event-stream",
      "cache-control": "no-cache",
      connection: "close",
    });
    response.write(`data: ${JSON.stringify({ id: "mock", model: "mock-local", choices: [{ index: 0, delta: { content: "harness-ok" } }] })}\n\n`);
    response.write(`data: ${JSON.stringify({ id: "mock", model: "mock-local", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\n`);
    response.end("data: [DONE]\n\n");
    return;
  }
  const promptTokens = text.length > 10_000
    ? Math.max(8, Math.floor(text.length / 6) - 32) : 10;
  sendJson(response, 200, {
    id: "mock",
    object: "chat.completion",
    model: "mock-local",
    choices: [{ index: 0, message: { role: "assistant", content: "harness-ok" }, finish_reason: "stop" }],
    usage: { prompt_tokens: promptTokens, completion_tokens: 3,
      total_tokens: promptTokens + 3 },
  });
});

server.listen(port, process.env.EXECLAW_MOCK_BIND ?? "127.0.0.1");
