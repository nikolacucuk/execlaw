import { apiFetch } from "./client";

export interface AgentCriterion {
    criterion_id: string;
    description: string;
    required: boolean;
    verifier?: { step_id: string; json_pointer: string; expected: unknown };
}

export interface AgentRequiredArtifact {
    artifact_id: string;
    description: string;
}

export interface AgentCompletionReport {
    contract: {
        acceptance_criteria: AgentCriterion[];
        required_artifacts: AgentRequiredArtifact[];
        delivery_required: boolean;
    };
    verifications: Array<{
        criterion_id: string;
        status: "pending" | "passed" | "failed" | "blocked";
        evidence_refs: string[];
        detail: string | null;
    }>;
    artifacts: Array<{
        artifact_id: string;
        present: boolean;
        evidence_ref: string | null;
        detail: string | null;
    }>;
    delivery_confirmed: boolean;
    delivery_evidence_ref: string | null;
    status: "incomplete" | "partial" | "blocked" | "verified_complete";
    unfinished: string[];
}

export interface AgentView {
    id: string;
    name: string;
    role_prompt: string;
    model: string | null;
    backend_purpose: string;
    tools: string[];
    trust_policy: Record<string, unknown>;
    trigger: Record<string, unknown>;
    reply_mode: "draft" | "automatic";
    definition_version: number;
    schedule_next_at: number | null;
    interval_secs: number;
    token_budget: number;
    max_runtime_secs: number;
    concurrency_limit: number;
    enabled: boolean;
    paused: boolean;
    next_run_at: number | null;
    last_run_at: number | null;
    last_run_status: string | null;
    last_error: string | null;
}

export interface AgentRun {
    id: string;
    agent_id: string;
    status: string;
    started_at: number;
    finished_at: number | null;
    tokens_used: number | null;
    checkpoint: Record<string, unknown>;
    output_text: string | null;
    error: string | null;
    completion: AgentCompletionReport | null;
    mailbox_id?: string | null;
    definition_version?: number | null;
    outcome_kind?: string | null;
}

export interface AgentScheduleFire {
    agent_id: string;
    due_at: number;
    status: "queued" | "skipped";
    reason: string | null;
    mailbox_id: string | null;
    recorded_at: number;
}

export interface AgentReplyDraft {
    id: string;
    agent_id: string;
    conversation_id: string;
    channel: string;
    recipient: string;
    audience: Record<string, unknown>;
    revision: number;
    inbound_text: string;
    draft_text: string;
    status: string;
    stale_at: number | null;
    model_seq: number | null;
    created_at: number;
}

export interface AgentPreviewEvent {
    source: string;
    id: string;
    channel: string;
    recipient: string;
    group_id: string | null;
    group_name: string | null;
    text: string;
    occurred_at: number;
}

export interface AgentPreviewResult {
    decisions: Array<{ source: string; event_id: string; matched: boolean; reason: string }>;
    matched: number;
    false_positives: number;
    missed: number;
    effect_count: number;
    model_results: Array<{ event_id: string; outcome_kind: string | null; error: string | null; latency_ms: number }>;
    outcome_mismatches: number;
    missing_drafts: number;
    irrelevant_count: number;
    max_latency_ms: number;
}

export interface AgentOwnership {
    conversation_id: string;
    owner_kind: "controller" | "agent";
    agent_id: string | null;
    previous_agent_id?: string | null;
    generation: number;
    updated_at: number;
}

export interface TransportIdentityLink {
    link_id: string;
    controller_id: string;
    left_channel: string;
    left_subject: string;
    right_channel: string;
    right_subject: string;
    verification_id: string;
    linked_at: number;
    revoked_at: number | null;
}

export interface AgentRequest {
    id?: string;
    name: string;
    role_prompt: string;
    model?: string | null;
    backend_purpose?: string;
    tools?: string[];
    trust_policy?: Record<string, unknown>;
    trigger?: Record<string, unknown>;
    reply_mode?: "draft" | "automatic";
    interval_secs?: number;
    token_budget?: number;
    max_runtime_secs?: number;
    concurrency_limit?: number;
    enabled?: boolean;
    completion_contract?: {
        acceptance_criteria: AgentCriterion[];
        required_artifacts: AgentRequiredArtifact[];
        delivery_required: boolean;
    };
}

export function listAgents(token: () => string | null) {
    return apiFetch<AgentView[]>("/api/admin/agents", {}, token);
}
export function createAgent(body: AgentRequest, token: () => string | null) {
    return apiFetch<AgentView>("/api/admin/agents", { method: "POST", body }, token);
}
export function updateAgent(id: string, body: AgentRequest, token: () => string | null) {
    return apiFetch<AgentView>(`/api/admin/agents/${encodeURIComponent(id)}`, { method: "PUT", body }, token);
}
export function importAgentMarkdown(markdown: string, token: () => string | null) {
    return apiFetch<AgentView>("/api/admin/agents/import-markdown", { method: "POST", body: { markdown } }, token);
}
export function pauseAgent(id: string, token: () => string | null) {
    return apiFetch<boolean>(`/api/admin/agents/${encodeURIComponent(id)}/pause`, { method: "POST" }, token);
}
export function resumeAgent(id: string, token: () => string | null) {
    return apiFetch<boolean>(`/api/admin/agents/${encodeURIComponent(id)}/resume`, { method: "POST" }, token);
}
export function sendAgentMessage(id: string, content: string, token: () => string | null) {
    return apiFetch<string>(`/api/admin/agents/${encodeURIComponent(id)}/messages`, { method: "POST", body: { content } }, token);
}
export function listAgentRuns(id: string, token: () => string | null) {
    return apiFetch<AgentRun[]>(`/api/admin/agents/${encodeURIComponent(id)}/runs`, {}, token);
}

