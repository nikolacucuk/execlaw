import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { AuthProvider } from "../auth/AuthContext";
import { WorkspaceExecutionPage } from "../settings/WorkspaceExecutionPage";

let fetchMock: ReturnType<typeof vi.fn>;

function response(body: unknown) {
    return new Response(JSON.stringify(body), { status: 200 });
}

describe("WorkspaceExecutionPage", () => {
    beforeEach(() => {
        localStorage.setItem("execlaw.access_token", "tok");
        localStorage.setItem("execlaw.refresh_token", "tok");
        fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
            if (url === "/api/admin/me") {
                return response({
                    user_id: "ctrl-1",
                    username: "ctrl",
                    display_name: "Controller",
                    email: null,
                    role: "controller",
                    last_login_at: null,
                });
            }
            if (url === "/api/admin/workspace-execution") {
                if (init?.method === "PUT") {
                    return response({
                        image_reference: "sha256:" + "a".repeat(64),
                        language_servers: { rust: ["rust-analyzer"] },
                        approval: "controller_digest_approval",
                        updated_at: 1,
                        updated_by: "ctrl",
                    });
                }
                return response({
                    image_reference: null,
                    language_servers: {},
                    updated_at: 0,
                    updated_by: "migration",
                });
            }
            return response({});
        });
        vi.stubGlobal("fetch", fetchMock);
    });

    afterEach(() => vi.unstubAllGlobals());

    it("saves a local image ID and language-server argv map with explicit Controller approval", async () => {
        render(
            <AuthProvider>
                <WorkspaceExecutionPage />
            </AuthProvider>,
        );
        await screen.findByRole("heading", { name: "Workspace execution" });

        fireEvent.change(screen.getByLabelText("Toolchain image reference"), {
            target: { value: "sha256:" + "a".repeat(64) },
        });
        fireEvent.change(screen.getByLabelText("Language-server argv map"), {
            target: { value: '{"rust":["rust-analyzer"]}' },
        });
        fireEvent.click(
            screen.getByLabelText(
                "Approve this exact image digest on this installation without remote attestation",
            ),
        );
        fireEvent.click(screen.getByRole("button", { name: "Save toolchain" }));

        await waitFor(() =>
            expect(fetchMock).toHaveBeenCalledWith(
                "/api/admin/workspace-execution",
                expect.objectContaining({ method: "PUT" }),
            ),
        );
        const call = fetchMock.mock.calls.find(
            ([url, init]) =>
                url === "/api/admin/workspace-execution" && init?.method === "PUT",
        );
        expect(JSON.parse((call?.[1] as RequestInit).body as string)).toEqual({
            image_reference: "sha256:" + "a".repeat(64),
            language_servers: { rust: ["rust-analyzer"] },
            approve_image_digest: true,
        });
        expect(await screen.findByRole("status")).toHaveTextContent(
            "Controller-approved",
        );
    });
});
