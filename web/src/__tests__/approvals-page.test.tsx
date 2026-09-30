// Tests for the standalone /approvals page body.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ApprovalsPage } from "../approvals/ApprovalsPage";
import { AuthProvider } from "../auth/AuthContext";

let fetchMock: ReturnType<typeof vi.fn>;

const meResponse = () =>
    new Response(
        JSON.stringify({
            user_id: "ctrl-1",
            username: "ctrl",
            display_name: "Ctrl",
            email: null,
            role: "controller",
            last_login_at: null,
        }),
        { status: 200 },
    );

interface ApprovalFixture {
    approval_id: string;
    conversation_id: string;
    sender_principal_id: string;
    original_text: string;
    scope?: string;
    reason?: string;
    requested_action?: string;
}

function approvalsResponse(approvals: ApprovalFixture[]) {
    const withContext = approvals.map((approval) => ({
        scope: "Trust record for this principal",
        reason: "A new sender has no approved trust level.",
        requested_action: "Choose a trust decision and review the queued message.",
        ...approval,
    }));
    return new Response(JSON.stringify({ approvals: withContext, memory_promotions: [] }), { status: 200 });
}

function mountPage() {
    return render(
        <AuthProvider>
            <ApprovalsPage />
        </AuthProvider>,
    );
}

beforeEach(() => {
    localStorage.setItem("execlaw.access_token", "tok");
    localStorage.setItem("execlaw.refresh_token", "tok");
    fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
    vi.unstubAllGlobals();
});

