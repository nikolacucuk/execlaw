// Client for the M5 /admin/inference observability surface.

import { apiFetch } from "./client";

export type InferenceConsumer =
    | "chat"
    | "routines"
    | "research"
    | "automations"
    | "other";

export interface ConsumerSnapshot {
    consumer: InferenceConsumer;
    in_flight: number;
    total_calls: number;
    total_failures: number;
    sample_count: number;
    p50_ms: number | null;
    p95_ms: number | null;
}

export interface MetricsSnapshot {
    consumers: ConsumerSnapshot[];
    phases: PhaseSnapshot[];
    contexts: ContextSnapshot[];
}

export interface QualificationCheck {
    passed: boolean;
    code: string;
    elapsed_ms: number | null;
    request_bytes: number | null;
    prompt_tokens: number | null;
}

export interface ModelQualificationResponse {
    identity: {
        model_id: string;
        quantization: string;
        chat_template: string;
        backend_version: string;
        parser_version: string;
    };
    qualified: boolean;
    context_tokens: number;
    qualified_at: number | null;
    checks: {
        model: string;
        protocol: string;
        text: QualificationCheck;
        streaming: QualificationCheck;
        tools: QualificationCheck;
        structured_json: QualificationCheck;
        context: QualificationCheck;
        vision: QualificationCheck;
        total_elapsed_ms: number;
    };
}

export interface StoredModelCapabilityProfile {
    identity: ModelQualificationResponse["identity"];
    context_tokens: number;
    observed: Record<string, unknown>;
    qualified_at: number;
    invalidated_at: number | null;
}

export type InferencePhase =
    | "prompt_assembly"
    | "prefill_decode"
    | "tool_wait"
    | "retry"
    | "stream_delay";

export interface PhaseSnapshot {
    consumer: InferenceConsumer;
    phase: InferencePhase;
    sample_count: number;
    p50_ms: number | null;
    p95_ms: number | null;
    baseline_p95_ms: number | null;
    regression_budget_percent: number;
    regression_detected: boolean | null;
}

export interface ContextSnapshot {
    consumer: InferenceConsumer;
    phase: InferencePhase;
    sample_count: number;
    p50_serialized_bytes: number | null;
    p95_serialized_bytes: number | null;
    p50_estimated_tokens: number | null;
    p95_estimated_tokens: number | null;
    baseline_p95_estimated_tokens: number | null;
    regression_budget_percent: number;
    regression_detected: boolean | null;
}

export async function getInferenceMetrics(
    tokenAccessor: () => string | null,
): Promise<MetricsSnapshot> {
    return apiFetch<MetricsSnapshot>(
        "/api/admin/inference/metrics",
        {},
        tokenAccessor,
    );
}

export function qualifyInferenceModel(
    contextTokens: number,
    tokenAccessor: () => string | null,
): Promise<ModelQualificationResponse> {
    return apiFetch<ModelQualificationResponse>(
        "/api/admin/inference/qualify",
        { method: "POST", body: { context_tokens: contextTokens } },
        tokenAccessor,
    );
}

export function getCurrentModelProfile(
    tokenAccessor: () => string | null,
): Promise<StoredModelCapabilityProfile | null> {
    return apiFetch<StoredModelCapabilityProfile | null>(
        "/api/admin/inference/capability-profile/current",
        {},
        tokenAccessor,
    );
}

export function consumerLabel(c: InferenceConsumer): string {
    switch (c) {
        case "chat":
            return "Chat";
        case "routines":
            return "Routines";
        case "research":
            return "Research";
        case "automations":
            return "Automations";
        case "other":
            return "Other";
    }
}
