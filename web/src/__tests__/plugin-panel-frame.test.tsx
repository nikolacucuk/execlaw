import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { AuthProvider } from "../auth/AuthContext";
import { DynamicPluginPanel } from "../settings/DynamicPluginPanel";

const meResponse = () => new Response(JSON.stringify({
    user_id: "controller-1",
    username: "controller",
    display_name: "Controller",
    email: null,
    role: "controller",
    last_login_at: null,
}), { status: 200 });

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
    localStorage.setItem("execlaw.access_token", "panel-secret-token");
    localStorage.setItem("execlaw.refresh_token", "panel-refresh-secret");
    fetchMock = vi.fn(async (url: string) => {
        if (url === "/api/admin/me") return meResponse();
        if (url === "/api/admin/plugins/ui_panels") return new Response(JSON.stringify({
            panels: [{
                plugin_id: "fixture-panel",
                mount: "admin/plugins/fixture-panel",
                entry: "ui/panel.js",
                rpc_routes: [{ method: "GET", path: "/api/admin/plugins/fixture-panel/status" }],
                oauth_accounts: [],
                rpc_capabilities: ["plugin_admin_routes"],
            }],
        }), { status: 200 });
        if (url === "/api/admin/plugins/fixture-panel/ui/panel.js") return new Response(
            "export default function Panel(){ return null }",
            { status: 200, headers: { "content-type": "application/javascript" } },
        );
        return new Response("{}", { status: 200 });
    });
    vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => vi.unstubAllGlobals());

describe("DynamicPluginPanel sandbox", () => {
    it("mounts the bundle in an opaque-origin frame without passing bearer data into its document", async () => {
        render(
            <AuthProvider>
                <DynamicPluginPanel
                    pluginId="fixture-panel"
                    pluginVersion="1.0.0"
                    pluginDisplayName="Fixture panel"
                    onConfigChanged={() => {}}
                />
            </AuthProvider>,
        );

        const frame = await screen.findByTestId("plugin-panel-frame");
        expect(frame).toHaveAttribute("sandbox", "allow-scripts");
        expect(frame).toHaveAttribute("referrerpolicy", "no-referrer");
        const source = frame.getAttribute("srcdoc") ?? "";
        expect(source).toContain("connect-src 'none'");
        expect(source).toContain("plugin-panel-frame.js");
        expect(source).not.toContain("panel-secret-token");
        expect(source).not.toContain("panel-refresh-secret");
        expect(source).not.toContain("Authorization");
        expect(source).not.toContain("<script>");

        await waitFor(() => expect(fetchMock).toHaveBeenCalledWith(
            "/api/admin/plugins/fixture-panel/ui/panel.js",
            expect.objectContaining({
                headers: { Authorization: "Bearer panel-secret-token" },
            }),
        ));
    });
});
