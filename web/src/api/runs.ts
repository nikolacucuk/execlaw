import { apiFetch } from "./client";

export interface RunSummary {
    run_id: string;
    conversation_id: string;
    parent_run_id: string | null;
    status: string;
    cursor: number;
    input_event_seq: number;
    started_at: number;
    updated_at: number;
    deadline_at: number | null;
}

export interface RunStepTrace {
    step_id: string;
    ordinal: number;
    kind: string;
    status: string;
    attempt: number;
    input_hash: string;
    approval_id: string | null;
    started_at: number | null;
    completed_at: number | null;
}

export interface RunTraceEvent {
    cursor: number;
    run_id: string;
    event_kind: string;
    subject_id: string | null;
    status: string;
    metadata: Record<string, unknown>;
    created_at: number;
}

export interface ChildTaskTrace {
    child_run_id: string;
    parent_run_id: string;
    task: { version?: number; task?: string; context?: string; result?: unknown };
    task_hash: string;
    trust_ceiling: Record<string, unknown>;
    budget_tokens: number;
    tokens_used: number | null;
    dependencies: string[];
    result_artifact_id: string | null;
    status: string;
    created_at: number;
    updated_at: number;
}

export interface WorkspaceRoot {
    workspace_id: string;
    canonical_path: string;
    created_at: number;
    created_by: string;
}

export interface WorkspaceCheckpoint {
    checkpoint_id: string;
    workspace_id: string;
    run_id: string;
    parent_checkpoint_id: string | null;
    file_count: number;
    total_bytes: number;
    created_at: number;
}

export interface WorkspaceDiffPreview {
    run_id: string;
    workspace_id: string;
    checkpoint_id: string;
    preview_hash: string;
    conflicts: number;
    changes: Array<{
        path: string;
        base_sha256: string | null;
        current_sha256: string | null;
        proposed_sha256: string | null;
        conflict: boolean;
    }>;
}

export interface WorkspaceApplyReceipt {
    apply_id: string;
    run_id: string;
    workspace_id: string;
    checkpoint_id: string;
    preview_hash: string;
    status: string;
    created_at: number;
    updated_at: number;
}

export interface WorkspaceApplyState {
    latest_apply: WorkspaceApplyReceipt | null;
    latest_restore: WorkspaceApplyReceipt | null;
}

export function listWorkspaceRoots(token: () => string | null): Promise<WorkspaceRoot[]> {
    return apiFetch("/api/admin/workspaces", {}, token);
}

export function registerWorkspaceRoot(path: string, token: () => string | null): Promise<WorkspaceRoot> {
    return apiFetch("/api/admin/workspaces", { method: "POST", body: { path } }, token);
}

export function createWorkspaceCheckpoint(runId: string, workspaceId: string, token: () => string | null): Promise<WorkspaceCheckpoint> {
    return apiFetch(`/api/admin/runs/${encodeURIComponent(runId)}/workspace-checkpoints`, { method: "POST", body: { workspace_id: workspaceId } }, token);
}

export function previewWorkspaceDiff(runId: string, token: () => string | null): Promise<WorkspaceDiffPreview> {
    return apiFetch(`/api/admin/runs/${encodeURIComponent(runId)}/workspace-diff`, {}, token);
}

export function applyWorkspaceDiff(runId: string, preview_hash: string, requestId: string, token: () => string | null): Promise<WorkspaceApplyReceipt> {
    return apiFetch<WorkspaceApplyReceipt>(`/api/admin/runs/${encodeURIComponent(runId)}/workspace-apply`, {
        method: "POST",
        body: { preview_hash },
        headers: { "Idempotency-Key": requestId },
    }, token);
}

export function getWorkspaceApplyState(runId: string, token: () => string | null): Promise<WorkspaceApplyState> {
    return apiFetch(`/api/admin/runs/${encodeURIComponent(runId)}/workspace-apply`, {}, token);
}

