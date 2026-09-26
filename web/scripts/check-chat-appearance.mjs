import assert from "node:assert/strict";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "@playwright/test";
import { createServer, preview } from "vite";

const output = await mkdtemp(join(tmpdir(), "execlaw-appearance-"));
const fixture = `
import React from "react";
import { createRoot } from "react-dom/client";
import { MessageStream } from "/src/chat/MessageStream.tsx";
import { AuthContext } from "/src/auth/AuthContext.tsx";
import { setMessages, appendStreamingToken } from "/src/chat/store.ts";
import "/src/styles/theme.scss";
import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/600.css";
const message = (seq, text, kind = "user_msg", channel_origin = "web") => ({
    seq, text, kind, channel_origin, actor: kind === "model_turn" ? "agent" : "Operator",
    committed_at: 1789580800 + seq * 60,
});
localStorage.setItem("execlaw.chat.appearance", "nexus");
setMessages("appearance-check", [
    message(1, "Please reconcile the supplier update with the shipment records."),
    { ...message(2, "The shipment is scheduled for Thursday. Is the warehouse ready?", "user_msg", "signal"), transport_context: "Maya Chen / Warehouse team" },
    { ...message(3, "The revised invoice total is $12,480.", "user_msg", "email"), transport_context: "Procurement / supplier@example.test" },
    { ...message(4, '{"shipment":"SHP-2048","warehouse":"ready"}', "tool_result"), actor: "warehouse-mcp" },
    { ...message(5, "The warehouse is ready for **Thursday**. Review the response before sending.", "model_turn", "signal"), reply_to_seq: 2 },
    message(6, "Please keep the receiving team in the loop.", "user_msg", "whatsapp"),
    { ...message(7, "I have prepared the supplier response.", "model_turn", "whatsapp"), reply_to_seq: 6 },
    message(8, "Long source labels must wrap without changing the timeline width.", "user_msg", "custom-database-with-a-long-source-identifier"),
    { ...message(9, "A reply with unavailable source history.", "model_turn"), reply_to_seq: 0 },
]);
appendStreamingToken("appearance-check", "Checking the remaining records...");
document.body.style.cssText = "margin:0;min-width:0";
const mount = document.getElementById("root");
mount.style.cssText = "height:100dvh;display:flex;flex-direction:column;max-width:1120px;margin:auto";
createRoot(mount).render(React.createElement(AuthContext.Provider, {
    value: { status: "authenticated", user: null, tokens: null, getAccessToken: () => "fixture-token" },
}, React.createElement(MessageStream, { conversationId: "appearance-check", onSendTransportReply: async () => {} })));
`;

