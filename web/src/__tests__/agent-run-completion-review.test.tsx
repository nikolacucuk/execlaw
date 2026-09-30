import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { AgentRunCompletionReview } from "../routes/AgentRunCompletionReview";

const { mockRecordAgentCriterion, mockRecordAgentArtifact, mockConfirmAgentDelivery } = vi.hoisted(() => ({
    mockRecordAgentCriterion: vi.fn(),
    mockRecordAgentArtifact: vi.fn(),
    mockConfirmAgentDelivery: vi.fn(),
}));

vi.mock("../api/agents", () => ({
    recordAgentCriterion: (...args: unknown[]) => mockRecordAgentCriterion(...args),
    recordAgentArtifact: (...args: unknown[]) => mockRecordAgentArtifact(...args),
    confirmAgentDelivery: (...args: unknown[]) => mockConfirmAgentDelivery(...args),
}));

const report = {
    contract: {
        acceptance_criteria: [{ criterion_id: "format", description: "Uses the required format", required: true }],
        required_artifacts: [{ artifact_id: "report", description: "A reviewable report" }],
        delivery_required: true,
    },
    verifications: [{ criterion_id: "format", status: "pending" as const, evidence_refs: [], detail: null }],
    artifacts: [{ artifact_id: "report", present: false, evidence_ref: null, detail: null }],
    delivery_confirmed: false,
    delivery_evidence_ref: null,
    status: "incomplete" as const,
    unfinished: ["Required criterion 'Uses the required format' is not verified"],
};

describe("AgentRunCompletionReview", () => {
    beforeEach(() => {
        mockRecordAgentCriterion.mockReset().mockResolvedValue(report);
        mockRecordAgentArtifact.mockReset().mockResolvedValue(report);
        mockConfirmAgentDelivery.mockReset().mockResolvedValue(report);
    });

    it("shows incomplete requirements, links trace evidence, and records a verifier result", async () => {
        const onUpdated = vi.fn();
        render(<AgentRunCompletionReview agentId="agent-a" runId="run-a" report={report} token={() => "token"} onUpdated={onUpdated} />);

        expect(screen.getByText("incomplete")).toBeInTheDocument();
        expect(screen.getByText("A reviewable report")).toBeInTheDocument();
        expect(screen.getByText(/not verified/)).toBeInTheDocument();

        fireEvent.change(screen.getByLabelText("Evidence for Uses the required format"), {
            target: { value: "trace:12" },
        });
        fireEvent.click(screen.getByRole("button", { name: "Record" }));
        await waitFor(() => expect(mockRecordAgentCriterion).toHaveBeenCalledWith(
            "agent-a",
            "run-a",
            "format",
            { status: "passed", evidence_refs: ["trace:12"], detail: null },
            expect.any(Function),
        ));
        expect(onUpdated).toHaveBeenCalledOnce();
    });
});
