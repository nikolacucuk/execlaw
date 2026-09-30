import { describe, expect, it } from "vitest";
import type { UiPanelSummary } from "../api/endpoints";
import {
    isAllowedPanelRpc,
    isPanelFrameMessageTrusted,
    pluginPanelFrameDocument,
} from "../plugins/panel-sandbox";

const policy: UiPanelSummary = {
    plugin_id: "google-apps",
    mount: "admin/plugins/google-apps",
    entry: "ui/panel.js",
    rpc_routes: [
        { method: "GET", path: "/api/admin/plugins/google-apps/config" },
        { method: "POST", path: "/api/admin/plugins/google-apps/test" },
    ],
    oauth_accounts: ["controller"],
    rpc_capabilities: ["own_plugin_settings", "own_oauth_accounts"],
};

describe("sandboxed plugin panel RPC policy", () => {
    it("permits exact plugin routes and this plugin's OAuth account", () => {
        expect(isAllowedPanelRpc(policy, "get", "/api/admin/plugins/google-apps/config")).toBe(true);
        expect(isAllowedPanelRpc(policy, "POST", "/api/admin/plugins/google-apps/test")).toBe(true);
        expect(isAllowedPanelRpc(policy, "GET", "/api/admin/plugins/google-apps/settings/enabled_modules")).toBe(true);
        expect(isAllowedPanelRpc(policy, "PUT", "/api/admin/plugins/google-apps/settings/enabled_modules")).toBe(true);
        expect(isAllowedPanelRpc(policy, "GET", "/api/admin/oauth/clients/google-apps/controller")).toBe(true);
        expect(isAllowedPanelRpc(policy, "POST", "/api/admin/oauth/clients/google-apps/controller/connect")).toBe(true);
    });

    it("rejects other plugins, methods, accounts, and unrelated Controller routes", () => {
        expect(isAllowedPanelRpc(policy, "DELETE", "/api/admin/plugins/google-apps/config")).toBe(false);
        expect(isAllowedPanelRpc(policy, "GET", "/api/admin/plugins/slack/config")).toBe(false);
        expect(isAllowedPanelRpc(policy, "GET", "/api/admin/oauth/clients/slack/controller")).toBe(false);
        expect(isAllowedPanelRpc(policy, "GET", "/api/admin/oauth/clients/google-apps/other")).toBe(false);
        expect(isAllowedPanelRpc(policy, "POST", "/api/admin/users/other/password")).toBe(false);
        expect(isAllowedPanelRpc(policy, "GET", "https://attacker.example/api/admin/plugins/google-apps/config")).toBe(false);
        expect(isAllowedPanelRpc(policy, "GET", "/api/admin/plugins/google-apps/../users")).toBe(false);
    });

    it("builds an opaque-origin frame with no network or inline script grant", () => {
        const document = pluginPanelFrameDocument("nonce-1", "http://127.0.0.1:3031");
        expect(document).toContain("connect-src 'none'");
        expect(document).toContain("http://127.0.0.1:3031/plugin-panel-frame.js");
        expect(document).toContain("data-frame-nonce=\"nonce-1\"");
        expect(document).not.toContain("<script>");
        expect(document).not.toContain("getAccessToken");
        expect(document).not.toContain("Authorization");
    });

    it("rejects messages from a different window or a stale frame nonce", () => {
        const frame = {} as Window;
        expect(isPanelFrameMessageTrusted(
            { source: frame, data: { nonce: "live" } },
            frame,
            "live",
        )).toBe(true);
        expect(isPanelFrameMessageTrusted(
            { source: {} as Window, data: { nonce: "live" } },
            frame,
            "live",
        )).toBe(false);
        expect(isPanelFrameMessageTrusted(
            { source: frame, data: { nonce: "old" } },
            frame,
            "live",
        )).toBe(false);
    });
});