export function restoreWorkspaceDiff(runId: string, apply_id: string, requestId: string, token: () => string | null): Promise<WorkspaceApplyReceipt> {
    return apiFetch(`/api/admin/runs/${encodeURIComponent(runId)}/workspace-restore`, {
        method: "POST", body: { apply_id }, headers: { "Idempotency-Key": requestId },
    }, token);
}

export interface RunTracePage {
    run: RunSummary;
    asset_loadout?: {
        agent_scope: string;
        conversation_trust_class: string;
        readable_trust_classes: string[];
        readable_owner_scopes: string[];
        resolved_at: number;
        retrieval_query_sha256: string | null;
        assets: Array<{
            asset_id: string;
            name: string;
            asset_type: string;
            version: number;
            source_hash: string | null;
            owner_scope: string;
            visibility: string;
            trust_floor: string;
            status: string;
            expires_at: number | null;
            binding_agent_scope: string;
            binding_mode: string;
            binding_priority: number;
            binding_max_chars: number;
            injected_chars: number;
            admission_reasons: string[];
        }>;
        retrieved_assets: Array<{
            asset_id: string;
            name: string;
            asset_type: string;
            version: number;
            source_hash: string | null;
            owner_scope: string;
            visibility: string;
            trust_floor: string;
            expires_at: number | null;
            score_micros: number;
            lexical_rank: number;
            vector_rank: number | null;
            injected_chars: number;
            admission_reasons: string[];
        }>;
    } | null;
    recovery_action?: {
        action: string;
        step?: { step_id: string; kind: string; status: string; lease_expires_at: number | null };
        cursor?: number;
    } | null;
    steps: RunStepTrace[];
    children: RunSummary[];
    child_tasks: ChildTaskTrace[];
    retries: Array<{
        step_id: string;
        tool_name: string;
        attempts_used: number;
        retry_budget_total: number;
        next_retry_at_ms: number | null;
        status: string;
        input_schema_hash: string | null;
        result_schema_hash: string | null;
    }>;
    delivery: Array<{
        event_seq: number;
        transition: string;
        occurred_at: number;
        attempt: number;
        external_receipt: string | null;
        actor?: string | null;
    }>;
    events: RunTraceEvent[];
    next_cursor: number;
    cursor_reset: boolean;
}

export interface AcceptanceCriterion {
    criterion_id: string;
    description: string;
    required: boolean;
    verifier?: {
        step_id: string;
        json_pointer: string;
        expected: unknown;
    };
}

export interface RequiredRunArtifact {
    artifact_id: string;
    description: string;
}

export interface CriterionVerification {
    criterion_id: string;
    status: "pending" | "passed" | "failed" | "blocked";
    evidence_refs: string[];
    detail: string | null;
    verified_at: number;
}

export interface ArtifactVerification {
    artifact_id: string;
    present: boolean;
    evidence_ref: string | null;
    detail: string | null;
    checked_at: number;
}

export interface RunCompletionReport {
    contract: {
        run_id: string;
        acceptance_criteria: AcceptanceCriterion[];
        required_artifacts: RequiredRunArtifact[];
        delivery_required: boolean;
        created_at: number;
    };
    verifications: CriterionVerification[];
    artifacts: ArtifactVerification[];
    delivery_confirmed: boolean;
    delivery_evidence_ref: string | null;
    status: "incomplete" | "partial" | "blocked" | "verified_complete";
    unfinished: string[];
}

export interface UnknownOutboxEffect {
    id: number;
    conversation_id: string;
    effect_kind: string;
    attempts: number;
    enqueued_seq: number;
}

export interface DeadLetterJobView {
    job_kind: string;
    job_id: string;
    attempts: number;
    max_attempts: number | null;
    affected_resource: string;
    cause: string;
    reconciliation_status: string;
    redrive_mode: string | null;
}

export type UnknownOutboxResolution = "authorize_retry" | "confirm_delivered";

