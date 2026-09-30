// Settings → Inference observability page (M5).
//
// Per-consumer slice of LLM call load. Six columns:
//
//   * Consumer (chat / routines / research / automations / other)
//   * In flight (current outstanding calls)
//   * Total calls (lifetime of the server process)
//   * Failures (subset of total_calls that returned Err)
//   * p50, p95 (over the last 256 calls per consumer)
//
// Polls every 5s by default; the operator can pause via the toggle
// (handy when staring at the page to debug a regression — no flashing
// numbers).

import { useCallback, useEffect, useRef, useState } from "react";
import Button from "react-bootstrap/Button";
import { useAuth } from "../auth/AuthContext";
import { ErrorBanner } from "../components/ErrorBanner";
import {
    consumerLabel,
    getCurrentModelProfile,
    getInferenceMetrics,
    qualifyInferenceModel,
    type ModelQualificationResponse,
    type StoredModelCapabilityProfile,
    type MetricsSnapshot,
} from "../api/inference";

const REFRESH_MS = 5_000;

export function InferencePage() {
    const auth = useAuth();
    const token = auth.getAccessToken;
    const [snap, setSnap] = useState<MetricsSnapshot | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [autoRefresh, setAutoRefresh] = useState<boolean>(true);
    const [qualification, setQualification] = useState<ModelQualificationResponse | null>(null);
    const [qualifying, setQualifying] = useState(false);
    const [qualificationTokens, setQualificationTokens] = useState(32_768);
    const [storedProfile, setStoredProfile] = useState<StoredModelCapabilityProfile | null>(null);
    const timerRef = useRef<number | null>(null);

    const fetchNow = useCallback(async () => {
        setError(null);
        try {
            const s = await getInferenceMetrics(token);
            setSnap(s);
        } catch (e) {
            setError((e as Error).message || "Failed to load inference metrics");
        }
    }, [token]);

    const qualify = useCallback(async () => {
        setQualifying(true);
        setError(null);
        try {
            const result = await qualifyInferenceModel(qualificationTokens, token);
            setQualification(result);
            setStoredProfile(await getCurrentModelProfile(token));
        } catch (e) {
            setError((e as Error).message || "Model qualification failed");
        } finally {
            setQualifying(false);
        }
    }, [token, qualificationTokens]);

    useEffect(() => {
        void fetchNow();
    }, [fetchNow]);

    useEffect(() => {
        let live = true;
        void getCurrentModelProfile(token)
            .then((profile) => { if (live) setStoredProfile(profile); })
            .catch(() => {});
        return () => { live = false; };
    }, [token]);

    useEffect(() => {
        if (!autoRefresh) {
            if (timerRef.current !== null) {
                window.clearInterval(timerRef.current);
                timerRef.current = null;
            }
            return;
        }
        timerRef.current = window.setInterval(() => {
            void fetchNow();
        }, REFRESH_MS);
        return () => {
            if (timerRef.current !== null) {
                window.clearInterval(timerRef.current);
                timerRef.current = null;
            }
        };
    }, [autoRefresh, fetchNow]);

    return (
        <div data-testid="inference-page">
            <ErrorBanner message={error} onDismiss={() => setError(null)} />
            <div className="d-flex justify-content-between align-items-center mb-3">
                <p className="text-muted small mb-0">
                    Per-consumer LLM call load. The same inference backend
                    serves chat, routines, research, and automations — this
                    table tells you who's driving the load.
                </p>
                <div className="d-flex gap-2 align-items-center">
                    <div className="form-check form-switch mb-0">
                        <input
                            className="form-check-input"
                            type="checkbox"
                            role="switch"
                            id="inference-autorefresh"
                            checked={autoRefresh}
                            onChange={(e) => setAutoRefresh(e.target.checked)}
                            data-testid="inference-autorefresh-switch"
                        />
                        <label
                            className="form-check-label small"
                            htmlFor="inference-autorefresh"
                        >
                            Auto-refresh ({Math.round(REFRESH_MS / 1000)}s)
                        </label>
                    </div>
                    <Button
                        variant="outline-secondary"
                        size="sm"
                        onClick={() => void fetchNow()}
                        data-testid="inference-refresh-btn"
                    >
                        <i className="bi bi-arrow-clockwise me-1" aria-hidden />
                        Refresh
                    </Button>
                    <Button variant="outline-primary" size="sm" onClick={() => void qualify()} disabled={qualifying}>
                        {qualifying ? "Qualifying local model…" : `Qualify model (${Math.round(qualificationTokens / 1024)}K)`}
                    </Button>
                    <input className="form-control form-control-sm" aria-label="Qualification context tokens" type="number" min={4096} max={262144} step={4096} value={qualificationTokens} onChange={(event) => setQualificationTokens(Number(event.target.value))} style={{ width: 130 }} />
                </div>
            </div>

            {qualification && <div className={`alert ${qualification.qualified ? "alert-success" : "alert-warning"}`} data-testid="model-qualification-result">
                <div className="fw-semibold">{qualification.qualified ? "Profile qualified" : "Profile not qualified"}</div>
                <div className="small">{qualification.identity.model_id} · {qualification.identity.quantization} · {qualification.identity.chat_template} · {qualification.identity.backend_version} · parser {qualification.identity.parser_version}</div>
                <div className="small">Context limit: {qualification.context_tokens.toLocaleString()} tokens. Vision remains unavailable until a vision fixture is qualified. Matrix time: {qualification.checks.total_elapsed_ms} ms.</div>
                <div className="small">Context calibration: {qualification.checks.context.request_bytes?.toLocaleString() ?? "—"} serialized bytes / {qualification.checks.context.prompt_tokens?.toLocaleString() ?? "—"} backend prompt tokens.</div>
                <div className="d-flex flex-wrap gap-3 mt-2 small">{(["text", "streaming", "tools", "structured_json", "context", "vision"] as const).map((key) => { const check = qualification.checks[key]; return <span key={key} className={check.passed ? "text-success" : "text-danger"}>{key}: {check.code}{check.elapsed_ms === null ? "" : ` (${check.elapsed_ms} ms)`}</span>; })}</div>
            </div>}
            {!qualification && storedProfile && <div className="alert alert-success" data-testid="stored-model-profile">
                <div className="fw-semibold">Stored qualified profile</div>
                <div className="small">{storedProfile.identity.model_id} · {storedProfile.identity.quantization} · {storedProfile.identity.chat_template} · {storedProfile.identity.backend_version} · parser {storedProfile.identity.parser_version}</div>
                <div className="small">Context limit: {storedProfile.context_tokens.toLocaleString()} tokens · qualified {new Date(storedProfile.qualified_at * 1_000).toLocaleString()}</div>
                <div className="d-flex flex-wrap gap-3 mt-2 small">{(["tools", "structured_json", "context", "vision"] as const).map((key) => { const observed = storedProfile.observed[key] as { passed?: boolean; code?: string } | undefined; return <span key={key} className={observed?.passed ? "text-success" : "text-danger"}>{key}: {observed?.code ?? "not qualified"}</span>; })}</div>
            </div>}

            {snap === null ? (
                <div className="execlaw-muted small p-3" data-testid="inference-loading">
                    Loading…
                </div>
            ) : snap.consumers.length === 0 ? (
                <div
                    className="execlaw-muted small p-3 border rounded"
                    data-testid="inference-empty"
                >
                    No LLM calls observed yet. Counters populate as chat
                    turns, automation runs, routines, or research jobs make
                    inference requests.
                </div>
            ) : (
                <>
                    <ConsumersTable snap={snap} />
                    <h2 className="h6 mt-4">Phase latency</h2>
                    {snap.phases.length === 0 ? (
                        <p className="small text-muted">No phase samples observed yet.</p>
                    ) : (
                        <table className="table table-sm align-middle" data-testid="inference-phases-table">
                            <thead><tr><th>Consumer</th><th>Phase</th><th className="text-end">Samples</th><th className="text-end">p50</th><th className="text-end">p95</th><th>Budget check</th></tr></thead>
                            <tbody>{snap.phases.map((p) => <tr key={`${p.consumer}-${p.phase}`}>
                                <td>{consumerLabel(p.consumer)}</td><td>{phaseLabel(p.phase)}</td>
                                <td className="text-end font-monospace">{p.sample_count}</td>
                                <td className="text-end font-monospace">{fmtMs(p.p50_ms)}</td>
                                <td className="text-end font-monospace">{fmtMs(p.p95_ms)}</td>
                                <td>{regressionLabel(p.regression_detected, p.regression_budget_percent, p.baseline_p95_ms === null ? null : fmtMs(p.baseline_p95_ms))}</td>
                            </tr>)}</tbody>
                        </table>
                    )}
                    <h2 className="h6 mt-4">Context size (request estimates)</h2>
                    <p className="small text-muted">Serialized request size; token counts estimate one token per four bytes. Prompt content is not retained.</p>
                    {snap.contexts.length === 0 ? (
                        <p className="small text-muted">No context samples observed yet.</p>
                    ) : (
                        <table className="table table-sm align-middle" data-testid="inference-context-table">
                            <thead><tr><th>Consumer</th><th className="text-end">Samples</th><th className="text-end">p50 bytes</th><th className="text-end">p95 bytes</th><th className="text-end">p50 tokens*</th><th className="text-end">p95 tokens*</th><th>Budget check</th></tr></thead>
                            <tbody>{snap.contexts.map((c) => <tr key={`${c.consumer}-${c.phase}`}>
                                <td>{consumerLabel(c.consumer)}</td><td className="text-end font-monospace">{c.sample_count}</td>
                                <td className="text-end font-monospace">{fmtNumber(c.p50_serialized_bytes)}</td>
                                <td className="text-end font-monospace">{fmtNumber(c.p95_serialized_bytes)}</td>
                                <td className="text-end font-monospace">{fmtNumber(c.p50_estimated_tokens)}</td>
                                <td className="text-end font-monospace">{fmtNumber(c.p95_estimated_tokens)}</td>
                                <td>{regressionLabel(c.regression_detected, c.regression_budget_percent, fmtNumber(c.baseline_p95_estimated_tokens))}</td>
                            </tr>)}</tbody>
                        </table>
                    )}
                </>
            )}
        </div>
    );
}

