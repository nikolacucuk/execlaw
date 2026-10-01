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
    assertions: [{
        assertion_id: "assertion-language",
        scope: "principal:ctrl-1",
        trust_class: "Controller",
        kind: "profile",
        subject: "controller",
        predicate: "preferred_language",
        object: "English",
        confidence: 1,
        status: "approved",
        observed_from: 1,
        observed_to: null,
        valid_from: 1,
        valid_to: null,
        supersedes_id: null,
        extraction_run_id: "memory-run-1",
        created_event_seq: 4,
        created_at: 1,
        evidence: [{
            evidence_id: "evidence-language",
            conversation_id: "chat-language",
            event_seq: 4,
            payload_path: "$.text",
            quote_hash: "a".repeat(64),
            evidence_kind: "direct_quote",
            created_at: 1,
        }],
        evidence_total: 1,
        review: null,
    }],
};

describe("MemoryAssetsPage", () => {
    beforeEach(() => {
        localStorage.setItem("execlaw.access_token", "tok");
        localStorage.setItem("execlaw.refresh_token", "tok");
        fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
            if (url === "/api/admin/me") return meResponse();
            if (url === "/api/admin/memory-assets") return response(assets);
            if (url === "/api/admin/memory-assets/retrieval-config") {
                return init?.method === "PUT"
                    ? response({ embedding_model_id: "local-embed-v1", reranker_version: "local-hybrid-rrf-v1", updated_at: 1 })
                    : response(null);
            }
            if (url === "/api/admin/memory-assets/embeddings/rebuild") return response({
                embedding_model_id: "local-embed-v1",
                index_id: "local-embed-v1:fixture",
                embedded: 1,
                has_more: false,
            });
            if (url === "/api/admin/memory-assertions/forget-source") return response({
                conversation_id: "chat-language",
                event_seq: 4,
                assertions_hidden: 1,
                evidence_hidden: 1,
            });
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

    it("configures a local embedding model and rebuilds the derived index", async () => {
        render(<AuthProvider><MemoryAssetsPage /></AuthProvider>);
        await waitFor(() => expect(screen.getByTestId("memory-assets-page")).toBeInTheDocument());

        fireEvent.change(screen.getByLabelText("Embedding model ID"), {
            target: { value: "local-embed-v1" },
        });
        fireEvent.click(screen.getByRole("button", { name: "Save retrieval config" }));
        await waitFor(() => expect(fetchMock).toHaveBeenCalledWith(
            "/api/admin/memory-assets/retrieval-config",
            expect.objectContaining({ method: "PUT" }),
        ));
        await waitFor(() => expect(screen.getByRole("button", { name: "Rebuild" })).toBeEnabled());
        fireEvent.click(screen.getByRole("button", { name: "Rebuild" }));
        await waitFor(() => expect(fetchMock).toHaveBeenCalledWith(
            "/api/admin/memory-assets/embeddings/rebuild",
            expect.objectContaining({ method: "POST" }),
        ));
        expect(await screen.findByRole("status")).toHaveTextContent("Index is current.");
    });

    it("forgets an evidence source and confirms that the conversation remains", async () => {
        const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
        render(<AuthProvider><MemoryAssetsPage /></AuthProvider>);
        await screen.findByTestId("memory-assertion-row");

        fireEvent.click(screen.getByRole("button", { name: "Forget memory from conversation event 4" }));

        await waitFor(() => expect(fetchMock).toHaveBeenCalledWith(
            "/api/admin/memory-assertions/forget-source",
            expect.objectContaining({ method: "POST" }),
        ));
        const call = fetchMock.mock.calls.find(([url]) => url === "/api/admin/memory-assertions/forget-source");
        expect(JSON.parse((call?.[1] as RequestInit).body as string)).toEqual({
            conversation_id: "chat-language",
            event_seq: 4,
        });
        expect(await screen.findByRole("status")).toHaveTextContent("The source event remains in conversation history and backups.");
        expect(confirm).toHaveBeenCalled();
        confirm.mockRestore();
    });
});
