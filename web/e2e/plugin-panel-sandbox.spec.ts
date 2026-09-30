import { createServer, type Server } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { test, expect } from "@playwright/test";
import * as esbuild from "esbuild";
import { pluginPanelFrameDocument } from "../src/plugins/panel-sandbox";

const webRoot = resolve(import.meta.dirname, "..");
let server: Server;
let origin: string;
let panelSource: string;
let allowedRpcCalls = 0;
let unrelatedRouteHits = 0;

test.beforeAll(async () => {
    const build = await esbuild.build({
        entryPoints: [resolve(webRoot, "e2e/fixtures/frame-entry.ts")],
        bundle: true,
        format: "esm",
        platform: "browser",
        target: "es2022",
        write: false,
        nodePaths: [resolve(webRoot, "node_modules")],
    });
    panelSource = build.outputFiles[0].text;
    server = createServer(async (request, response) => {
        const path = new URL(request.url ?? "/", origin || "http://127.0.0.1").pathname;
        if (path === "/") {
            const nonce = "browser-panel-isolation-test";
            const frame = pluginPanelFrameDocument(nonce, origin);
            const escapedFrame = frame.replace(/&/g, "&amp;").replace(/"/g, "&quot;");
            response.writeHead(200, {
                "content-type": "text/html; charset=utf-8",
                "content-security-policy": "default-src 'self'; script-src 'self' blob:; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; frame-src 'self'; frame-ancestors 'none'; object-src 'none'",
            });
            response.end(`<!doctype html><html><body><iframe id="panel" title="plugin panel" sandbox="allow-scripts" referrerpolicy="no-referrer" srcdoc="${escapedFrame}"></iframe><script src="/parent.js"></script></body></html>`);
            return;
        }
        if (path === "/parent.js") {
            response.writeHead(200, { "content-type": "application/javascript; charset=utf-8" });
            response.end(`
                localStorage.setItem("execlaw.access_token", "test-controller-token");
                document.cookie = "execlaw_access=parent-cookie-secret; SameSite=Strict";
                fetch("/panel.js").then((response) => response.text()).then((source) => {
                    const frame = document.getElementById("panel");
                    const nonce = "browser-panel-isolation-test";
                    const allowed = {type:"GET", path:"/api/admin/plugins/fixture/status"};
                    window.addEventListener("message", async (event) => {
                        if (event.source !== frame.contentWindow || !event.data || event.data.nonce !== nonce) return;
                        const message = event.data;
                        if (message.type === "panel_frame_ready") {
                            frame.contentWindow.postMessage({type:"panel_boot", nonce, source, identity:{id:"fixture", displayName:"Fixture", version:"1"}}, "*");
                        } else if (message.type === "panel_rpc_request") {
                            if (message.method !== allowed.type || message.path !== allowed.path) {
                                frame.contentWindow.postMessage({type:"panel_rpc_response", nonce, id:message.id, ok:false, error:"manifest denied"}, "*");
                                return;
                            }
                            const token = localStorage.getItem("execlaw.access_token");
                            const result = await fetch(message.path, {headers:{Authorization:"Bearer " + token}}).then((r) => r.json());
                            frame.contentWindow.postMessage({type:"panel_rpc_response", nonce, id:message.id, ok:true, result}, "*");
                        }
                    });
                });
            `);
            return;
        }
        if (path === "/plugin-panel-frame.js") {
            response.writeHead(200, { "content-type": "application/javascript; charset=utf-8" });
            response.end(await readFile(resolve(webRoot, "public/plugin-panel-frame.js"), "utf8"));
            return;
        }
        if (path === "/panel.js") {
            response.writeHead(200, { "content-type": "application/javascript; charset=utf-8" });
            response.end(panelSource);
            return;
        }
        if (path === "/api/admin/plugins/fixture/status") {
            allowedRpcCalls++;
            if (request.headers.authorization !== "Bearer test-controller-token") {
                response.writeHead(401).end();
                return;
            }
            response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ status: "healthy" }));
            return;
        }
        if (path === "/api/admin/users") {
            unrelatedRouteHits++;
            response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ secret: "must-not-reach-panel" }));
            return;
        }
        response.writeHead(404).end();
    });
    await new Promise<void>((resolveListen) => server.listen(0, "127.0.0.1", resolveListen));
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("test server has no TCP address");
    origin = `http://127.0.0.1:${address.port}`;
});

test.afterAll(async () => {
    await new Promise<void>((resolveClose, reject) => server.close((error) => error ? reject(error) : resolveClose()));
});

test("opaque plugin panel cannot read parent credentials or call undeclared admin routes", async ({ page }) => {
    allowedRpcCalls = 0;
    unrelatedRouteHits = 0;
    await page.goto(origin);
    const frame = page.frameLocator("#panel");
    const result = frame.locator("#panel-results");
    await expect(result).toContainText('"localStorage":"blocked"');
    await expect(result).toContainText('"parentDom":"blocked"');
    await expect(result).toContainText('"cookies":"blocked"');
    await expect(result).toContainText('"directFetch":"blocked"');
    await expect(result).toContainText('"allowedRpc":"healthy"');
    await expect(result).toContainText('"unrelatedRpc":"blocked"');
    expect(await page.evaluate(() => localStorage.getItem("execlaw.access_token"))).toBe("test-controller-token");
    expect(allowedRpcCalls).toBe(1);
    expect(unrelatedRouteHits).toBe(0);
});