function ConsumersTable({ snap }: { snap: MetricsSnapshot }) {
    return (
        <table
            className="table table-sm align-middle"
            data-testid="inference-consumers-table"
        >
            <thead>
                <tr>
                    <th>Consumer</th>
                    <th className="text-end">In flight</th>
                    <th className="text-end">Total calls</th>
                    <th className="text-end">Failures</th>
                    <th className="text-end">p50</th>
                    <th className="text-end">p95</th>
                </tr>
            </thead>
            <tbody>
                {snap.consumers.map((c) => (
                    <tr
                        key={c.consumer}
                        data-testid={`inference-row-${c.consumer}`}
                    >
                        <td>{consumerLabel(c.consumer)}</td>
                        <td className="text-end font-monospace">
                            {c.in_flight}
                        </td>
                        <td className="text-end font-monospace">
                            {c.total_calls}
                        </td>
                        <td className="text-end font-monospace">
                            {c.total_failures}
                            {c.total_calls > 0 && c.total_failures > 0 && (
                                <span className="small text-muted ms-1">
                                    (
                                    {(
                                        (c.total_failures / c.total_calls) *
                                        100
                                    ).toFixed(1)}
                                    %)
                                </span>
                            )}
                        </td>
                        <td className="text-end font-monospace">
                            {fmtMs(c.p50_ms)}
                        </td>
                        <td className="text-end font-monospace">
                            {fmtMs(c.p95_ms)}
                        </td>
                    </tr>
                ))}
            </tbody>
        </table>
    );
}

function fmtMs(ms: number | null): string {
    if (ms === null) return "—";
    if (ms >= 1000) return `${(ms / 1000).toFixed(2)}s`;
    return `${ms}ms`;
}

function fmtNumber(value: number | null): string {
    return value === null ? "—" : value.toLocaleString();
}

function regressionLabel(detected: boolean | null, budgetPercent: number, baseline: string | null): string {
    if (detected === null) return `Collecting baseline (${budgetPercent}% budget)`;
    if (detected) return `Above ${budgetPercent}% budget (baseline ${baseline})`;
    return `Within ${budgetPercent}% budget`;
}

function phaseLabel(phase: import("../api/inference").InferencePhase): string {
    switch (phase) {
        case "prompt_assembly": return "Prompt assembly";
        case "prefill_decode": return "Local inference";
        case "tool_wait": return "Tool wait";
        case "retry": return "Retry";
        case "stream_delay": return "First visible token";
    }
}
