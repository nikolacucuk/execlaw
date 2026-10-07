import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AuthContext } from "../auth/AuthContext";
import { RunInspectorPage } from "../settings/RunInspectorPage";

const mocks = vi.hoisted(() => ({
    listRuns: vi.fn(),
    listUnknownOutboxEffects: vi.fn(),
    listDeadLetters: vi.fn(),
    listWorkspaceRoots: vi.fn(),
    redriveDeadLetter: vi.fn(),
}));

vi.mock("../api/runs", async (importOriginal) => ({
    ...await importOriginal<typeof import("../api/runs")>(),
    listRuns: mocks.listRuns,
    listUnknownOutboxEffects: mocks.listUnknownOutboxEffects,
    listDeadLetters: mocks.listDeadLetters,
    listWorkspaceRoots: mocks.listWorkspaceRoots,
    redriveDeadLetter: mocks.redriveDeadLetter,
}));

describe("run inspector dead-letter review", () => {
    beforeEach(() => {
        vi.clearAllMocks();
        mocks.listRuns.mockResolvedValue([]);
        mocks.listUnknownOutboxEffects.mockResolvedValue([]);
        mocks.listWorkspaceRoots.mockResolvedValue([]);
        mocks.listDeadLetters.mockResolvedValue([
            {
                job_kind: "memory_extraction",
                job_id: "memory-job-1",
                attempts: 5,
                max_attempts: 5,
                affected_resource: "conversation c-1 events 4-6",
                cause: "timeout",
                reconciliation_status: "local_idempotent_resume_available",
                redrive_mode: "same_job_and_source_identity",
            },
            {
                job_kind: "automation_event",
                job_id: "event-1",
                attempts: 5,
                max_attempts: null,
                affected_resource: "automation event webhook.received from fixture",
                cause: "operation_failed",
                reconciliation_status: "manual_handler_review_required",
                redrive_mode: null,
            },
        ]);
        mocks.redriveDeadLetter.mockResolvedValue({ status: "pending" });
    });

    it("shows sanitized failures and requires a reason for same-identity redrive", async () => {
        render(
            <AuthContext.Provider value={{ status: "authenticated", getAccessToken: () => "token" } as never}>
                <RunInspectorPage />
            </AuthContext.Provider>,
        );

        expect(await screen.findByTestId("dead-letter-jobs")).toBeTruthy();
        expect(screen.getByText("conversation c-1 events 4-6")).toBeTruthy();
        expect(screen.getByText(/Automatic redrive is disabled/)).toBeTruthy();
        const reason = screen.getByLabelText("Redrive reason for memory_extraction memory-job-1");
        const button = screen.getByRole("button", { name: "Redrive same identity" });
        expect(button).toBeDisabled();
        fireEvent.change(reason, { target: { value: "local model recovered" } });
        fireEvent.click(button);
        await waitFor(() => expect(mocks.redriveDeadLetter).toHaveBeenCalledWith(
            "memory_extraction",
            "memory-job-1",
            "local model recovered",
            expect.any(Function),
        ));
    });
});
