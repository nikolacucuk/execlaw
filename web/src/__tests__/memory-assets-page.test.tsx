import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryAssetsPage } from "../settings/MemoryAssetsPage";
import { AuthProvider } from "../auth/AuthContext";

let fetchMock: ReturnType<typeof vi.fn>;

function response(body: unknown) {
    return new Response(JSON.stringify(body), { status: 200 });
}

function meResponse() {
    return response({
        user_id: "ctrl-1",
        username: "ctrl",
        display_name: "Controller",
        email: null,
        role: "controller",
        last_login_at: null,
    });
}

const assets = {
    assets: [{
        asset_id: "asset-language",
        asset_type: "memory",
        name: "Preferred language",
        description: "Language preference",
        owner_scope: "global",
        visibility: "private",
        trust_floor: "Controller",
        status: "active",
        version: 2,
        source_hash: "sha256-example",
        expires_at: null,
    }],
    bindings: [{
        asset_id: "asset-language",
        agent_scope: "default",
        injection_mode: "hot",
        priority: 10,
        max_chars: 256,
        created_at: 1,
    }],
    agent_scopes: [{ id: "default", name: "Default chat agent" }],
};

describe("MemoryAssetsPage", () => {
    beforeEach(() => {
        localStorage.setItem("execlaw.access_token", "tok");
        localStorage.setItem("execlaw.refresh_token", "tok");
        fetchMock = vi.fn(async (url: string) => {
            if (url === "/api/admin/me") return meResponse();
            if (url === "/api/admin/memory-assets") return response(assets);
            return response({});
        });
        vi.stubGlobal("fetch", fetchMock);
    });

    afterEach(() => vi.unstubAllGlobals());

    it("shows bindings and saves/removes them through the controller API", async () => {
        render(<AuthProvider><MemoryAssetsPage /></AuthProvider>);
        await waitFor(() => expect(screen.getByTestId("memory-binding-row")).toBeInTheDocument());
        expect(screen.getByText("Preferred language")).toBeInTheDocument();

        fireEvent.click(screen.getByRole("button", { name: "Bind asset" }));
        await waitFor(() => expect(fetchMock).toHaveBeenCalledWith(
            "/api/admin/memory-assets/asset-language/binding",
            expect.objectContaining({ method: "PUT" }),
        ));
        const bindCall = fetchMock.mock.calls.find(([url, init]) =>
            url === "/api/admin/memory-assets/asset-language/binding" && init?.method === "PUT",
        );
        expect(JSON.parse((bindCall?.[1] as RequestInit).body as string)).toMatchObject({
            agent_scope: "default",
            injection_mode: "hot",
            max_chars: 512,
        });

        fireEvent.click(screen.getByRole("button", { name: "Unbind Preferred language" }));
        await waitFor(() => expect(fetchMock).toHaveBeenCalledWith(
            "/api/admin/memory-assets/asset-language/binding",
            expect.objectContaining({ method: "DELETE" }),
        ));
    });
});
