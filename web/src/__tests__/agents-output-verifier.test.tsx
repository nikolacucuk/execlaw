import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { Agents } from "../routes/Agents";

const mocks = vi.hoisted(() => ({
    createAgent: vi.fn(),
    listAgents: vi.fn(),
    listChildTasks: vi.fn(),
    listAgentReplyDrafts: vi.fn(),
    listAgentRuns: vi.fn(),
    listAgentScheduleFires: vi.fn(),
    previewAgent: vi.fn(),
    takeOverAgentConversation: vi.fn(),
    handBackAgentConversation: vi.fn(),
    getAgentOwnership: vi.fn(),
}));

vi.mock("../api/agents", () => ({
    createAgent: mocks.createAgent,
    listAgents: mocks.listAgents,
    listAgentReplyDrafts: mocks.listAgentReplyDrafts,
    listAgentRuns: mocks.listAgentRuns,
    listAgentScheduleFires: mocks.listAgentScheduleFires,
    previewAgent: mocks.previewAgent,
    takeOverAgentConversation: mocks.takeOverAgentConversation,
    handBackAgentConversation: mocks.handBackAgentConversation,
    getAgentOwnership: mocks.getAgentOwnership,
}));
vi.mock("../api/runs", () => ({ listChildTasks: mocks.listChildTasks }));
vi.mock("../auth/AuthContext", () => ({ useAuth: () => ({ getAccessToken: () => null }) }));
vi.mock("../chat/Sidebar", () => ({ Sidebar: () => null }));
vi.mock("../api/ws", () => ({ WsClient: class { open() {} close() {} } }));

beforeEach(() => {
    mocks.createAgent.mockReset().mockResolvedValue({});
    mocks.listAgents.mockReset().mockResolvedValue([]);
    mocks.listChildTasks.mockReset().mockResolvedValue([]);
    mocks.listAgentReplyDrafts.mockReset().mockResolvedValue([]);
    mocks.listAgentRuns.mockReset().mockResolvedValue([]);
    mocks.listAgentScheduleFires.mockReset().mockResolvedValue([]);
    mocks.previewAgent.mockReset().mockResolvedValue({ decisions: [], matched: 0, false_positives: 0, missed: 0, effect_count: 0, model_results: [], outcome_mismatches: 0, missing_drafts: 0, irrelevant_count: 0, max_latency_ms: 0 });
    mocks.takeOverAgentConversation.mockReset().mockResolvedValue({ owner_kind: "controller", generation: 2 });
    mocks.handBackAgentConversation.mockReset().mockResolvedValue(true);
    mocks.getAgentOwnership.mockReset().mockResolvedValue(null);
});

it("previews a selected agent without effects and exposes a stale draft", async () => {
    mocks.listAgents.mockResolvedValue([{ id: "camper", name: "camper", trigger: { channel: "whatsapp", keywords: ["camper"] },
        enabled: true, paused: false, reply_mode: "draft", definition_version: 2, schedule_next_at: null, last_run_status: null }]);
    mocks.listAgentReplyDrafts.mockResolvedValue([{ id: "draft-1", agent_id: "camper", conversation_id: "chat-1", channel: "whatsapp",
        recipient: "group@g.us", audience: { group_id: "group@g.us", membership_epoch: 3, members: ["principal-a", "principal-b"] }, revision: 1, inbound_text: "Camper?", draft_text: "Please send dates.", status: "pending", stale_at: 10,
        model_seq: 4, created_at: 9 }]);
    render(<Agents />);
    fireEvent.click(await screen.findByRole("button", { name: /camper.*Running/ }));
    fireEvent.change(screen.getByLabelText("Preview events JSON"), { target: { value: "[]" } });
    fireEvent.click(screen.getByRole("button", { name: "Preview selected agent" }));
    await waitFor(() => expect(mocks.previewAgent).toHaveBeenCalledWith("camper", expect.objectContaining({ channel: "whatsapp" }), [], [], {}, false, expect.any(Function)));
    expect(screen.getByText(/effects 0/)).toBeInTheDocument();
    expect(screen.getByText(/Stale · regenerate before sending/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save revision" })).toBeDisabled();
    expect(screen.getByLabelText("Audience for draft-1")).toHaveTextContent("principal-a");
    expect(screen.getByLabelText("Audience for draft-1")).toHaveTextContent("audience epoch 3");
});

it("submits a structured output verifier with the matching agent criterion", async () => {
    render(<Agents />);
    fireEvent.change(screen.getByPlaceholderText("Name"), { target: { value: "reporter" } });
    fireEvent.change(screen.getByPlaceholderText("Role prompt"), { target: { value: "Return JSON" } });
    fireEvent.change(screen.getByLabelText(/Acceptance criteria/), {
        target: { value: "format=Output has the required format" },
    });
    fireEvent.change(screen.getByLabelText(/Structured output checks/), {
        target: { value: 'format|/json/format|"ok"' },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create agent" }));
    await waitFor(() => expect(mocks.createAgent).toHaveBeenCalled());
    const [request] = mocks.createAgent.mock.calls[0];
    expect(request.completion_contract.acceptance_criteria[0]).toEqual({
        criterion_id: "format",
        description: "Output has the required format",
        required: true,
        verifier: { step_id: "agent:output", json_pointer: "/json/format", expected: "ok" },
    });
});