const server = await createServer({
    root: fileURLToPath(new URL("..", import.meta.url)),
    server: { host: "127.0.0.1", port: 0, strictPort: false, open: false },
    plugins: [{
        name: "appearance-check-fixture",
        resolveId(id) { if (id === "virtual:appearance-check") return "\0appearance-check"; },
        load(id) { if (id === "\0appearance-check") return fixture; },
    }],
});
let browser;
let productionServer;
try {
    await server.listen();
    const address = server.httpServer.address();
    const origin = `http://127.0.0.1:${address.port}`;
    browser = await chromium.launch();
    const page = await browser.newPage();
    page.setDefaultNavigationTimeout(120_000);
    page.setDefaultTimeout(120_000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.route(`${origin}/api/**`, route => {
        const path = new URL(route.request().url()).pathname;
        if (path.endsWith("/nexus")) return route.fulfill({ contentType: "application/json", body: JSON.stringify({
            annotations: [
                { seq: 2, branch_id: "Shipment", tags: ["urgent"], links: [] },
                { seq: 5, branch_id: "Shipment", tags: ["urgent"], links: [{ target_seq: 2, relation: "replies_to" }] },
            ], views: [{ name: "Urgent", filters: { tag: "urgent" } }],
        }) });
        if (path.endsWith("/messages/search")) return route.fulfill({ contentType: "application/json", body: JSON.stringify({ matches: [
            { seq: 2, text: "The shipment is scheduled for Thursday", source: "signal", committed_at: 1789580920 },
        ], has_more: false }) });
        return route.fulfill({ contentType: "application/json", body: '{"saved":true}' });
    });
    const html = await server.transformIndexHtml("/__appearance_check", '<html><head><meta name="viewport" content="width=device-width, initial-scale=1"></head><body><div id="root"></div><script type="module" src="/@id/virtual:appearance-check"></script></body></html>');
    await page.route(`${origin}/__appearance_check`, route => route.fulfill({ contentType: "text/html", body: html }));
    await page.goto(`${origin}/__appearance_check`);
    await page.getByRole("navigation", { name: "Message source navigation" }).waitFor();
    await page.getByRole("button", { name: "Collapse branch Shipment" }).waitFor();
    for (const theme of ["dark", "light"]) {
        for (const width of [1440, 390, 320]) {
            await page.setViewportSize({ width, height: 900 });
            await page.evaluate(theme => document.documentElement.setAttribute("data-bs-theme", theme), theme);
            await page.getByRole("combobox", { name: "Message source" }).selectOption("Email");
            await page.getByRole("button", { name: "Next matching message" }).click();
            assert.equal(await page.evaluate(() => document.activeElement.dataset.messageSeq), "3");
            await page.getByRole("button", { name: /Reply to #2/ }).click();
            assert.equal(await page.evaluate(() => document.activeElement.dataset.messageSeq), "2");
            assert.equal(await page.getByRole("button", { name: /Source not loaded/ }).isDisabled(), true);
            const geometry = await page.evaluate(() => ({
                overflow: document.documentElement.scrollWidth > innerWidth,
                badElements: [...document.querySelectorAll(".execlaw-msg, .execlaw-nexus__navigator, .execlaw-msg__bubble")]
                    .filter(element => element.scrollWidth > element.clientWidth + 1).map(element => element.className),
                background: getComputedStyle(document.querySelector(".execlaw-nexus")).backgroundColor,
            }));
            assert.equal(geometry.overflow, false, `${theme}/${width}: document overflow`);
            assert.deepEqual(geometry.badElements, [], `${theme}/${width}: element overflow`);
            assert.equal(geometry.background, theme === "light" ? "rgb(250, 251, 252)" : "rgb(21, 23, 25)");
            await page.screenshot({ path: join(output, `${theme}-${width}.png`) });
            console.log(`PASS ${theme} ${width}px: layout, source navigation, missing history`);
        }
    }
    await page.getByRole("button", { name: "Collapse branch Shipment" }).click();
    assert.equal(await page.locator('[data-message-seq="2"]').count(), 0);
    assert.equal(await page.locator('[data-message-seq="5"]').count(), 1);
    await page.getByRole("button", { name: /Reply to #2/ }).click();
    assert.equal(await page.locator('[data-message-seq="2"]').count(), 1);
    await page.getByRole("searchbox", { name: "Search messages" }).fill("shipment");
    await page.getByRole("button", { name: "Search full history" }).click();
    await page.getByRole("region", { name: "Full history search results" }).getByRole("button", { name: /signal #2/ }).waitFor();
    await page.evaluate(() => {
        localStorage.setItem("execlaw.chat.appearance", "classic");
        window.dispatchEvent(new StorageEvent("storage", { key: "execlaw.chat.appearance", newValue: "classic" }));
    });
    await page.locator(".execlaw-nexus").waitFor({ state: "detached" });
    assert.equal(await page.locator("[data-message-seq]").count(), 9);
    assert.equal(await page.getByTestId("streaming-bubble").count(), 1);
    assert.deepEqual(errors, []);
    console.log(`PASS Classic restoration and streaming; screenshots: ${output}`);
    productionServer = await preview({
        root: fileURLToPath(new URL("..", import.meta.url)),
        preview: { host: "127.0.0.1", port: 0, strictPort: false, open: false },
    });
    const productionOrigin = `http://127.0.0.1:${productionServer.httpServer.address().port}`;
    const productionPage = await browser.newPage();
    productionPage.on("pageerror", error => errors.push(error.message));
    await productionPage.route(`${productionOrigin}/api/**`, route => {
        const pathname = new URL(route.request().url()).pathname;
        return route.fulfill(pathname === "/api/ping"
            ? { status: 200, contentType: "text/plain", body: "pong" }
            : { status: 401, contentType: "application/json", body: '{"error":"unauthorized"}' });
    });
    await productionPage.goto(productionOrigin);
    await productionPage.locator('input[type="password"]').waitFor();
    assert.equal(new URL(productionPage.url()).pathname, "/login");
    assert.deepEqual(errors, []);
    console.log("PASS production bundle boot and login routing (mocked API)");
} finally {
    await browser?.close();
    await productionServer?.close();
    await server.close();
}