export function listAgentScheduleFires(id: string, token: () => string | null) {
    return apiFetch<AgentScheduleFire[]>(`/api/admin/agents/${encodeURIComponent(id)}/schedule-fires`, {}, token);
}

export function previewAgent(agent_id: string, trigger: Record<string, unknown>, events: AgentPreviewEvent[], expected_event_ids: string[], expected_outcomes: Record<string, string>, run_model: boolean, token: () => string | null) {
    return apiFetch<AgentPreviewResult>("/api/admin/agents/preview", { method: "POST", body: { agent_id, trigger, events, expected_event_ids, expected_outcomes, run_model } }, token);
}

export function listAgentReplyDrafts(token: () => string | null) {
    return apiFetch<AgentReplyDraft[]>("/api/admin/agents/reply-drafts", {}, token);
}

export function editAgentReplyDraft(id: string, revision: number, text: string, token: () => string | null) {
    return apiFetch<AgentReplyDraft>(`/api/admin/agents/reply-drafts/${encodeURIComponent(id)}`, { method: "PUT", body: { revision, text } }, token);
}

export function rejectAgentReplyDraft(id: string, revision: number, token: () => string | null) {
    return apiFetch<boolean>(`/api/admin/agents/reply-drafts/${encodeURIComponent(id)}/reject`, { method: "POST", body: { revision } }, token);
}

export function getAgentOwnership(conversation_id: string, channel: string, recipient: string, token: () => string | null) {
    const query = new URLSearchParams({ conversation_id, channel, recipient });
    return apiFetch<AgentOwnership | null>(`/api/admin/agents/ownership?${query}`, {}, token);
}

export function takeOverAgentConversation(conversation_id: string, channel: string, recipient: string, token: () => string | null) {
    return apiFetch<AgentOwnership>("/api/admin/agents/ownership/takeover", { method: "POST", body: { conversation_id, channel, recipient } }, token);
}

export function handBackAgentConversation(conversation_id: string, channel: string, recipient: string, generation: number, token: () => string | null) {
    return apiFetch<boolean>("/api/admin/agents/ownership/handback", { method: "POST", body: { conversation_id, channel, recipient, generation } }, token);
}
export function linkTransportIdentity(body: Omit<TransportIdentityLink, "link_id" | "controller_id" | "linked_at" | "revoked_at">, token: () => string | null) {
    return apiFetch<TransportIdentityLink>("/api/admin/transport-identities/link", { method: "POST", body }, token);
}
export function unlinkTransportIdentity(id: string, token: () => string | null) {
    return apiFetch<boolean>(`/api/admin/transport-identities/${encodeURIComponent(id)}/unlink`, { method: "POST" }, token);
}
export function transferTransportContext(id: string, body: {
    origin_channel: string; origin_subject: string; origin_conversation: string;
    destination_channel: string; destination_subject: string; destination_conversation: string;
    selected_message_ids: string[]; audience_kind: "direct" | "group";
}, token: () => string | null) {
    return apiFetch<string>(`/api/admin/transport-identities/${encodeURIComponent(id)}/transfer`, { method: "POST", body }, token);
}
export function recordAgentCriterion(agentId: string, runId: string, criterionId: string, body: { status: "passed" | "failed" | "blocked"; evidence_refs: string[]; detail: string | null }, token: () => string | null) {
    return apiFetch<AgentCompletionReport>(`/api/admin/agents/${encodeURIComponent(agentId)}/runs/${encodeURIComponent(runId)}/completion/criteria/${encodeURIComponent(criterionId)}`, { method: "PUT", body }, token);
}
export function recordAgentArtifact(agentId: string, runId: string, artifactId: string, body: { present: boolean; evidence_ref: string | null; detail: string | null }, token: () => string | null) {
    return apiFetch<AgentCompletionReport>(`/api/admin/agents/${encodeURIComponent(agentId)}/runs/${encodeURIComponent(runId)}/completion/artifacts/${encodeURIComponent(artifactId)}`, { method: "PUT", body }, token);
}
export function confirmAgentDelivery(agentId: string, runId: string, evidence_ref: string, token: () => string | null) {
    return apiFetch<AgentCompletionReport>(`/api/admin/agents/${encodeURIComponent(agentId)}/runs/${encodeURIComponent(runId)}/completion/delivery`, { method: "POST", body: { evidence_ref } }, token);
}