describe("ApprovalsPage", () => {
    it("renders the empty state when there are no pending approvals", async () => {
        fetchMock.mockImplementation(async (url: string) => {
            if (url === "/api/admin/me") return meResponse();
            if (url === "/api/admin/approvals") return approvalsResponse([]);
            return new Response("{}", { status: 200 });
        });
        mountPage();
        await waitFor(() => {
            expect(screen.getByTestId("approvals-empty")).toBeInTheDocument();
        });
        expect(screen.getByTestId("approvals-empty").textContent).toContain(
            "No pending approvals",
        );
    });

    it("renders one card per pending approval with all five verb buttons", async () => {
        fetchMock.mockImplementation(async (url: string) => {
            if (url === "/api/admin/me") return meResponse();
            if (url === "/api/admin/approvals")
                return approvalsResponse([
                    {
                        approval_id: "appr-1",
                        conversation_id: "conv-1",
                        sender_principal_id: "pri_signal_+15551234567",
                        original_text: "Hey there",
                    },
                    {
                        approval_id: "appr-2",
                        conversation_id: "conv-2",
                        sender_principal_id: "pri_web_anon-9",
                        original_text: "different message",
                    },
                ]);
            return new Response("{}", { status: 200 });
        });
        mountPage();
        await waitFor(() => {
            expect(screen.getAllByTestId("approval-row")).toHaveLength(2);
        });
        // Each row carries the canonical 5 verbs.
        const verbs = [
            "trust",
            "trust_limited",
            "claim_as_me",
            "ignore_once",
            "block",
        ];
        for (const verb of verbs) {
            expect(
                screen.getAllByTestId(`approval-row-verb-${verb}`),
            ).toHaveLength(2);
        }
        expect(screen.getAllByTestId("approval-row-text")[0].textContent).toContain(
            "Hey there",
        );
        expect(screen.getAllByTestId("approval-context")[0].textContent).toContain("Trust record for this principal");
        expect(screen.getAllByTestId("approval-context")[0].textContent).toContain("no approved trust level");
        expect(screen.getAllByTestId("approval-requested-action")[0].textContent).toContain("queued message");
    });

    it("submits the operator-selected topic scope for limited trust", async () => {
        const calls: Array<{ url: string; init?: RequestInit }> = [];
        fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
            calls.push({ url, init });
            if (url === "/api/admin/me") return meResponse();
            if (url === "/api/admin/approvals") return approvalsResponse([{
                approval_id: "appr-scope",
                conversation_id: "conv-scope",
                sender_principal_id: "pri_signal_contact",
                original_text: "Can you help with gardening?",
            }]);
            return new Response("{}", { status: 200 });
        });
        mountPage();
        await waitFor(() => expect(screen.getByTestId("approval-limited-scope")).toBeInTheDocument());
        fireEvent.change(screen.getByTestId("approval-limited-scope"), {
            target: { value: "gardening, home repair" },
        });
        fireEvent.click(screen.getByTestId("approval-row-verb-trust_limited"));
        await waitFor(() => expect(calls.some((call) =>
            call.url === "/api/admin/approvals/appr-scope/respond" && call.init?.method === "POST",
        )).toBe(true));
        const response = calls.find((call) =>
            call.url === "/api/admin/approvals/appr-scope/respond" && call.init?.method === "POST",
        );
        expect(JSON.parse(response?.init?.body as string)).toEqual({
            verb: "trust_limited",
            allowed_topics: ["gardening", "home repair"],
        });
    });

    it("POSTs the chosen verb (snake_case) to the respond endpoint", async () => {
        const calls: Array<{ url: string; init?: RequestInit }> = [];
        fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
            calls.push({ url, init });
            if (url === "/api/admin/me") return meResponse();
            if (url === "/api/admin/approvals" && (!init || init.method !== "POST"))
                return approvalsResponse([
                    {
                        approval_id: "appr-1",
                        conversation_id: "conv-1",
                        sender_principal_id: "pri_signal_+15551234567",
                        original_text: "Hey there",
                    },
                ]);
            if (
                url === "/api/admin/approvals/appr-1/respond" &&
                init?.method === "POST"
            ) {
                return new Response(
                    JSON.stringify({
                        approval_id: "appr-1",
                        principal_id: "controller-x",
                        conversation_id: "conv-1",
                        new_trust_class: "Controller",
                        outcome: "claim_as_me",
                    }),
                    { status: 200 },
                );
            }
            return new Response("{}", { status: 200 });
        });
        mountPage();
        await waitFor(() => {
            expect(screen.getByTestId("approval-row")).toBeInTheDocument();
        });
        fireEvent.click(screen.getByTestId("approval-row-verb-claim_as_me"));
        await waitFor(() => {
            const respondCall = calls.find(
                (c) =>
                    c.url === "/api/admin/approvals/appr-1/respond" &&
                    c.init?.method === "POST",
            );
            expect(respondCall).toBeDefined();
        });
        const respondCall = calls.find(
            (c) =>
                c.url === "/api/admin/approvals/appr-1/respond" &&
                c.init?.method === "POST",
        )!;
        const body = JSON.parse((respondCall.init?.body as string) ?? "{}");
        // Wire value MUST be snake_case — the server's serde rejects
        // PascalCase. This is the contract the pre-fix code violated.
        expect(body.verb).toBe("claim_as_me");
    });

    it("renders memory proposals with only approve/reject and uses their dedicated endpoint", async () => {
        const calls: Array<{ url: string; init?: RequestInit }> = [];
        fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
            calls.push({ url, init });
            if (url === "/api/admin/me") return meResponse();
            if (url === "/api/admin/approvals") {
                return new Response(JSON.stringify({
                    approvals: [],
                    memory_promotions: [{
                        id: 42,
                        scope: "global",
                        trust_class: "Controller",
                        key: "preferred_language",
                        from_tier: "warm",
                        to_tier: "hot",
                        reason: "frequency",
                        proposed_by: "sweeper",
                        proposed_at: 100,
                        evidence_ref: "memory://global/Controller/preferred_language",
                    }],
                }), { status: 200 });
            }
            return new Response("{}", { status: 200 });
        });
        mountPage();
        await waitFor(() => expect(screen.getByTestId("memory-promotion-row")).toBeInTheDocument());
        expect(screen.getByTestId("memory-promotion-row").textContent).toContain("preferred_language");
        expect(screen.queryByTestId("approval-row-verb-trust")).not.toBeInTheDocument();
        fireEvent.click(screen.getByTestId("memory-promotion-approve"));
        await waitFor(() => {
            expect(calls.some((call) =>
                call.url === "/api/admin/memory-promotions/42/respond" &&
                call.init?.method === "POST",
            )).toBe(true);
        });
        const decision = calls.find((call) =>
            call.url === "/api/admin/memory-promotions/42/respond" &&
            call.init?.method === "POST",
        );
        expect(JSON.parse(decision?.init?.body as string)).toEqual({ decision: "approve" });
    });

    it("renders effectful chain approvals with only approve/reject and includes the signed token", async () => {
        const calls: Array<{ url: string; init?: RequestInit }> = [];
        fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
            calls.push({ url, init });
            if (url === "/api/admin/me") return meResponse();
            if (url === "/api/admin/approvals") return new Response(JSON.stringify({
                approvals: [{
                    kind: "effectful_chain",
                    approval_id: "chain-1",
                    conversation_id: "conv-1",
                    sender_principal_id: "tool-chain",
                    original_text: "Tool-chain execution awaiting approval: send report",
                    scope: "External effect set in saved plan plan-1",
                    reason: "External effects require Controller approval.",
                    requested_action: "[{\"label\":\"send report\",\"effect_kind\":\"email\",\"payload\":{}}]",
                    approval_token: "signed-effect-token",
                }],
                memory_promotions: [],
            }), { status: 200 });
            return new Response("{}", { status: 200 });
        });
        mountPage();
        await waitFor(() => expect(screen.getByTestId("approval-row")).toBeInTheDocument());
        expect(screen.getByTestId("approval-context").textContent).toContain("External effect set in saved plan plan-1");
        expect(screen.getByTestId("approval-requested-action").textContent).toContain("send report");
        expect(screen.getByTestId("approval-row-verb-approve")).toBeInTheDocument();
        expect(screen.queryByTestId("approval-row-verb-trust")).not.toBeInTheDocument();
        fireEvent.click(screen.getByTestId("approval-row-verb-approve"));
        await waitFor(() => expect(calls.some((call) =>
            call.url === "/api/admin/approvals/chain-1/respond" && call.init?.method === "POST",
        )).toBe(true));
        const response = calls.find((call) => call.url === "/api/admin/approvals/chain-1/respond" && call.init?.method === "POST");
        expect(JSON.parse(response?.init?.body as string)).toEqual({
            verb: "approve",
            approval_token: "signed-effect-token",
        });
    });
});
