import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";
import { ApiError } from "../api/client";
import { useAuth } from "../auth/AuthContext";
import { postMessage } from "../api/endpoints";
import { ErrorBanner } from "../components/ErrorBanner";
import { ArtifactDownloadLink } from "../components/ArtifactDownloadLink";
import { CompletionEvidenceRef } from "../components/CompletionEvidenceRef";
import {
    confirmRunDelivery,
    getRunCompletion,
    getRunTrace,
    forkRun,
    listRuns,
    listWorkspaceRoots,
    registerWorkspaceRoot,
    createWorkspaceCheckpoint,
    previewWorkspaceDiff,
    applyWorkspaceDiff,
    getWorkspaceApplyState,
    restoreWorkspaceDiff,
    recordRunArtifact,
    recordRunCriterion,
    setRunCompletionContract,
    listUnknownOutboxEffects,
    resolveUnknownOutboxEffect,
    type RunCompletionReport,
    type RunSummary,
    type RunTracePage,
    type UnknownOutboxEffect,
    type UnknownOutboxResolution,
    type WorkspaceRoot,
    type WorkspaceDiffPreview,
    type WorkspaceApplyState,
} from "../api/runs";

const REFRESH_MS = 4_000;

function dateTime(value: number): string {
    const millis = value < 10_000_000_000 ? value * 1_000 : value;
    return new Date(millis).toLocaleString();
}

function recoveryDescription(action: NonNullable<RunTracePage["recovery_action"]>): string {
    const step = action.step;
    switch (action.action) {
        case "claim": return `Pending ${step?.kind ?? "step"} ${step?.step_id ?? ""} can be claimed by its executor.`;
        case "reclaim_expired": return `The lease for ${step?.kind ?? "step"} ${step?.step_id ?? ""} expired; its executor must revalidate before resuming.`;
        case "wait_for_lease": return `${step?.kind ?? "Step"} ${step?.step_id ?? ""} still has an active lease.`;
        case "wait_for_approval": return `Step ${step?.step_id ?? ""} is waiting for approval.`;
        case "wait": return `Step ${step?.step_id ?? ""} is waiting for its executor.`;
        case "advance_cursor": return `Checkpoint ${step?.step_id ?? ""} is complete; replay its stored output before advancing.`;
        case "complete_run": return `No step exists at cursor ${action.cursor ?? "?"}; the run can be finalized without dispatching work.`;
        case "run_failed": return "The run is failed and requires operator review.";
        case "run_cancelled": return "The run is cancelled.";
        case "run_completed": return "The run is complete.";
        default: return `Recovery state: ${action.action.replaceAll("_", " ")}.`;
    }
}

