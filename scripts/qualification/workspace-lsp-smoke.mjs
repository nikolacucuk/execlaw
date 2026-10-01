#!/usr/bin/env node
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const image = process.argv[2];
if (!image || !/^sha256:[0-9a-f]{64}$/.test(image)) {
  process.stderr.write("usage: node scripts/qualification/workspace-lsp-smoke.mjs <local-image-id>\n");
  process.exit(2);
}

const workspace = mkdtempSync(join(tmpdir(), "execlaw-lsp-smoke-"));
mkdirSync(join(workspace, "src"));
writeFileSync(join(workspace, "Cargo.toml"), "[package]\nname = \"lsp-smoke\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
writeFileSync(join(workspace, "Cargo.lock"), "version = 4\n\n[[package]]\nname = \"lsp-smoke\"\nversion = \"0.1.0\"\n");
writeFileSync(join(workspace, "src/lib.rs"), "pub mod broken;\n");
const source = "pub fn broken( -> i32 {\n";
writeFileSync(join(workspace, "src/broken.rs"), source);

const args = [
  "run", "--rm", "--interactive", "--network", "none", "--read-only",
  "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true",
  "--pids-limit", "128", "--memory", "2g", "--cpus", "2",
  "--tmpfs", "/tmp:rw,exec,nosuid,nodev,size=128m,mode=1777",
  "--mount", `type=bind,source=${workspace},target=/workspace,readonly`,
  "--workdir", "/workspace", "-e", "HOME=/tmp", "-e", "CARGO_NET_OFFLINE=true",
  "-e", "CARGO_TARGET_DIR=/tmp/target",
  "--entrypoint", "/usr/bin/timeout",
  image, "--signal=TERM", "--kill-after=2s", "90s", "rust-analyzer",
];
const sandboxPrefix = [
  "run", "--rm", "--network", "none", "--read-only", "--cap-drop", "ALL",
  "--security-opt", "no-new-privileges:true", "--pids-limit", "128",
  "--memory", "2g", "--cpus", "2", "--tmpfs", "/tmp:rw,exec,nosuid,nodev,size=128m,mode=1777",
  "--mount", `type=bind,source=${workspace},target=/workspace,readonly`,
  "--workdir", "/workspace", "--entrypoint", "/usr/bin/timeout", image,
  "--signal=TERM", "--kill-after=2s", "5s",
];
const networkProbe = spawnSync("docker", [
  ...sandboxPrefix,
  "node", "-e",
  "const net=require('node:net');const socket=net.createConnection({host:'1.1.1.1',port:80,timeout:1000});socket.on('connect',()=>process.exit(2));socket.on('error',()=>process.exit(0));socket.on('timeout',()=>process.exit(0));",
], { encoding: "utf8", timeout: 10_000 });
if (networkProbe.status !== 0) {
  throw new Error(`network sandbox probe expected a denied connection; exit=${networkProbe.status} ${networkProbe.stderr}`);
}
const writeProbe = spawnSync("docker", [
  ...sandboxPrefix,
  "node", "-e", "require('node:fs').writeFileSync('/workspace/src/lib.rs','modified')",
], { encoding: "utf8", timeout: 10_000 });
if (writeProbe.status === 0) throw new Error("read-only workspace mount allowed a container write");
process.stdout.write("workspace_sandbox_probes_ok network=denied workspace_mount=read_only\n");
const child = spawn("docker", args, { stdio: ["pipe", "pipe", "pipe"] });

let buffer = Buffer.alloc(0);
let stderr = "";
const queued = [];
let wakeReader;
let stdoutClosed = false;
child.stdout.on("data", (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  wakeReader?.();
});
child.stdout.on("end", () => {
  stdoutClosed = true;
  wakeReader?.();
});
child.stderr.on("data", (chunk) => {
  stderr = (stderr + chunk.toString("utf8")).slice(-8192);
});
child.on("error", (error) => {
  queued.push({ _spawn_error: error.message });
  wakeReader?.();
});

function frame(message) {
  const body = Buffer.from(JSON.stringify(message));
  return Buffer.concat([Buffer.from(`Content-Length: ${body.length}\r\n\r\n`), body]);
}

function popFrame() {
  const divider = buffer.indexOf("\r\n\r\n");
  if (divider < 0) return null;
  const header = buffer.subarray(0, divider).toString("ascii");
  const length = Number(header.match(/(?:^|\r\n)Content-Length:\s*(\d+)/i)?.[1]);
  if (!Number.isInteger(length) || length < 0 || length > 1024 * 1024) {
    throw new Error("invalid or oversized LSP Content-Length");
  }
  const start = divider + 4;
  if (buffer.length < start + length) return null;
  const body = buffer.subarray(start, start + length);
  buffer = buffer.subarray(start + length);
  return JSON.parse(body.toString("utf8"));
}

async function nextMessage(deadline) {
  for (;;) {
    const message = popFrame();
    if (message) return message;
    if (queued.length) throw new Error(queued.shift()._spawn_error);
    if (stdoutClosed) throw new Error(`language server closed stdout; stderr: ${stderr}`);
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw new Error(`language server response timed out; stderr: ${stderr}`);
    await new Promise((resolve) => {
      const timer = setTimeout(resolve, remaining);
      wakeReader = () => { clearTimeout(timer); wakeReader = undefined; resolve(); };
    });
  }
}

function send(message) {
  child.stdin.write(frame(message));
}

async function replyToServerRequest(message) {
  if (!Object.hasOwn(message, "id")) return;
  const method = message.method;
  const items = message.params?.items;
  const result = method === "workspace/configuration"
    ? Array.isArray(items) ? items.slice(0, 64).map(() => null) : []
    : method === "workspace/workspaceFolders"
      ? [{ uri: "file:///workspace", name: "workspace" }]
      : method === "workspace/applyEdit"
        ? { applied: false, failureReason: "edits require the workspace patch tool" }
        : null;
  send({ jsonrpc: "2.0", id: message.id, result });
}

async function main() {
  const uri = "file:///workspace/src/broken.rs";
  const initDeadline = Date.now() + 30_000;
  send({
    jsonrpc: "2.0", id: 1, method: "initialize", params: {
      processId: null,
      rootUri: "file:///workspace/",
      workspaceFolders: [{ uri: "file:///workspace/", name: "workspace" }],
      capabilities: { general: { positionEncodings: ["utf-16"] }, workspace: { configuration: true, workspaceFolders: true }, textDocument: { synchronization: { didSave: true }, publishDiagnostics: { relatedInformation: true } } },
      clientInfo: { name: "execlaw-lsp-smoke", version: "1" },
      initializationOptions: { diagnostics: { enable: true }, checkOnSave: true, cargo: { allTargets: true } },
    },
  });
  let initialized = false;
  let pullDiagnostics = false;
  while (!initialized) {
    const message = await nextMessage(initDeadline);
    if (message.id === 1 && !message.method) {
      if (message.error) throw new Error(`initialize failed: ${JSON.stringify(message.error)}`);
      pullDiagnostics = Boolean(message.result?.capabilities?.diagnosticProvider);
      initialized = true;
    } else if (message.method && Object.hasOwn(message, "id")) {
      await replyToServerRequest(message);
    }
  }
  send({ jsonrpc: "2.0", method: "initialized", params: {} });
  send({ jsonrpc: "2.0", method: "workspace/didChangeConfiguration", params: { settings: { "rust-analyzer": { diagnostics: { enable: true }, checkOnSave: true, cargo: { allTargets: true } } } } });
  send({ jsonrpc: "2.0", method: "textDocument/didOpen", params: { textDocument: { uri, languageId: "rust", version: 1, text: source } } });
  send({ jsonrpc: "2.0", method: "textDocument/didChange", params: { textDocument: { uri, version: 2 }, contentChanges: [{ text: source }] } });
  send({ jsonrpc: "2.0", method: "textDocument/didSave", params: { textDocument: { uri }, text: source } });
  if (pullDiagnostics) {
    send({ jsonrpc: "2.0", id: 2, method: "textDocument/diagnostic", params: { textDocument: { uri }, identifier: null, previousResultId: null } });
  }

  const diagnosticsDeadline = Date.now() + 45_000;
  let emptyPushAt = 0;
  for (;;) {
    let message;
    try {
      message = await nextMessage(emptyPushAt ? Math.min(diagnosticsDeadline, emptyPushAt + 5000) : diagnosticsDeadline);
    } catch (error) {
      if (emptyPushAt && Date.now() >= emptyPushAt + 5000) break;
      throw error;
    }
    if (message.method === "textDocument/publishDiagnostics" && message.params?.uri === uri) {
      const items = message.params.diagnostics ?? [];
      if (items.length) {
        if (!items.some((item) => item.severity === 1)) throw new Error(`expected error diagnostic, got ${JSON.stringify(items)}`);
        process.stdout.write(`workspace_lsp_smoke_ok diagnostics=${items.length}\n`);
        break;
      }
      emptyPushAt = Date.now();
    } else if (message.id === 2 && !message.method) {
      if (message.error) throw new Error(`diagnostics request failed: ${JSON.stringify(message.error)}`);
      const items = message.result?.items ?? [];
      if (items.some((item) => item.severity === 1)) {
        process.stdout.write(`workspace_lsp_smoke_ok diagnostics=${items.length}\n`);
        break;
      }
      emptyPushAt = Date.now();
    } else if (message.method && Object.hasOwn(message, "id")) {
      await replyToServerRequest(message);
    }
    if (Date.now() >= diagnosticsDeadline) throw new Error("diagnostics deadline exceeded");
  }

  send({ jsonrpc: "2.0", method: "textDocument/didClose", params: { textDocument: { uri } } });
  send({ jsonrpc: "2.0", method: "exit", params: null });
  child.stdin.end();
  const code = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", resolve);
  });
  if (code !== 0) throw new Error(`docker/language-server process exited ${code}; stderr: ${stderr}`);
}

try {
  await main();
} catch (error) {
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  child.kill();
  process.exitCode = 1;
} finally {
  rmSync(workspace, { recursive: true, force: true });
}
