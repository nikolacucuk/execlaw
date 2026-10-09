import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import http from "node:http";
import net from "node:net";
import { test } from "node:test";
import { gzipSync } from "node:zlib";

async function freePort() {
    const socket = net.createServer();
    await new Promise((resolve) => socket.listen(0, "127.0.0.1", resolve));
    const port = socket.address().port;
    await new Promise((resolve) => socket.close(resolve));
    return port;
}

test("API proxy forwards POST with Expect and decodes buffered responses", async (t) => {
    let upstreamRequest;
    const backend = http.createServer(async (request, response) => {
        const chunks = [];
        for await (const chunk of request) chunks.push(chunk);
        upstreamRequest = {
            method: request.method,
            expect: request.headers.expect,
            body: Buffer.concat(chunks).toString(),
        };
        const encoded = gzipSync(Buffer.from('{"accepted":true}'));
        response.writeHead(200, {
            "content-type": "application/json",
            "content-encoding": "gzip",
            "content-length": encoded.length,
        });
        response.end(encoded);
    });
    await new Promise((resolve) => backend.listen(0, "127.0.0.1", resolve));
    t.after(() => backend.close());

    const frontendPort = await freePort();
    const child = spawn(process.execPath, [
        "scripts/local-spa-server.mjs",
        String(backend.address().port),
        String(frontendPort),
    ], { cwd: process.cwd(), stdio: ["ignore", "pipe", "pipe"] });
    t.after(() => child.kill());
    await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("SPA proxy did not start")), 10_000);
        child.stdout.on("data", (data) => {
            if (data.toString().includes(`127.0.0.1:${frontendPort}`)) {
                clearTimeout(timer);
                resolve();
            }
        });
        child.once("exit", (code) => {
            clearTimeout(timer);
            reject(new Error(`SPA proxy exited before listening: ${code}`));
        });
    });

    const body = '{"text":"Reply OK"}';
    const result = await new Promise((resolve, reject) => {
        const request = http.request({
            hostname: "127.0.0.1",
            port: frontendPort,
            path: "/api/chats/test/messages",
            method: "POST",
            headers: { "content-type": "application/json", expect: "100-continue" },
        }, (response) => {
            const chunks = [];
            response.on("data", (chunk) => chunks.push(chunk));
            response.on("end", () => resolve({
                status: response.statusCode,
                headers: response.headers,
                body: Buffer.concat(chunks).toString(),
            }));
        });
        request.on("error", reject);
        request.on("continue", () => request.end(body));
        request.flushHeaders();
    });

    assert.equal(result.status, 200);
    assert.equal(result.body, '{"accepted":true}');
    assert.equal(result.headers["content-encoding"], undefined);
    assert.equal(upstreamRequest.method, "POST");
    assert.equal(upstreamRequest.expect, undefined);
    assert.equal(upstreamRequest.body, body);
});