export function RunInspectorPage() {
    const auth = useAuth();
    const token = auth.getAccessToken;
    const [runs, setRuns] = useState<RunSummary[]>([]);
    const [hasMoreRuns, setHasMoreRuns] = useState(false);
    const [selectedId, setSelectedId] = useState<string | null>(null);
    const [snapshot, setSnapshot] = useState<RunTracePage | null>(null);
    const [cursor, setCursor] = useState(0);
    const [events, setEvents] = useState<RunTracePage["events"]>([]);
    const [error, setError] = useState<string | null>(null);
    const [resetNotice, setResetNotice] = useState(false);
    const [autoRefresh, setAutoRefresh] = useState(true);
    const [resuming, setResuming] = useState(false);
    const [forking, setForking] = useState(false);
    const [workspaceRoots, setWorkspaceRoots] = useState<WorkspaceRoot[]>([]);
    const [workspacePath, setWorkspacePath] = useState("");
    const [workspaceId, setWorkspaceId] = useState("");
    const [workspaceBusy, setWorkspaceBusy] = useState(false);
    const [workspaceDiff, setWorkspaceDiff] = useState<WorkspaceDiffPreview | null>(null);
    const [workspaceMessage, setWorkspaceMessage] = useState<string | null>(null);
    const [workspaceApplying, setWorkspaceApplying] = useState(false);
    const workspaceApplyRequest = useRef<{ previewHash: string; requestId: string } | null>(null);
    const [workspaceApplyState, setWorkspaceApplyState] = useState<WorkspaceApplyState | null>(null);
    const workspaceRestoreRequest = useRef<{ applyId: string; requestId: string } | null>(null);
    const [completion, setCompletion] = useState<RunCompletionReport | null>(null);
    const [criteriaText, setCriteriaText] = useState("");
    const [artifactText, setArtifactText] = useState("");
    const [deliveryRequired, setDeliveryRequired] = useState(false);
    const [completionEvidence, setCompletionEvidence] = useState<Record<string, string>>({});
    const [completionDetails, setCompletionDetails] = useState<Record<string, string>>({});
    const [completionStatuses, setCompletionStatuses] = useState<Record<string, "passed" | "failed" | "blocked">>({});
    const [unknownOutboxEffects, setUnknownOutboxEffects] = useState<UnknownOutboxEffect[]>([]);
    const [outboxEvidence, setOutboxEvidence] = useState<Record<number, string>>({});
    const [resolvingOutboxId, setResolvingOutboxId] = useState<number | null>(null);
    const [outboxError, setOutboxError] = useState<string | null>(null);

    const refreshRuns = useCallback(async (append = false) => {
        try {
            const before = append ? runs[runs.length - 1] : undefined;
            if (append && !before) return;
            const next = await listRuns(token, before);
            setRuns((current) => append ? [...current, ...next] : next);
            setHasMoreRuns(next.length === 50);
            setError(null);
            if (!selectedId && next.length) setSelectedId(next[0].run_id);
        } catch (cause) {
            setError((cause as Error).message || "Could not load durable runs");
        }
    }, [token, selectedId, runs]);

    const refreshTrace = useCallback(async (runId: string) => {
        try {
            const page = await getRunTrace(runId, cursor, token);
            const completionReport = await getRunCompletion(runId, token).catch((cause) => {
                if (cause instanceof ApiError && cause.status === 404) return null;
                throw cause;
            });
            const applyState = await getWorkspaceApplyState(runId, token).catch(() => null);
            setSnapshot(page);
            setCompletion(completionReport);
            setWorkspaceApplyState(applyState);
            setEvents((current) => (page.cursor_reset ? page.events : [...current, ...page.events]).slice(-500));
            setCursor(page.next_cursor);
            setResetNotice(page.cursor_reset);
            setError(null);
        } catch (cause) {
            setError((cause as Error).message || "Could not reconnect to the run trace");
        }
    }, [token, cursor]);

    const refreshUnknownOutbox = useCallback(async () => {
        try {
            setUnknownOutboxEffects(await listUnknownOutboxEffects(token));
            setOutboxError(null);
        } catch (cause) {
            setOutboxError((cause as Error).message || "Could not load uncertain delivery outcomes");
        }
    }, [token]);

    const resolveOutboxEffect = async (outboxId: number, resolution: UnknownOutboxResolution) => {
        const evidence_ref = outboxEvidence[outboxId]?.trim();
        if (!evidence_ref) return;
        setResolvingOutboxId(outboxId);
        try {
            await resolveUnknownOutboxEffect(outboxId, resolution, evidence_ref, token);
            setOutboxEvidence((current) => ({ ...current, [outboxId]: "" }));
            await refreshUnknownOutbox();
        } catch (cause) {
            setOutboxError((cause as Error).message || "Could not resolve the outbox outcome");
        } finally {
            setResolvingOutboxId(null);
        }
    };

    useEffect(() => { void refreshRuns(); }, [refreshRuns]);
    useEffect(() => { void refreshUnknownOutbox(); }, [refreshUnknownOutbox]);
    useEffect(() => {
        let mounted = true;
        void listWorkspaceRoots(token).then((roots) => {
            if (!mounted) return;
            setWorkspaceRoots(roots);
            if (!workspaceId && roots.length) setWorkspaceId(roots[0].workspace_id);
        }).catch(() => { if (mounted) setWorkspaceRoots([]); });
        return () => { mounted = false; };
    }, [token]);
    useEffect(() => {
        if (!selectedId) { setSnapshot(null); setEvents([]); setCursor(0); setCompletion(null); return; }
        setEvents([]);
        setCursor(0);
        setCompletion(null);
        setCriteriaText("");
        setArtifactText("");
        setDeliveryRequired(false);
        setCompletionEvidence({});
        setCompletionDetails({});
        setCompletionStatuses({});
        void refreshTrace(selectedId);
    // Selection changes start a fresh authoritative trace read.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [selectedId, token]);
    useEffect(() => {
        if (!autoRefresh) return;
        const timer = window.setInterval(() => {
            void refreshRuns();
            void refreshUnknownOutbox();
            if (selectedId) void refreshTrace(selectedId);
        }, REFRESH_MS);
        return () => window.clearInterval(timer);
    }, [autoRefresh, refreshRuns, refreshTrace, refreshUnknownOutbox, selectedId]);

    const selected = useMemo(() => runs.find((run) => run.run_id === selectedId) ?? null, [runs, selectedId]);
    const eventDurations = useMemo(() => {
        const started = new Map<string, number>();
        const durations = new Map<number, number>();
        for (const event of events) {
            const key = `${event.event_kind}\0${event.subject_id ?? ""}`;
            if (event.status === "started") {
                started.set(key, event.created_at);
            } else {
                const start = started.get(key);
                if (start !== undefined) durations.set(event.cursor, Math.max(0, event.created_at - start));
                started.delete(key);
            }
        }
        return durations;
    }, [events]);
    const lastDelivery = snapshot?.delivery.at(-1);
    const deliveryUncertain = lastDelivery?.transition === "send_requested"
        || lastDelivery?.transition === "lease_reclaimed"
        || lastDelivery?.transition === "retry_scheduled"
        || lastDelivery?.transition === "outcome_unknown"
        || lastDelivery?.transition === "retry_authorized";

    const saveContract = async () => {
        if (!snapshot) return;
        try {
            const acceptance_criteria = criteriaText.split("\n").map((line) => line.trim()).filter(Boolean)
                .map((description, index) => ({ criterion_id: `criterion-${index + 1}`, description, required: true }));
            const required_artifacts = artifactText.split("\n").map((line) => line.trim()).filter(Boolean)
                .map((description, index) => ({ artifact_id: `artifact-${index + 1}`, description }));
            const report = await setRunCompletionContract(snapshot.run.run_id, { acceptance_criteria, required_artifacts, delivery_required: deliveryRequired }, token);
            setCompletion(report);
            setError(null);
        } catch (cause) {
            setError((cause as Error).message || "Could not save the completion contract");
        }
    };

    const saveCriterion = async (criterionId: string) => {
        if (!snapshot) return;
        try {
            const evidence_refs = (completionEvidence[criterionId] ?? "").split("\n").map((value) => value.trim()).filter(Boolean);
            const report = await recordRunCriterion(snapshot.run.run_id, {
                criterion_id: criterionId,
                status: completionStatuses[criterionId] ?? "passed",
                evidence_refs,
                detail: completionDetails[criterionId]?.trim() || null,
            }, token);
            setCompletion(report);
            setError(null);
        } catch (cause) {
            setError((cause as Error).message || "Could not record acceptance evidence");
        }
    };

    const saveArtifact = async (artifactId: string, present: boolean) => {
        if (!snapshot) return;
        try {
            const report = await recordRunArtifact(snapshot.run.run_id, {
                artifact_id: artifactId,
                present,
                evidence_ref: completionEvidence[artifactId]?.trim() || null,
                detail: completionDetails[artifactId]?.trim() || null,
            }, token);
            setCompletion(report);
            setError(null);
        } catch (cause) {
            setError((cause as Error).message || "Could not record artifact evidence");
        }
    };

    const saveDelivery = async () => {
        if (!snapshot) return;
        try {
            const report = await confirmRunDelivery(snapshot.run.run_id, completionEvidence.delivery ?? "", token);
            setCompletion(report);
            setError(null);
        } catch (cause) {
            setError((cause as Error).message || "Could not confirm delivery");
        }
    };

    const resumeRun = async () => {
        if (!snapshot || !snapshot.recovery_action) return;
        const retryable = ["claim", "reclaim_expired", "advance_cursor", "complete_run"]
            .includes(snapshot.recovery_action.action);
        if (!retryable || !window.confirm("Resume this saved Controller run from its durable checkpoint? The server will refuse active approvals and ambiguous direct effects. Check delivery receipts before continuing.")) return;
        setResuming(true);
        try {
            await postMessage(
                snapshot.run.conversation_id,
                { text: "", resume_run_id: snapshot.run.run_id },
                token,
                crypto.randomUUID(),
            );
            setError(null);
            await Promise.all([refreshRuns(), refreshTrace(snapshot.run.run_id)]);
        } catch (cause) {
            setError((cause as Error).message || "Could not resume the saved run");
        } finally {
            setResuming(false);
        }
    };

    const createFork = async () => {
        if (!snapshot) return;
        setForking(true);
        try {
            const fork = await forkRun(snapshot.run.run_id, token);
            setEvents([]);
            setCursor(0);
            setSelectedId(fork.run_id);
            await refreshRuns();
            setError(null);
        } catch (cause) {
            setError((cause as Error).message || "Could not create a fresh run fork");
        } finally {
            setForking(false);
        }
    };

    const addWorkspaceRoot = async () => {
        if (!workspacePath.trim()) return;
        setWorkspaceBusy(true);
        try {
            const root = await registerWorkspaceRoot(workspacePath.trim(), token);
            setWorkspaceRoots((current) => [...current, root]);
            setWorkspaceId(root.workspace_id);
            setWorkspacePath("");
        } catch (cause) {
            setError((cause as Error).message || "Could not register workspace root");
        } finally { setWorkspaceBusy(false); }
    };

    const checkpointWorkspace = async () => {
        if (!snapshot || !workspaceId) return;
        setWorkspaceBusy(true);
        try {
            const checkpoint = await createWorkspaceCheckpoint(snapshot.run.run_id, workspaceId, token);
            setError(null);
            setWorkspaceDiff(null);
            setWorkspaceMessage(`Checkpoint ${checkpoint.checkpoint_id} saved with ${checkpoint.file_count} files (${checkpoint.total_bytes} bytes).`);
        } catch (cause) {
            setError((cause as Error).message || "Could not checkpoint workspace");
        } finally { setWorkspaceBusy(false); }
    };

    const previewWorkspace = async () => {
        if (!snapshot) return;
        setWorkspaceBusy(true);
        try {
            setWorkspaceDiff(await previewWorkspaceDiff(snapshot.run.run_id, token));
            workspaceApplyRequest.current = null;
            setError(null);
            setWorkspaceMessage(null);
        } catch (cause) {
            setError((cause as Error).message || "Could not preview workspace changes");
        } finally { setWorkspaceBusy(false); }
    };

    const applyWorkspace = async () => {
        if (!snapshot || !workspaceDiff || workspaceDiff.conflicts > 0) return;
        const request = workspaceApplyRequest.current?.previewHash === workspaceDiff.preview_hash
            ? workspaceApplyRequest.current
            : { previewHash: workspaceDiff.preview_hash, requestId: crypto.randomUUID() };
        workspaceApplyRequest.current = request;
        if (!window.confirm(`Apply ${workspaceDiff.changes.length} reviewed file changes to the registered workspace? External sends, commits, and publication are not part of this operation.`)) return;
        setWorkspaceApplying(true);
        try {
            const result = await applyWorkspaceDiff(snapshot.run.run_id, workspaceDiff.preview_hash, request.requestId, token);
            setWorkspaceMessage(`Workspace apply receipt: ${JSON.stringify(result)}`);
            setWorkspaceApplyState(await getWorkspaceApplyState(snapshot.run.run_id, token));
            await previewWorkspace();
            setError(null);
        } catch (cause) {
            setError((cause as Error).message || "Workspace apply stopped on a concurrent edit");
            await previewWorkspace();
        } finally { setWorkspaceApplying(false); }
    };

    const restoreWorkspace = async () => {
        if (!snapshot || !workspaceApplyState?.latest_apply) return;
        const source = workspaceApplyState.latest_apply;
        const request = workspaceRestoreRequest.current?.applyId === source.apply_id
            ? workspaceRestoreRequest.current
            : { applyId: source.apply_id, requestId: crypto.randomUUID() };
        workspaceRestoreRequest.current = request;
        if (!window.confirm(`Restore only the changes owned by ${source.apply_id}? Any file changed since that apply will be preserved and reported as a conflict.`)) return;
        setWorkspaceApplying(true);
        try {
            const receipt = await restoreWorkspaceDiff(snapshot.run.run_id, source.apply_id, request.requestId, token);
            setWorkspaceMessage(`Restore receipt: ${receipt.apply_id} (${receipt.status}).`);
            setWorkspaceApplyState(await getWorkspaceApplyState(snapshot.run.run_id, token));
            await previewWorkspace();
            setError(null);
        } catch (cause) {
            if (cause instanceof ApiError && cause.status === 409) workspaceRestoreRequest.current = null;
            setError((cause as Error).message || "Workspace restore stopped on a later edit");
            await previewWorkspace();
            setWorkspaceApplyState(await getWorkspaceApplyState(snapshot.run.run_id, token).catch(() => null));
        } finally { setWorkspaceApplying(false); }
    };

    const exportTrace = async () => {
        if (!snapshot) return;
        try {
            let page = await getRunTrace(snapshot.run.run_id, 0, token);
            const allEvents = [...page.events];
            let next = page.next_cursor;
            while (page.events.length === 200 && next > 0) {
                page = await getRunTrace(snapshot.run.run_id, next, token);
                if (!page.events.length || page.next_cursor === next) break;
                allEvents.push(...page.events);
                next = page.next_cursor;
            }
            const payload = { exported_at: new Date().toISOString(), privacy: "metadata_only", ...page, events: allEvents };
            const blob = new Blob([JSON.stringify(payload, null, 2)], { type: "application/json" });
            const url = URL.createObjectURL(blob);
            const anchor = document.createElement("a");
            anchor.href = url;
            anchor.download = `execlaw-run-${snapshot.run.run_id}.json`;
            anchor.click();
            URL.revokeObjectURL(url);
        } catch (cause) {
            setError((cause as Error).message || "Could not export the complete run trace");
        }
    };

    return (
        <section data-testid="run-inspector-page">
            <ErrorBanner message={error} onDismiss={() => setError(null)} />
            <div className="d-flex flex-wrap justify-content-between align-items-center gap-2 mb-3">
                <div>
                    <h3 className="h5 mb-1">Execution inspector</h3>
                    <p className="text-muted small mb-0">Durable run state and reconnectable, metadata-only traces.</p>
                </div>
                <div className="d-flex gap-2">
                    <Button size="sm" variant="outline-secondary" onClick={() => setAutoRefresh((value) => !value)}>
                        Auto refresh: {autoRefresh ? "On" : "Off"}
                    </Button>
                    <Button size="sm" variant="outline-secondary" onClick={() => { void refreshRuns(); void refreshUnknownOutbox(); if (selectedId) void refreshTrace(selectedId); }}>
                        Refresh
                    </Button>
                    <Button size="sm" variant="outline-primary" disabled={!snapshot} onClick={() => { void exportTrace(); }}>
                        Export trace
                    </Button>
                </div>
            </div>
            {outboxError && <ErrorBanner message={outboxError} onDismiss={() => setOutboxError(null)} />}
            {unknownOutboxEffects.length > 0 && <div className="alert alert-warning mb-3" role="status" data-testid="unknown-outbox-effects">
                <h4 className="h6">External delivery needs review</h4>
                <p className="small mb-2">The sink may have accepted these effects. Check its status before authorizing another attempt.</p>
                <ul className="list-group list-group-flush">{unknownOutboxEffects.map((effect) => <li key={effect.id} className="list-group-item px-0 bg-transparent">
                    <div><code>#{effect.id}</code> · {effect.effect_kind} · conversation {effect.conversation_id} · attempt {effect.attempts}</div>
                    <div className="d-flex flex-wrap gap-2 mt-2">
                        <Form.Control size="sm" className="flex-grow-1" style={{ minWidth: "16rem" }} aria-label={`Resolution evidence for outbox ${effect.id}`} placeholder="Receipt reference or reason after checking the sink" value={outboxEvidence[effect.id] ?? ""} onChange={(event) => setOutboxEvidence((current) => ({ ...current, [effect.id]: event.target.value }))} />
                        <Button size="sm" variant="outline-danger" disabled={!outboxEvidence[effect.id]?.trim() || resolvingOutboxId !== null} onClick={() => { void resolveOutboxEffect(effect.id, "authorize_retry"); }}>{resolvingOutboxId === effect.id ? "Saving…" : "Authorize retry"}</Button>
                        <Button size="sm" variant="outline-success" disabled={!outboxEvidence[effect.id]?.trim() || resolvingOutboxId !== null} onClick={() => { void resolveOutboxEffect(effect.id, "confirm_delivered"); }}>{resolvingOutboxId === effect.id ? "Saving…" : "Confirm delivered"}</Button>
                    </div>
                </li>)}</ul>
            </div>}
            {resetNotice && <div className="alert alert-info py-2" role="status">The saved trace cursor was unavailable. The inspector reloaded the authoritative run snapshot.</div>}
            <div className="row g-3">
                <div className="col-12 col-xl-4">
                    <div className="list-group" aria-label="Durable runs">
                        {runs.map((run) => (
                            <button key={run.run_id} type="button" className={`list-group-item list-group-item-action${run.run_id === selectedId ? " active" : ""}`} onClick={() => setSelectedId(run.run_id)}>
                                <div className="d-flex justify-content-between gap-2"><strong>{run.status}</strong><small>{dateTime(run.started_at)}</small></div>
                                <code className="d-block text-truncate">{run.run_id}</code>
                                <small className="text-muted">Conversation {run.conversation_id} · cursor {run.cursor}</small>
                            </button>
                        ))}
                        {hasMoreRuns && <button type="button" className="list-group-item list-group-item-action text-center" onClick={() => { void refreshRuns(true); }}>Load older runs</button>}
                        {!runs.length && <div className="text-muted small p-3">No durable runs are available.</div>}
                    </div>
                </div>
                <div className="col-12 col-xl-8">
                    {!snapshot || !selected ? <div className="text-muted p-3">Select a run to inspect its checkpoints and trace.</div> : <>
                        <div className="card mb-3"><div className="card-body">
                            <div className="d-flex justify-content-between flex-wrap gap-2"><h4 className="h6 mb-0">Run {snapshot.run.run_id}</h4><span className="badge text-bg-secondary">{snapshot.run.status}</span></div>
                            <div className="small text-muted mt-2">Conversation {snapshot.run.conversation_id} · input event {snapshot.run.input_event_seq} · started {dateTime(snapshot.run.started_at)} · updated {dateTime(snapshot.run.updated_at)}</div>
                            <div className="small mt-2">Trace cursor {cursor}; next poll resumes after this durable event.</div>
                            <Button size="sm" variant="outline-secondary" className="mt-2" disabled={forking} onClick={() => void createFork()}>{forking ? "Forking…" : "Fork fresh run"}</Button>
                            {snapshot.recovery_action && <div className="alert alert-warning py-2 small mt-2 mb-0" role="status"><strong>Recovery checkpoint:</strong> {recoveryDescription(snapshot.recovery_action)}
                                <div className="mt-2"><Button size="sm" variant="warning" disabled={resuming || !["claim", "reclaim_expired", "advance_cursor", "complete_run"].includes(snapshot.recovery_action.action)} onClick={() => void resumeRun()}>{resuming ? "Resuming…" : "Resume saved run"}</Button></div>
                            </div>}
                        </div></div>
                        {snapshot.asset_loadout && <div className="card mb-3" data-testid="run-asset-loadout"><div className="card-header">Governed memory loaded for this turn</div><div className="card-body">
                            <div className="small text-muted mb-2">Resolved {dateTime(snapshot.asset_loadout.resolved_at)} · scope <code>{snapshot.asset_loadout.agent_scope}</code> · conversation trust {snapshot.asset_loadout.conversation_trust_class}</div>
                            <div className="small text-muted mb-2">Readable trust: {snapshot.asset_loadout.readable_trust_classes.join(", ") || "none"} · owner scopes: {snapshot.asset_loadout.readable_owner_scopes.join(", ") || "none"}</div>
                            {snapshot.asset_loadout.assets.length > 0 ? <ul className="list-group list-group-flush">{snapshot.asset_loadout.assets.map((asset) => <li key={asset.asset_id} className="list-group-item px-0 small">
                                <div><strong>{asset.name}</strong> · <code>{asset.asset_id}</code> · v{asset.version} · {asset.injected_chars} chars injected</div>
                                <div className="text-muted">Owner {asset.owner_scope} · visibility {asset.visibility} · trust floor {asset.trust_floor} · source {asset.source_hash ?? "unversioned"}</div>
                                <div className="text-muted">Binding {asset.binding_mode} in <code>{asset.binding_agent_scope}</code> · priority {asset.binding_priority} · cap {asset.binding_max_chars} chars · {asset.admission_reasons.join(", ")}</div>
                            </li>)}</ul> : <div className="small text-muted">No governed HOT assets were eligible for this turn.</div>}
                            <div className="border-top mt-2 pt-2"><strong className="small">Trust-first retrieved evidence</strong>{snapshot.asset_loadout.retrieval_query_sha256 && <div className="small text-muted">Query fingerprint <code>{snapshot.asset_loadout.retrieval_query_sha256}</code></div>}
                                {snapshot.asset_loadout.retrieved_assets.length > 0 ? <ul className="list-group list-group-flush">{snapshot.asset_loadout.retrieved_assets.map((asset) => <li key={asset.asset_id} className="list-group-item px-0 small">
                                    <div><strong>{asset.name}</strong> · <code>{asset.asset_id}</code> · v{asset.version} · {asset.injected_chars} chars injected</div>
                                    <div className="text-muted">Rank {asset.lexical_rank || "—"}/{asset.vector_rank ?? "—"} · score {(asset.score_micros / 1_000_000).toFixed(4)} · owner {asset.owner_scope} · trust floor {asset.trust_floor} · source {asset.source_hash ?? "unversioned"}</div>
                                    <div className="text-muted">{asset.admission_reasons.join(", ")}</div>
                                </li>)}</ul> : <div className="small text-muted">No trust-eligible indexed evidence matched this turn.</div>}
                            </div>
                        </div></div>}
                        <div className="card mb-3"><div className="card-header">Workspace checkpoint and diff preview</div><div className="card-body">
                            <p className="small text-muted">A checkpoint copies the registered tree into an isolated run checkout. Snapshot traversal skips secret files and rejects symlinks, junctions, and hard links.</p>
                            <div className="input-group input-group-sm mb-2"><Form.Control aria-label="Workspace root path" placeholder="Absolute workspace root path" value={workspacePath} onChange={(event) => setWorkspacePath(event.target.value)} /><Button variant="outline-secondary" disabled={workspaceBusy || !workspacePath.trim()} onClick={() => void addWorkspaceRoot()}>Register root</Button></div>
                            <div className="d-flex gap-2 flex-wrap"><Form.Select aria-label="Registered workspace" size="sm" className="w-auto" value={workspaceId} onChange={(event) => setWorkspaceId(event.target.value)}><option value="">Select workspace</option>{workspaceRoots.map((root) => <option key={root.workspace_id} value={root.workspace_id}>{root.canonical_path}</option>)}</Form.Select><Button size="sm" variant="outline-primary" disabled={workspaceBusy || !workspaceId} onClick={() => void checkpointWorkspace()}>Create isolated checkpoint</Button><Button size="sm" variant="outline-secondary" disabled={workspaceBusy} onClick={() => void previewWorkspace()}>Preview changes</Button></div>
                            {workspaceApplyState?.latest_apply && !(workspaceApplyState.latest_restore && workspaceApplyState.latest_restore.updated_at >= workspaceApplyState.latest_apply.updated_at) && <div className="small mt-2">Last applied diff <code>{workspaceApplyState.latest_apply.apply_id}</code> Â· {dateTime(workspaceApplyState.latest_apply.updated_at)} <Button size="sm" variant="outline-danger" disabled={workspaceApplying} onClick={() => void restoreWorkspace()}>{workspaceApplying ? "Restoring…" : "Restore owned changes"}</Button></div>}
                            {workspaceMessage && <div className="small text-success mt-2" role="status">{workspaceMessage}</div>}
                            {workspaceDiff && <div className="mt-3"><div className="small">{workspaceDiff.changes.length} changed paths Â· {workspaceDiff.conflicts} conflicts Â· preview <code>{workspaceDiff.preview_hash}</code></div>{workspaceDiff.changes.map((change) => <div key={change.path} className={`small border-top py-1${change.conflict ? " text-danger" : ""}`}><code>{change.path}</code> Â· {change.conflict ? "conflicts with operator edits" : change.proposed_sha256 ? "ready" : "delete"}</div>)}<Button size="sm" variant="outline-danger" className="mt-2" disabled={workspaceApplying || workspaceDiff.conflicts > 0 || workspaceDiff.changes.length === 0} onClick={() => void applyWorkspace()}>{workspaceApplying ? "Applying…" : "Apply reviewed diff"}</Button><div className="small text-muted mt-2">Only files matching the reviewed base hashes are replaced or removed. External sends, commits, and publication remain separate.</div></div>}
                        </div></div>
                        {!completion ? <div className="card mb-3"><div className="card-header">Completion contract</div><div className="card-body">
                            <p className="small text-muted">Define required outcomes before marking evidence. One criterion or artifact is required. A model reply alone does not verify completion.</p>
                            <label className="form-label" htmlFor="run-acceptance-criteria">Acceptance criteria (one per line)</label>
                            <textarea id="run-acceptance-criteria" className="form-control mb-2" rows={3} value={criteriaText} onChange={(event) => setCriteriaText(event.target.value)} />
                            <label className="form-label" htmlFor="run-required-artifacts">Required artifacts (one per line)</label>
                            <textarea id="run-required-artifacts" className="form-control mb-2" rows={2} value={artifactText} onChange={(event) => setArtifactText(event.target.value)} />
                            <div className="form-check mb-3"><input id="run-delivery-required" className="form-check-input" type="checkbox" checked={deliveryRequired} onChange={(event) => setDeliveryRequired(event.target.checked)} /><label className="form-check-label" htmlFor="run-delivery-required">External delivery must be confirmed</label></div>
                            <Button size="sm" onClick={() => { void saveContract(); }} disabled={!criteriaText.trim() && !artifactText.trim()}>Save immutable contract</Button>
                        </div></div> : <div className="card mb-3"><div className="card-header d-flex justify-content-between"><span>Completion evidence</span><strong className="text-capitalize">{completion.status.replaceAll("_", " ")}</strong></div><div className="card-body">
                            <p className="small text-muted">Controller-entered evidence is retained with the run. Use trace:&lt;cursor&gt; for an execution event, attachment:&lt;id&gt; for a downloadable artifact, or an HTTPS verifier report URL to create a review link. Other references remain inert text. Required failed, blocked, or missing evidence prevents verified completion.</p>
                            {completion.contract.acceptance_criteria.map((criterion) => {
                                const recorded = completion.verifications.find((item) => item.criterion_id === criterion.criterion_id);
                                return <div key={criterion.criterion_id} className="border rounded p-2 mb-2"><strong>{criterion.description}</strong>{!criterion.required && <span className="badge text-bg-light ms-2">optional</span>}
                                    <div className="small text-muted">Status: {recorded?.status ?? "pending"}{recorded?.evidence_refs.length ? <span> · Evidence: {recorded.evidence_refs.map((reference, index) => <span key={`${index}-${reference}`} className="me-2"><CompletionEvidenceRef reference={reference} /></span>)}</span> : null}{recorded?.detail ? ` · ${recorded.detail}` : ""}</div>
                                    <div className="row g-2 mt-1"><div className="col-md-3"><label className="visually-hidden" htmlFor={`criterion-status-${criterion.criterion_id}`}>Verification status</label><select id={`criterion-status-${criterion.criterion_id}`} className="form-select form-select-sm" value={completionStatuses[criterion.criterion_id] ?? "passed"} onChange={(event) => setCompletionStatuses((current) => ({ ...current, [criterion.criterion_id]: event.target.value as "passed" | "failed" | "blocked" }))}><option value="passed">Passed</option><option value="failed">Failed</option><option value="blocked">Blocked</option></select></div><div className="col-md-6"><label className="visually-hidden" htmlFor={`criterion-evidence-${criterion.criterion_id}`}>Evidence reference</label><input id={`criterion-evidence-${criterion.criterion_id}`} className="form-control form-control-sm" placeholder="Evidence reference (required for pass)" value={completionEvidence[criterion.criterion_id] ?? ""} onChange={(event) => setCompletionEvidence((current) => ({ ...current, [criterion.criterion_id]: event.target.value }))} /></div><div className="col-md-3"><Button size="sm" variant="outline-primary" disabled={(completionStatuses[criterion.criterion_id] ?? "passed") === "passed" && !(completionEvidence[criterion.criterion_id] ?? "").trim()} onClick={() => { void saveCriterion(criterion.criterion_id); }}>Record</Button></div></div>
                                    <label className="visually-hidden" htmlFor={`criterion-detail-${criterion.criterion_id}`}>Verification notes</label><input id={`criterion-detail-${criterion.criterion_id}`} className="form-control form-control-sm mt-2" placeholder="Optional verifier note" value={completionDetails[criterion.criterion_id] ?? ""} onChange={(event) => setCompletionDetails((current) => ({ ...current, [criterion.criterion_id]: event.target.value }))} />
                                </div>;
                            })}
                            {completion.contract.required_artifacts.map((artifact) => {
                                const recorded = completion.artifacts.find((item) => item.artifact_id === artifact.artifact_id);
                                return <div key={artifact.artifact_id} className="border rounded p-2 mb-2"><strong>{artifact.description}</strong><div className="small text-muted">{recorded?.present && recorded.evidence_ref ? <>Present · <CompletionEvidenceRef reference={recorded.evidence_ref} /></> : "Not verified"}</div><div className="input-group input-group-sm mt-2"><input className="form-control" aria-label={`Evidence reference for ${artifact.description}`} placeholder="Artifact path or evidence reference" value={completionEvidence[artifact.artifact_id] ?? ""} onChange={(event) => setCompletionEvidence((current) => ({ ...current, [artifact.artifact_id]: event.target.value }))} /><Button variant="outline-primary" disabled={!(completionEvidence[artifact.artifact_id] ?? "").trim()} onClick={() => { void saveArtifact(artifact.artifact_id, true); }}>Confirm present</Button><Button variant="outline-secondary" onClick={() => { void saveArtifact(artifact.artifact_id, false); }}>Missing</Button></div></div>;
                            })}
                            {completion.contract.delivery_required && <div className="border rounded p-2"><strong>External delivery</strong><div className="small text-muted">{completion.delivery_confirmed && completion.delivery_evidence_ref ? <>Confirmed · <CompletionEvidenceRef reference={completion.delivery_evidence_ref} /></> : "Confirmation required"}</div><div className="input-group input-group-sm mt-2"><input className="form-control" aria-label="Delivery evidence reference" placeholder="Durable receipt or delivery reference" value={completionEvidence.delivery ?? ""} onChange={(event) => setCompletionEvidence((current) => ({ ...current, delivery: event.target.value }))} /><Button variant="outline-primary" disabled={!(completionEvidence.delivery ?? "").trim()} onClick={() => { void saveDelivery(); }}>Confirm delivery</Button></div></div>}
                            {completion.unfinished.length > 0 && <ul className="small text-danger mt-3 mb-0">{completion.unfinished.map((item) => <li key={item}>{item}</li>)}</ul>}
                        </div></div>}
                        <div className="card mb-3"><div className="card-header">Checkpoints and retries</div><div className="table-responsive"><table className="table table-sm mb-0"><thead><tr><th>Step</th><th>Kind</th><th>Status</th><th>Attempts</th><th>Timing</th></tr></thead><tbody>
                            {snapshot.steps.map((step) => <tr key={step.step_id} id={`step-${encodeURIComponent(step.step_id)}`}><td><code>{step.step_id}</code>{step.approval_id && <small className="d-block">Approval {step.approval_id}</small>}</td><td>{step.kind}</td><td>{step.status}</td><td>{step.attempt}</td><td>{step.started_at ? dateTime(step.started_at) : "—"}{step.completed_at ? ` → ${dateTime(step.completed_at)}` : ""}</td></tr>)}
                            {!snapshot.steps.length && <tr><td colSpan={5} className="text-muted">No step checkpoints.</td></tr>}
                        </tbody></table></div>
                        {snapshot.retries.length > 0 && <ul className="list-group list-group-flush">{snapshot.retries.map((retry) => <li key={retry.step_id} className="list-group-item small"><strong>{retry.tool_name}</strong> · {retry.status} · {retry.attempts_used}/{retry.retry_budget_total} attempts{retry.next_retry_at_ms ? ` · retry at ${dateTime(retry.next_retry_at_ms)}` : ""}<span className="d-block text-muted">Schema {retry.input_schema_hash ?? "unknown"}</span></li>)}</ul>}
                        </div>
                        {snapshot.children.length > 0 && <div className="card mb-3"><div className="card-header">Child runs</div><ul className="list-group list-group-flush">{snapshot.children.map((child) => { const task = snapshot.child_tasks.find((item) => item.child_run_id === child.run_id); return <li key={child.run_id} className="list-group-item"><div className="d-flex justify-content-between"><button type="button" className="btn btn-link p-0" onClick={() => setSelectedId(child.run_id)}><code>{child.run_id}</code></button><span>{child.status}</span></div>{task && <><div className="small mt-1">{task.task.task ?? "Delegated task"}</div><div className="small text-muted">Budget {task.tokens_used ?? task.budget_tokens}/{task.budget_tokens} tokens Â· trust ceiling {JSON.stringify(task.trust_ceiling)}{task.dependencies.length ? ` Â· depends on ${task.dependencies.join(", ")}` : " Â· no dependencies"}</div>{task.result_artifact_id && <div className="small">Result artifact: <ArtifactDownloadLink artifactId={task.result_artifact_id} /></div>}</>}</li>; })}</ul></div>}
                        <div className="card mb-3"><div className="card-header">Execution events</div><ol className="list-group list-group-flush">{events.map((event) => { const duration = eventDurations.get(event.cursor); return <li id={`trace-${event.cursor}`} key={event.cursor} className="list-group-item small"><div className="d-flex justify-content-between gap-2"><strong>{event.event_kind}: {event.status}{duration === undefined ? "" : ` · ${duration} ms`}</strong><time>{dateTime(event.created_at)}</time></div><code>{event.subject_id ?? "—"}</code><pre className="small mb-0 mt-1">{JSON.stringify(event.metadata, null, 2)}</pre></li>; })}{!events.length && <li className="list-group-item text-muted">No trace events recorded yet.</li>}</ol></div>
                        <div className="card"><div className="card-header">Delivery timeline</div><ol className="list-group list-group-flush">{snapshot.delivery.map((event, index) => <li key={`${event.event_seq}-${event.transition}-${index}`} className="list-group-item small"><strong>{event.transition}</strong> · event {event.event_seq} · attempt {event.attempt} · {dateTime(event.occurred_at)}{event.actor && <span className="d-block text-muted">Resolved by {event.actor}</span>}{event.external_receipt && <span className="d-block text-muted">Receipt {event.external_receipt}</span>}</li>)}{!snapshot.delivery.length && <li className="list-group-item text-muted">No transport delivery receipts for this conversation.</li>}</ol></div>
                        {deliveryUncertain && <div className="alert alert-warning py-2 mt-2">Delivery outcome is uncertain. Check the transport receipt or retry state before assuming the message arrived.</div>}
                    </>}
                </div>
            </div>
        </section>
    );
}
