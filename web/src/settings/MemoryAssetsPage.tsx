import { useCallback, useEffect, useState, type FormEvent } from "react";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";
import {
    bindAdminMemoryAsset,
    correctAdminMemoryAssertion,
    deleteAdminMemoryAsset,
    forgetAdminMemorySourceEvent,
    getAdminMemoryRetrievalConfig,
    getAdminMemoryEvidenceSource,
    getAdminMemoryAssets,
    linkAdminMemoryAssetAssertion,
    putAdminMemoryRetrievalConfig,
    rebuildAdminMemoryEmbeddings,
    retractAdminMemoryAssertion,
    unbindAdminMemoryAsset,
    type AdminMemoryAsset,
    type AdminMemoryAssetBinding,
    type AdminMemoryAssertion,
    type AdminMemoryEvidenceSource,
    type AdminMemoryRetrievalConfig,
} from "../api/endpoints";
import { useAuth } from "../auth/AuthContext";
import { ErrorBanner } from "../components/ErrorBanner";

export function MemoryAssetsPage() {
    const { getAccessToken } = useAuth();
    const [assets, setAssets] = useState<AdminMemoryAsset[]>([]);
    const [bindings, setBindings] = useState<AdminMemoryAssetBinding[]>([]);
    const [assertions, setAssertions] = useState<AdminMemoryAssertion[]>([]);
    const [agentScopes, setAgentScopes] = useState<Array<{ id: string; name: string }>>([]);
    const [retrievalConfig, setRetrievalConfig] = useState<AdminMemoryRetrievalConfig | null>(null);
    const [embeddingModelId, setEmbeddingModelId] = useState("");
    const [rebuildStatus, setRebuildStatus] = useState<string | null>(null);
    const [privacyStatus, setPrivacyStatus] = useState<string | null>(null);
    const [agentScope, setAgentScope] = useState("default");
    const [selectedAsset, setSelectedAsset] = useState("");
    const [mode, setMode] = useState<AdminMemoryAssetBinding["injection_mode"]>("hot");
    const [priority, setPriority] = useState(0);
    const [maxChars, setMaxChars] = useState(512);
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [retractionReasons, setRetractionReasons] = useState<Record<string, string>>({});
    const [correctionValues, setCorrectionValues] = useState<Record<string, string>>({});
    const [sourceQuotes, setSourceQuotes] = useState<Record<string, AdminMemoryEvidenceSource>>({});
    const [sourceLoadingId, setSourceLoadingId] = useState<string | null>(null);
    const [visibleSourceId, setVisibleSourceId] = useState<string | null>(null);
    const [linkTargets, setLinkTargets] = useState<Record<string, string>>({});

    const refresh = useCallback(async () => {
        try {
            const result = await getAdminMemoryAssets(getAccessToken, agentScope);
            const retrieval = await getAdminMemoryRetrievalConfig(getAccessToken);
            setAssets(result.assets);
            setBindings(result.bindings);
            setAssertions(result.assertions ?? []);
            setAgentScopes(result.agent_scopes ?? []);
            setRetrievalConfig(retrieval);
            setEmbeddingModelId(retrieval?.embedding_model_id ?? "");
            setSelectedAsset((current) =>
                result.assets.some((asset) => asset.asset_id === current)
                    ? current
                    : result.assets[0]?.asset_id || "",
            );
            setError(null);
        } catch (reason) {
            setError(reason instanceof Error ? reason.message : String(reason));
        }
    }, [agentScope, getAccessToken]);

    useEffect(() => {
        void refresh();
    }, [refresh]);

    const submitBinding = async (event: FormEvent<HTMLFormElement>) => {
        event.preventDefault();
        if (!selectedAsset) return;
        setBusy(true);
        try {
            await bindAdminMemoryAsset(selectedAsset, {
                agent_scope: agentScope,
                injection_mode: mode,
                priority,
                max_chars: maxChars,
            }, getAccessToken);
            await refresh();
        } catch (reason) {
            setError(reason instanceof Error ? reason.message : String(reason));
        } finally {
            setBusy(false);
        }
    };

    const removeBinding = async (assetId: string) => {
        setBusy(true);
        try {
            await unbindAdminMemoryAsset(assetId, getAccessToken, agentScope);
            await refresh();
        } catch (reason) {
            setError(reason instanceof Error ? reason.message : String(reason));
        } finally {
            setBusy(false);
        }
    };

    const linkAssertion = async (assertion: AdminMemoryAssertion) => {
        const assetId = linkTargets[assertion.assertion_id];
        if (!assetId) return;
        setBusy(true);
        try {
            await linkAdminMemoryAssetAssertion(assetId, assertion.assertion_id, getAccessToken);
            await refresh();
        } catch (reason) {
            setError(reason instanceof Error ? reason.message : String(reason));
        } finally {
            setBusy(false);
        }
    };

    const exportSafeMemory = () => {
        const payload = {
            format: "execlaw-memory-review-export-v1",
            exported_at: new Date().toISOString(),
            scope: agentScope,
            assets: assets.map(({ asset_id, asset_type, name, description, owner_scope, visibility, trust_floor, status, version, source_hash, expires_at, assertion_ids }) => ({
                asset_id, asset_type, name, description, owner_scope, visibility, trust_floor,
                status, version, source_hash, expires_at, assertion_ids: assertion_ids ?? [],
            })),
            assertions: assertions.map(({ assertion_id, scope, trust_class, kind, subject, predicate, object, confidence, status, observed_from, observed_to, valid_from, valid_to, supersedes_id, extraction_run_id, created_event_seq, created_at, evidence, review }) => ({
                assertion_id, scope, trust_class, kind, subject, predicate, object, confidence,
                status, observed_from, observed_to, valid_from, valid_to, supersedes_id,
                extraction_run_id, created_event_seq, created_at,
                evidence: evidence.map(({ evidence_id, conversation_id, event_seq, payload_path, quote_hash, evidence_kind, created_at }) => ({
                    evidence_id, conversation_id, event_seq, payload_path, quote_hash, evidence_kind, created_at,
                })),
                review,
            })),
        };
        const blob = new Blob([JSON.stringify(payload, null, 2)], { type: "application/json" });
        const url = URL.createObjectURL(blob);
        const link = document.createElement("a");
        link.href = url;
        link.download = `execlaw-memory-review-${new Date().toISOString().slice(0, 10)}.json`;
        link.style.display = "none";
        document.body.append(link);
        link.click();
        link.remove();
        window.setTimeout(() => URL.revokeObjectURL(url), 0);
    };

    const saveRetrievalConfig = async (event: FormEvent<HTMLFormElement>) => {
        event.preventDefault();
        setBusy(true);
        setError(null);
        try {
            const saved = await putAdminMemoryRetrievalConfig({
                embedding_model_id: embeddingModelId.trim(),
                reranker_version: "local-hybrid-rrf-v1",
            }, getAccessToken);
            setRetrievalConfig(saved);
            setRebuildStatus("Saved. Rebuild embeddings for assets that do not match this local model.");
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : String(cause));
        } finally {
            setBusy(false);
        }
    };

    const rebuildEmbeddings = async () => {
        setBusy(true);
        setError(null);
        try {
            const result = await rebuildAdminMemoryEmbeddings(getAccessToken);
            setRebuildStatus(`Embedded ${result.embedded} assets with ${result.embedding_model_id}.${result.has_more ? " More assets remain; run rebuild again." : " Index is current."}`);
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : String(cause));
        } finally {
            setBusy(false);
        }
    };

    const deleteAsset = async () => {
        const asset = assets.find((item) => item.asset_id === selectedAsset);
        if (!asset) return;
        if (!window.confirm(
            `Delete "${asset.name}" and its bindings, search rows, embeddings, and derived wiki/code graph rows? Original source files and backups are not deleted. This action cannot be undone.`,
        )) return;
        setBusy(true);
        try {
            await deleteAdminMemoryAsset(asset.asset_id, getAccessToken);
            await refresh();
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : String(cause));
        } finally {
            setBusy(false);
        }
    };

    const retractAssertion = async (assertion: AdminMemoryAssertion) => {
        const reason = retractionReasons[assertion.assertion_id]?.trim();
        if (!reason || !window.confirm(`Retract this memory assertion? Its current projection and superseded ancestors will be removed from retrieval. The review event is permanent.`)) return;
        setBusy(true);
        try {
            await retractAdminMemoryAssertion(assertion.assertion_id, reason, getAccessToken);
            setRetractionReasons((current) => ({ ...current, [assertion.assertion_id]: "" }));
            await refresh();
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : String(cause));
        } finally {
            setBusy(false);
        }
    };

    const correctAssertion = async (assertion: AdminMemoryAssertion) => {
        const reason = retractionReasons[assertion.assertion_id]?.trim();
        if (!reason) return;
        let replacement: unknown;
        try {
            replacement = JSON.parse(correctionValues[assertion.assertion_id] ?? JSON.stringify(assertion.object));
        } catch {
            setError("Replacement value must be valid JSON.");
            return;
        }
        if (!window.confirm("Save this as a new approved assertion that supersedes the selected revision? The source event and correction are permanent.")) return;
        setBusy(true);
        try {
            await correctAdminMemoryAssertion(assertion.assertion_id, replacement, reason, getAccessToken);
            setRetractionReasons((current) => ({ ...current, [assertion.assertion_id]: "" }));
            await refresh();
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : String(cause));
        } finally {
            setBusy(false);
        }
    };

    const forgetSourceEvent = async (source: AdminMemoryAssertion["evidence"][number]) => {
        if (!window.confirm(
            `Forget memory derived from conversation event #${source.event_seq}? Its assertions, superseding revisions, and evidence references will be hidden from memory retrieval. The original conversation event and backups are retained.`,
        )) return;
        setBusy(true);
        setError(null);
        try {
            const report = await forgetAdminMemorySourceEvent(
                source.conversation_id,
                source.event_seq,
                getAccessToken,
            );
            setPrivacyStatus(
                `Forgot memory from event ${report.event_seq}: hid ${report.assertions_hidden} assertions and ${report.evidence_hidden} evidence references. The source event remains in conversation history and backups.`,
            );
            await refresh();
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : String(cause));
        } finally {
            setBusy(false);
        }
    };

    const inspectEvidence = async (assertionId: string, evidenceId: string) => {
        if (visibleSourceId === evidenceId) {
            setVisibleSourceId(null);
            return;
        }
        setVisibleSourceId(evidenceId);
        if (sourceQuotes[evidenceId]) return;
        setSourceLoadingId(evidenceId);
        try {
            const source = await getAdminMemoryEvidenceSource(assertionId, evidenceId, getAccessToken);
            setSourceQuotes((current) => ({ ...current, [evidenceId]: source }));
            setError(null);
        } catch (cause) {
            setVisibleSourceId(null);
            setError(cause instanceof Error ? cause.message : String(cause));
        } finally {
            setSourceLoadingId(null);
        }
    };

    const assetNames = new Map(assets.map((asset) => [asset.asset_id, asset.name]));

    return (
        <section className="execlaw-settings__pane" data-testid="memory-assets-page">
            <h3 className="h5">Memory assets</h3>
            <p className="execlaw-muted small">
                Assign governed assets to the default agent. HOT assets are injected only when their trust floor, lifecycle status, expiry, and turn byte budget permit.
            </p>
            <ErrorBanner message={error} onDismiss={() => setError(null)} className="mb-3" />

            <section className="border rounded p-3 mb-4" aria-labelledby="memory-retrieval-heading">
                <h4 id="memory-retrieval-heading" className="h6">Local hybrid retrieval</h4>
                <p className="execlaw-muted small">
                    Configure an embedding model served by the approved local Standard backend. Embeddings stay in SQLite and are rebuilt from active, source-versioned assets.
                </p>
                <form onSubmit={(event) => void saveRetrievalConfig(event)} className="row g-3 align-items-end">
                    <Form.Group className="col-12 col-lg-5">
                        <Form.Label htmlFor="memory-embedding-model">Embedding model ID</Form.Label>
                        <Form.Control id="memory-embedding-model" value={embeddingModelId} onChange={(event) => setEmbeddingModelId(event.target.value)} maxLength={256} required disabled={busy} placeholder="local embedding model and revision" />
                    </Form.Group>
                    <Form.Group className="col-12 col-lg-4">
                        <Form.Label htmlFor="memory-reranker-version">Reranker</Form.Label>
                        <Form.Control id="memory-reranker-version" value="local-hybrid-rrf-v1" readOnly aria-readonly="true" />
                    </Form.Group>
                    <div className="col-6 col-lg-2">
                        <Button type="submit" variant="primary" disabled={busy || !embeddingModelId.trim()}>Save retrieval config</Button>
                    </div>
                    <div className="col-6 col-lg-1">
                        <Button type="button" variant="outline-primary" disabled={busy || !retrievalConfig} onClick={() => void rebuildEmbeddings()}>Rebuild</Button>
                    </div>
                </form>
                {rebuildStatus && <p className="small mt-2 mb-0" role="status">{rebuildStatus}</p>}
            </section>

            <form onSubmit={(event) => void submitBinding(event)} className="row g-3 align-items-end mb-4">
                <Form.Group className="col-12 col-lg-3">
                    <Form.Label>Agent</Form.Label>
                    <Form.Select value={agentScope} onChange={(event) => setAgentScope(event.target.value)} disabled={busy}>
                        {agentScopes.map((scope) => <option key={scope.id} value={scope.id}>{scope.name}</option>)}
                    </Form.Select>
                </Form.Group>
                <Form.Group className="col-12 col-lg-3">
                    <Form.Label>Asset</Form.Label>
                    <Form.Select value={selectedAsset} onChange={(event) => setSelectedAsset(event.target.value)} disabled={busy || assets.length === 0}>
                        {assets.length === 0 && <option value="">No registered assets</option>}
                        {assets.map((asset) => <option key={asset.asset_id} value={asset.asset_id}>{asset.name} · {asset.asset_type} · {asset.status}</option>)}
                    </Form.Select>
                </Form.Group>
                <Form.Group className="col-6 col-lg-2">
                    <Form.Label>Injection</Form.Label>
                    <Form.Select value={mode} onChange={(event) => setMode(event.target.value as AdminMemoryAssetBinding["injection_mode"])} disabled={busy}>
                        <option value="hot">HOT context</option>
                        <option value="discoverable">Discoverable</option>
                        <option value="tool_only">Tool only</option>
                    </Form.Select>
                </Form.Group>
                <Form.Group className="col-6 col-lg-2">
                    <Form.Label>Priority</Form.Label>
                    <Form.Control type="number" min={-1000} max={1000} value={priority} onChange={(event) => setPriority(Number(event.target.value))} disabled={busy} />
                </Form.Group>
                <Form.Group className="col-6 col-lg-2">
                    <Form.Label>Character budget</Form.Label>
                    <Form.Control type="number" min={1} max={2048} value={maxChars} onChange={(event) => setMaxChars(Number(event.target.value))} disabled={busy || mode !== "hot"} />
                </Form.Group>
                <div className="col-6 col-lg-2">
                    <Button type="submit" variant="primary" disabled={busy || !selectedAsset}>Bind asset</Button>
                </div>
                <div className="col-6 col-lg-2">
                    <Button type="button" variant="outline-danger" disabled={busy || !selectedAsset} onClick={() => void deleteAsset()}>Delete asset</Button>
                </div>
            </form>

            <h4 className="h6">Bindings for {agentScopes.find((scope) => scope.id === agentScope)?.name ?? agentScope}</h4>
            {bindings.length === 0 ? (
                <p className="execlaw-muted small" data-testid="memory-bindings-empty">No assets are bound.</p>
            ) : (
                <ul className="list-unstyled mb-0">
                    {bindings.map((binding) => (
                        <li key={`${binding.agent_scope}:${binding.asset_id}`} className="execlaw-list-row d-flex align-items-center gap-3 py-2" data-testid="memory-binding-row">
                            <div className="flex-grow-1">
                                <strong>{assetNames.get(binding.asset_id) ?? binding.asset_id}</strong>
                                <div className="execlaw-muted small">{binding.injection_mode} · priority {binding.priority} · {binding.max_chars} chars</div>
                            </div>
                            <Button size="sm" variant="outline-danger" disabled={busy} onClick={() => void removeBinding(binding.asset_id)} aria-label={`Unbind ${assetNames.get(binding.asset_id) ?? binding.asset_id}`}>
                                <i className="bi bi-link-45deg me-1" aria-hidden />Unbind
                            </Button>
                        </li>
                    ))}
                </ul>
            )}
            <hr className="my-4" />
            <h4 className="h6">Memory assertion evidence</h4>
            <Button size="sm" variant="outline-secondary" className="mb-2" disabled={assertions.length === 0} onClick={exportSafeMemory}>Export loaded memory review data</Button>
            {privacyStatus && <p className="small mt-2" role="status">{privacyStatus}</p>}
            <p className="execlaw-muted small">Assertions are append-only. Review status, validity, revision lineage, and the event/path/hash references that support each fact.</p>
            {assertions.length === 0 ? <p className="execlaw-muted small">No memory assertions have been recorded.</p> : (
                <div className="d-flex flex-column gap-2">
                    {assertions.map((item) => <article key={item.assertion_id} className="border rounded p-3" data-testid="memory-assertion-row">
                        <div className="d-flex flex-wrap justify-content-between gap-2"><strong>{item.subject} · {item.predicate}</strong><span className={`badge ${item.status === "approved" ? "text-bg-success" : item.status === "proposed" ? "text-bg-warning" : "text-bg-secondary"}`}>{item.status}</span></div>
                        <pre className="small mb-2 mt-2 text-break">{JSON.stringify(item.object, null, 2)}</pre>
                        <div className="execlaw-muted small">{item.kind} · {item.trust_class} · confidence {item.confidence.toFixed(2)} · scope {item.scope}</div>
                        <div className="execlaw-muted small">Observed {item.observed_from}{item.observed_to === null ? " onward" : `–${item.observed_to}`} · valid from {new Date(item.valid_from * 1000).toLocaleString()}{item.valid_to === null ? " onward" : ` until ${new Date(item.valid_to * 1000).toLocaleString()}`}</div>
                        {item.supersedes_id && <div className="small">Supersedes <code>{item.supersedes_id}</code></div>}
                        <div className="small">Assertion <code>{item.assertion_id}</code> · extraction run <code>{item.extraction_run_id}</code></div>
                        <div className="small mt-1">Linked memory assets: {assets.filter((asset) => (asset.assertion_ids ?? []).includes(item.assertion_id)).map((asset) => asset.name).join(", ") || "none"}</div>
                        {item.status === "approved" && item.evidence_total > 0 && (() => {
                            const compatible = assets.filter((asset) => asset.asset_type === "memory" && asset.status === "active" && asset.owner_scope === item.scope && asset.trust_floor === item.trust_class && !(asset.assertion_ids ?? []).includes(item.assertion_id));
                            return compatible.length > 0 && <div className="d-flex gap-2 mt-2">
                                <Form.Select aria-label={`Memory asset for assertion ${item.assertion_id}`} value={linkTargets[item.assertion_id] ?? ""} onChange={(event) => setLinkTargets((current) => ({ ...current, [item.assertion_id]: event.target.value }))} disabled={busy}>
                                    <option value="">Link to derived memory asset</option>
                                    {compatible.map((asset) => <option key={asset.asset_id} value={asset.asset_id}>{asset.name}</option>)}
                                </Form.Select>
                                <Button size="sm" variant="outline-primary" disabled={busy || !linkTargets[item.assertion_id]} onClick={() => void linkAssertion(item)}>Link evidence</Button>
                            </div>;
                        })()}
                        {item.review && <div className="alert alert-secondary py-2 small mt-2 mb-0">{item.review.decision === "retracted" ? "Retracted" : "Corrected"} by <code>{item.review.reviewer_id}</code>: {item.review.reason} · <a href={`/chat/${encodeURIComponent(item.review.conversation_id)}`}>Review event #{item.review.event_seq}</a></div>}
                        <div className="mt-2">{item.evidence.length === 0 ? <span className="text-danger small">No evidence references are attached.</span> : <><div className="small text-muted">Showing {item.evidence.length} of {item.evidence_total} evidence references.</div>{item.evidence.map((source) => <div key={source.evidence_id} className="small border-top pt-2 mt-2">
                            <span className="badge text-bg-light me-2">{source.evidence_kind}</span>
                            <a href={`/chat/${encodeURIComponent(source.conversation_id)}`}>Conversation event #{source.event_seq}</a>
                            <Button size="sm" variant="link" className="p-0 ms-2" aria-expanded={visibleSourceId === source.evidence_id} onClick={() => void inspectEvidence(item.assertion_id, source.evidence_id)}>{visibleSourceId === source.evidence_id ? "Hide source span" : "Show source span"}</Button>
                            <Button size="sm" variant="outline-danger" className="ms-2" disabled={busy} onClick={() => void forgetSourceEvent(source)} aria-label={`Forget memory from conversation event ${source.event_seq}`}>Forget memory source</Button>
                            {visibleSourceId === source.evidence_id && (sourceLoadingId === source.evidence_id ? <div className="small text-muted">Verifying source event…</div> : sourceQuotes[source.evidence_id] && <div className="mt-2"><span className={`badge ${sourceQuotes[source.evidence_id].integrity_verified ? "text-bg-success" : "text-bg-warning"}`}>{sourceQuotes[source.evidence_id].integrity_verified ? "Event integrity verified" : "Event integrity unavailable"}</span><div className="small text-muted mt-1">Original source content is untrusted evidence. {sourceQuotes[source.evidence_id].truncated && `Showing the first ${sourceQuotes[source.evidence_id].source_quote.length} characters of ${sourceQuotes[source.evidence_id].source_quote_bytes} bytes.`}</div><pre className="small text-break p-2 bg-body-tertiary rounded mt-1">{sourceQuotes[source.evidence_id].source_quote}</pre></div>)}
                            <span className="d-block text-muted">Path <code>{source.payload_path}</code> · SHA-256 <code>{source.quote_hash}</code></span>
                        </div>)}</>}</div>
                        <div className="mt-3">
                            <label className="form-label small mb-1" htmlFor={`review-reason-${item.assertion_id}`}>Reason for correction or retraction</label>
                            <input id={`review-reason-${item.assertion_id}`} className="form-control form-control-sm" maxLength={2000} value={retractionReasons[item.assertion_id] ?? ""} onChange={(event) => setRetractionReasons((current) => ({ ...current, [item.assertion_id]: event.target.value }))} disabled={busy} />
                            <details className="mt-2"><summary>Replace assertion value</summary><label className="visually-hidden" htmlFor={`correction-value-${item.assertion_id}`}>Replacement value as JSON</label><textarea id={`correction-value-${item.assertion_id}`} className="form-control form-control-sm mt-2 font-monospace" rows={4} value={correctionValues[item.assertion_id] ?? JSON.stringify(item.object, null, 2)} onChange={(event) => setCorrectionValues((current) => ({ ...current, [item.assertion_id]: event.target.value }))} disabled={busy} /><Button size="sm" variant="outline-primary" className="mt-2" disabled={busy || !(retractionReasons[item.assertion_id] ?? "").trim()} onClick={() => void correctAssertion(item)}>Save as corrected revision</Button></details>
                            {item.status !== "retracted" && <Button size="sm" variant="outline-danger" className="mt-2 ms-2" disabled={busy || !(retractionReasons[item.assertion_id] ?? "").trim()} onClick={() => void retractAssertion(item)}>Retract assertion</Button>}
                        </div>
                    </article>)}
                </div>
            )}
        </section>
    );
}