export function listRuns(token: () => string | null, before?: Pick<RunSummary, "started_at" | "run_id">): Promise<RunSummary[]> {
    const params = new URLSearchParams({ limit: "50" });
    if (before !== undefined) {
        params.set("before_started_at", String(before.started_at));
        params.set("before_run_id", before.run_id);
    }
    return apiFetch(`/api/admin/runs?${params}`, {}, token);
}

export function listChildTasks(token: () => string | null): Promise<ChildTaskTrace[]> {
    return apiFetch("/api/admin/runs/child-tasks?limit=50", {}, token);
}

export function getRunTrace(
    runId: string,
    after: number,
    token: () => string | null,
): Promise<RunTracePage> {
    const params = new URLSearchParams({ after: String(after), limit: "200" });
    return apiFetch(`/api/admin/runs/${encodeURIComponent(runId)}/trace?${params}`, {}, token);
}

export function forkRun(runId: string, token: () => string | null): Promise<RunSummary> {
    return apiFetch(`/api/admin/runs/${encodeURIComponent(runId)}/fork`, { method: "POST" }, token);
}

export function getRunCompletion(runId: string, token: () => string | null): Promise<RunCompletionReport> {
    return apiFetch(`/api/admin/runs/${encodeURIComponent(runId)}/completion`, {}, token);
}

export function setRunCompletionContract(
    runId: string,
    contract: Pick<RunCompletionReport["contract"], "acceptance_criteria" | "required_artifacts" | "delivery_required">,
    token: () => string | null,
): Promise<RunCompletionReport> {
    return apiFetch(`/api/admin/runs/${encodeURIComponent(runId)}/completion-contract`, { method: "PUT", body: contract }, token);
}

export function recordRunCriterion(
    runId: string,
    criterion: Pick<CriterionVerification, "criterion_id" | "status" | "evidence_refs" | "detail">,
    token: () => string | null,
): Promise<RunCompletionReport> {
    return apiFetch(`/api/admin/runs/${encodeURIComponent(runId)}/completion-verifications`, { method: "POST", body: criterion }, token);
}

export function recordRunArtifact(
    runId: string,
    artifact: Pick<ArtifactVerification, "artifact_id" | "present" | "evidence_ref" | "detail">,
    token: () => string | null,
): Promise<RunCompletionReport> {
    return apiFetch(`/api/admin/runs/${encodeURIComponent(runId)}/completion-artifacts`, { method: "POST", body: artifact }, token);
}

export function confirmRunDelivery(runId: string, evidence_ref: string, token: () => string | null): Promise<RunCompletionReport> {
    return apiFetch(`/api/admin/runs/${encodeURIComponent(runId)}/delivery-confirmation`, { method: "POST", body: { evidence_ref } }, token);
}

export function listUnknownOutboxEffects(token: () => string | null): Promise<UnknownOutboxEffect[]> {
    return apiFetch("/api/admin/outbox/unknown", {}, token);
}

export function listDeadLetters(token: () => string | null): Promise<DeadLetterJobView[]> {
    return apiFetch("/api/admin/dead-letters", {}, token);
}

export function redriveDeadLetter(
    jobKind: string,
    jobId: string,
    reason: string,
    token: () => string | null,
): Promise<{ job_kind: string; job_id: string; status: string }> {
    return apiFetch(`/api/admin/dead-letters/${encodeURIComponent(jobKind)}/${encodeURIComponent(jobId)}/redrive`, {
        method: "POST",
        body: { reason },
    }, token);
}

export function resolveUnknownOutboxEffect(
    outboxId: number,
    resolution: UnknownOutboxResolution,
    evidence_ref: string,
    token: () => string | null,
): Promise<{ outbox_id: number; status: "pending" | "delivered" }> {
    return apiFetch(`/api/admin/outbox/${outboxId}/resolution`, {
        method: "POST",
        body: { resolution, evidence_ref },
    }, token);
}
