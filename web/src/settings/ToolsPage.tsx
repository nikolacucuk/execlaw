// Settings → Tools (Phase 8a per-tool trust-class allowlist).
//
// Lists every tool the runner might dispatch — builtins, plugin
// tools, and (Phase 8b+) MCP-server tools — with a per-row toggle
// for `enabled` and a multi-select for the trust-class allowlist.
// Mutations go through PATCH /api/admin/tools/{tool_name} (Controller-
// only on the server side; the SPA hides the controls when the
// caller isn't a Controller).

import { useCallback, useEffect, useState } from "react";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";
import {
    listTools,
    listSafetyProfiles,
    updateToolPolicy,
    setApprovedIntegrationTools,
    simulateToolPolicy,
    listToolPolicyRevisions,
    rollbackToolPolicyRevision,
    type ToolPolicySimulationReport,
    type ToolPolicyRevision,
    type ToolView,
    type SafetyProfileView,
} from "../api/endpoints";
import { useAuth } from "../auth/AuthContext";
import { ErrorBanner } from "../components/ErrorBanner";

const TRUST_CLASSES: ReadonlyArray<string> = [
    "Controller",
    "Delegated",
    "KnownTrusted",
    "KnownLimited",
    "UnknownPending",
    "Blocked",
];
const TRUST_RANK: Readonly<Record<string, number>> = {
    Controller: 5,
    Delegated: 4,
    KnownTrusted: 3,
    KnownLimited: 2,
    UnknownPending: 1,
    Blocked: 0,
};

const SOURCE_BADGE: Record<ToolView["source"], string> = {
    builtin: "is-known",
    plugin: "is-controller",
    mcp: "is-limited",
};

export function ToolsPage() {
    const auth = useAuth();
    const getToken = auth.getAccessToken;
    const [tools, setTools] = useState<ToolView[] | null>(null);
    const [safetyProfiles, setSafetyProfiles] = useState<SafetyProfileView[] | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [safetyProfileError, setSafetyProfileError] = useState<string | null>(null);
    const [busyTool, setBusyTool] = useState<string | null>(null);
    const [busySafetyProfile, setBusySafetyProfile] = useState(false);
    const [draftPolicies, setDraftPolicies] = useState<Record<string, { enabled: boolean; allowed_classes: string[] }>>({});
    const [candidateFloors, setCandidateFloors] = useState<Record<string, string>>({});
    const [simulations, setSimulations] = useState<Record<string, ToolPolicySimulationReport>>({});
    const [revisions, setRevisions] = useState<Record<string, ToolPolicyRevision[]>>({});
    const [historyTool, setHistoryTool] = useState<string | null>(null);

    const refresh = useCallback(async () => {
        try {
            const r = await listTools(getToken);
            setTools(r.tools);
            setError(null);
        } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
        }
    }, [getToken]);

    useEffect(() => {
        void refresh();
    }, [refresh]);

    const refreshSafetyProfiles = useCallback(async () => {
        try {
            const result = await listSafetyProfiles(getToken);
            setSafetyProfiles(Array.isArray(result.profiles) ? result.profiles : []);
            setSafetyProfileError(null);
        } catch (e) {
            setSafetyProfiles([]);
            setSafetyProfileError(e instanceof Error ? e.message : String(e));
        }
    }, [getToken]);

    useEffect(() => {
        void refreshSafetyProfiles();
    }, [refreshSafetyProfiles]);

    const meRole = auth.user?.role ?? "viewer";
    const canMutate = meRole === "controller";

    const onToggleEnabled = useCallback((tool: ToolView) => {
        setDraftPolicies((current) => {
            const prior = current[tool.tool_name] ?? {
                enabled: tool.enabled,
                allowed_classes: tool.allowed_classes,
            };
            return { ...current, [tool.tool_name]: { ...prior, enabled: !prior.enabled } };
        });
        setSimulations((current) => {
            const next = { ...current };
            delete next[tool.tool_name];
            return next;
        });
    }, []);

    const onToggleClass = useCallback((tool: ToolView, cls: string) => {
        setDraftPolicies((current) => {
            const prior = current[tool.tool_name] ?? {
                enabled: tool.enabled,
                allowed_classes: tool.allowed_classes,
            };
            const allowed_classes = prior.allowed_classes.includes(cls)
                ? prior.allowed_classes.filter((value) => value !== cls)
                : [...prior.allowed_classes, cls];
            return { ...current, [tool.tool_name]: { ...prior, allowed_classes } };
        });
        setSimulations((current) => {
            const next = { ...current };
            delete next[tool.tool_name];
            return next;
        });
    }, []);

    const onSimulate = useCallback(async (tool: ToolView) => {
        const candidate = draftPolicies[tool.tool_name] ?? {
            enabled: tool.enabled,
            allowed_classes: tool.allowed_classes,
        };
        setBusyTool(tool.tool_name);
        try {
            const report = await simulateToolPolicy(
                tool.tool_name,
                { ...candidate, trust_floor: candidateFloors[tool.tool_name] || tool.trust_floor },
                getToken,
            );
            setSimulations((current) => ({ ...current, [tool.tool_name]: report }));
            setError(null);
        } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
        } finally {
            setBusyTool(null);
        }
    }, [candidateFloors, draftPolicies, getToken]);

    const onCandidateFloorChange = useCallback((tool: ToolView, floor: string) => {
        setCandidateFloors((current) => ({ ...current, [tool.tool_name]: floor }));
        const allowed_classes = floor
            ? TRUST_CLASSES.filter((cls) => cls !== "Blocked" && cls !== "UnknownPending" && TRUST_RANK[cls] >= TRUST_RANK[floor])
            : tool.allowed_classes;
        setDraftPolicies((current) => ({
            ...current,
            [tool.tool_name]: {
                ...(current[tool.tool_name] ?? { enabled: tool.enabled, allowed_classes: tool.allowed_classes }),
                allowed_classes,
            },
        }));
        setSimulations((current) => {
            const next = { ...current };
            delete next[tool.tool_name];
            return next;
        });
    }, []);

    const onSavePolicy = useCallback(async (tool: ToolView) => {
        const candidate = draftPolicies[tool.tool_name];
        if (!candidate || !simulations[tool.tool_name]) return;
        setBusyTool(tool.tool_name);
        try {
            await updateToolPolicy(tool.tool_name, candidate, getToken);
            await refresh();
            setDraftPolicies((current) => {
                const next = { ...current };
                delete next[tool.tool_name];
                return next;
            });
            setSimulations((current) => {
                const next = { ...current };
                delete next[tool.tool_name];
                return next;
            });
            setError(null);
        } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
        } finally {
            setBusyTool(null);
        }
    }, [draftPolicies, getToken, refresh, simulations]);

    const onLoadRevisions = useCallback(async (tool: ToolView) => {
        if (historyTool === tool.tool_name) {
            setHistoryTool(null);
            return;
        }
        setBusyTool(tool.tool_name);
        try {
            const result = await listToolPolicyRevisions(tool.tool_name, getToken);
            setRevisions((current) => ({ ...current, [tool.tool_name]: result.revisions }));
            setHistoryTool(tool.tool_name);
        } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
        } finally {
            setBusyTool(null);
        }
    }, [getToken, historyTool]);

    const onRollback = useCallback(async (tool: ToolView, revision: number) => {
        setBusyTool(tool.tool_name);
        try {
            await rollbackToolPolicyRevision(tool.tool_name, revision, getToken);
            await refresh();
            const result = await listToolPolicyRevisions(tool.tool_name, getToken);
            setRevisions((current) => ({ ...current, [tool.tool_name]: result.revisions }));
            setDraftPolicies((current) => {
                const next = { ...current };
                delete next[tool.tool_name];
                return next;
            });
            setSimulations((current) => {
                const next = { ...current };
                delete next[tool.tool_name];
                return next;
            });
        } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
        } finally {
            setBusyTool(null);
        }
    }, [getToken, refresh]);

    const onToggleApprovedIntegration = useCallback(
        async (tool: ToolView) => {
            const profile = safetyProfiles?.find(
                (item) => item.profile.profile_id === "approved_integration",
            );
            if (!profile || !canMutate) return;
            const approved = profile.profile.approved_tools.includes(tool.tool_name)
                ? profile.profile.approved_tools.filter((name) => name !== tool.tool_name)
                : [...profile.profile.approved_tools, tool.tool_name];
            setBusySafetyProfile(true);
            try {
                await setApprovedIntegrationTools(approved, getToken);
                await refreshSafetyProfiles();
            } catch (e) {
                setSafetyProfileError(e instanceof Error ? e.message : String(e));
            } finally {
                setBusySafetyProfile(false);
            }
        },
        [canMutate, getToken, refreshSafetyProfiles, safetyProfiles],
    );

    return (
        <div data-testid="settings-tools">
            <div className="d-flex align-items-center mb-3">
                <h3 className="h6 mb-0 flex-grow-1">Tools</h3>
                <Button
                    size="sm"
                    variant="outline-secondary"
                    onClick={() => void refresh()}
                    data-testid="tools-refresh"
                >
                    <i className="bi bi-arrow-clockwise me-1" aria-hidden />
                    Refresh
                </Button>
            </div>

            {!canMutate && (
                <div className="execlaw-muted small mb-3">
                    Read-only view. Only Controllers can change tool access policy.
                </div>
            )}

            <ErrorBanner message={error} onDismiss={() => setError(null)} className="mb-3" />

            <div className="execlaw-card mb-3" data-testid="safety-profiles-settings">
                <div className="d-flex align-items-center mb-2">
                    <strong className="flex-grow-1">Task safety profiles</strong>
                    <Button
                        size="sm"
                        variant="outline-secondary"
                        onClick={() => void refreshSafetyProfiles()}
                        data-testid="safety-profiles-refresh"
                    >
                        Refresh
                    </Button>
                </div>
                <ErrorBanner
                    message={safetyProfileError}
                    onDismiss={() => setSafetyProfileError(null)}
                    className="mb-2"
                />
                {safetyProfiles === null ? (
                    <div className="execlaw-muted small">Loading safety profilesâ€¦</div>
                ) : (
                    safetyProfiles.map((profile) => (
                        <section
                            key={profile.profile.profile_id}
                            className="border-top pt-2 mt-2"
                            data-testid="safety-profile-row"
                            data-profile-id={profile.profile.profile_id}
                        >
                            <div className="d-flex align-items-center gap-2">
                                <strong>{profile.profile.display_name}</strong>
                                <span className="execlaw-muted small">revision {profile.profile.revision}</span>
                                {!profile.supported && (
                                    <span className="badge text-bg-warning" title={profile.unsupported_reason ?? undefined}>
                                        unavailable here
                                    </span>
                                )}
                            </div>
                            <div className="execlaw-muted small mt-1" data-testid="safety-profile-permissions">
                                Filesystem: {profile.permissions.filesystem}; process: {profile.permissions.process}; network: {profile.permissions.network}; secrets: {profile.permissions.secrets}; destinations: {profile.permissions.destinations}.
                            </div>
                            {!profile.supported && profile.unsupported_reason && (
                                <div className="text-warning small mt-1" role="status">
                                    {profile.unsupported_reason}
                                </div>
                            )}
                            {profile.profile.profile_id === "approved_integration" && (
                                <div className="mt-2">
                                    <div className="execlaw-muted small mb-1">
                                        Approved integration tools for this task profile:
                                    </div>
                                    {(tools ?? []).map((tool) => (
                                            <Form.Check
                                                key={tool.tool_name}
                                                type="checkbox"
                                                label={<><code>{tool.tool_name}</code>{tool.description ? ` — ${tool.description}` : ""}</>}
                                                checked={profile.profile.approved_tools.includes(tool.tool_name)}
                                                disabled={!canMutate || busySafetyProfile || !tool.enabled || tool.removed_at !== null || !tool.allowed_classes.includes("Controller")}
                                                onChange={() => void onToggleApprovedIntegration(tool)}
                                                data-testid="approved-integration-tool"
                                                data-tool-name={tool.tool_name}
                                            />
                                        ))}
                                    {(tools ?? []).length === 0 && (
                                        <div className="execlaw-muted small">No tools are registered.</div>
                                    )}
                                    {profile.effective_tools.length > 0 && (
                                        <div className="execlaw-muted small mt-1">
                                            Effective now: {profile.effective_tools.join(", ")}
                                        </div>
                                    )}
                                </div>
                            )}
                        </section>
                    ))
                )}
            </div>

            {tools === null ? (
                <div className="execlaw-muted small">Loading tools…</div>
            ) : tools.length === 0 ? (
                <div className="execlaw-muted small">
                    No tools registered yet. Install a plugin or wire up
                    an MCP server to populate the list.
                </div>
            ) : (
                tools.map((t) => {
                    const policy = draftPolicies[t.tool_name] ?? {
                        enabled: t.enabled,
                        allowed_classes: t.allowed_classes,
                    };
                    const hasDraft = !!draftPolicies[t.tool_name];
                    const simulation = simulations[t.tool_name];
                    return (
                    <div
                        className="execlaw-card"
                        key={t.tool_name}
                        data-testid="tool-row"
                        data-tool-name={t.tool_name}
                    >
                        <div className="d-flex align-items-center gap-2 mb-1">
                            <span className="execlaw-card__title flex-grow-1">
                                <code>{t.tool_name}</code>
                                <span
                                    className={`execlaw-trust-badge ms-2 ${SOURCE_BADGE[t.source]}`}
                                >
                                    {t.source}
                                </span>
                                {t.removed_at !== null && (
                                    <span className="execlaw-trust-badge ms-2 is-limited">
                                        removed
                                    </span>
                                )}
                                {!policy.enabled && (
                                    <span className="execlaw-trust-badge ms-2 is-limited">
                                        disabled
                                    </span>
                                )}
                            </span>
                            {canMutate && (
                                <Form.Check
                                    type="switch"
                                    id={`enabled-${t.tool_name}`}
                                    label="enabled"
                                    checked={policy.enabled}
                                    disabled={busyTool === t.tool_name}
                                    onChange={() => void onToggleEnabled(t)}
                                    data-testid="tool-enabled-toggle"
                                />
                            )}
                        </div>
                        {t.description && (
                            <div className="execlaw-muted small mb-2">
                                {t.description}
                            </div>
                        )}
                        <div className="d-flex flex-wrap gap-2 align-items-center">
                            <span className="execlaw-muted small me-2">
                                Allowed:
                            </span>
                            {TRUST_CLASSES.map((cls) => {
                                const checked = policy.allowed_classes.includes(cls);
                                return (
                                    <Form.Check
                                        key={cls}
                                        type="checkbox"
                                        inline
                                        id={`${t.tool_name}-${cls}`}
                                        label={cls}
                                        checked={checked}
                                        disabled={
                                            !canMutate || busyTool === t.tool_name
                                        }
                                        onChange={() => void onToggleClass(t, cls)}
                                        data-testid="tool-class-checkbox"
                                        data-class={cls}
                                    />
                                );
                            })}
                        </div>
                        {canMutate && (
                            <div className="mt-2" data-testid="candidate-trust-floor-control">
                                <Form.Label htmlFor={`candidate-floor-${t.tool_name}`} className="small execlaw-muted mb-1">
                                    Simulated trust floor
                                </Form.Label>
                                <Form.Select
                                    id={`candidate-floor-${t.tool_name}`}
                                    size="sm"
                                    value={candidateFloors[t.tool_name] ?? t.trust_floor ?? ""}
                                    disabled={busyTool === t.tool_name}
                                    onChange={(event) => onCandidateFloorChange(t, event.target.value)}
                                    data-testid="candidate-trust-floor"
                                >
                                    <option value="">Use current manifest floor and allowlist</option>
                                    {TRUST_CLASSES
                                        .filter((cls) => cls !== "UnknownPending" && cls !== "Blocked")
                                        .map((cls) => (
                                            <option
                                                key={cls}
                                                value={cls}
                                                disabled={!!t.trust_floor && TRUST_RANK[cls] < TRUST_RANK[t.trust_floor]}
                                            >
                                                {cls}
                                            </option>
                                        ))}
                                </Form.Select>
                                {t.trust_floor && (
                                    <div className="execlaw-muted small mt-1">
                                        The manifest floor {t.trust_floor} remains a hard lower bound.
                                    </div>
                                )}
                            </div>
                        )}
                        {t.trust_floor && (
                            <div className="execlaw-muted small mt-2" data-testid="tool-trust-floor">
                                Manifest trust floor: {t.trust_floor}
                            </div>
                        )}
                        {canMutate && (
                            <div className="d-flex gap-2 mt-2">
                                <Button
                                    size="sm"
                                    variant="outline-primary"
                                    disabled={busyTool === t.tool_name}
                                    onClick={() => void onSimulate(t)}
                                    data-testid="simulate-tool-policy"
                                >
                                    Simulate
                                </Button>
                                {hasDraft && (
                                    <>
                                        <Button
                                            size="sm"
                                            variant="primary"
                                            disabled={busyTool === t.tool_name || !simulation}
                                            onClick={() => void onSavePolicy(t)}
                                            data-testid="save-tool-policy"
                                        >
                                            Apply simulated policy
                                        </Button>
                                        <Button
                                            size="sm"
                                            variant="outline-secondary"
                                            disabled={busyTool === t.tool_name}
                                            onClick={() => {
                                                setDraftPolicies((current) => {
                                                    const next = { ...current };
                                                    delete next[t.tool_name];
                                                    return next;
                                                });
                                                setSimulations((current) => {
                                                    const next = { ...current };
                                                    delete next[t.tool_name];
                                                    return next;
                                                });
                                            }}
                                            data-testid="discard-tool-policy"
                                        >
                                            Discard
                                        </Button>
                                    </>
                                )}
                                <Button
                                    size="sm"
                                    variant="outline-secondary"
                                    disabled={busyTool === t.tool_name}
                                    onClick={() => void onLoadRevisions(t)}
                                    data-testid="tool-policy-history"
                                >
                                    {historyTool === t.tool_name ? "Hide history" : "History"}
                                </Button>
                            </div>
                        )}
                        {simulation && (
                            <div className="mt-2" role="status" data-testid="tool-policy-simulation">
                                <div className="execlaw-muted small">
                                    No tools were executed. Evaluated {simulation.report.evaluated_decisions} saved decisions and {simulation.trust_class_fixtures} trust-class fixtures.
                                </div>
                                <div className="small mt-1">
                                    Newly allowed: {simulation.report.newly_allowed.length}; newly denied: {simulation.report.newly_denied.length}; newly approval-gated: {simulation.report.newly_approval_gated.length}.
                                </div>
                                {[...simulation.report.newly_allowed, ...simulation.report.newly_denied, ...simulation.report.newly_approval_gated].slice(0, 10).map((change) => (
                                    <div key={`${change.decision_id}-${change.change}`} className="execlaw-muted small">
                                        {change.caller_trust}: {change.change} â€” {change.explanation}
                                    </div>
                                ))}
                                {simulation.report.omitted_changes > 0 && (
                                    <div className="execlaw-muted small">{simulation.report.omitted_changes} additional changes omitted.</div>
                                )}
                            </div>
                        )}
                        {historyTool === t.tool_name && (
                            <div className="mt-2" data-testid="tool-policy-revisions">
                                <div className="execlaw-muted small mb-1">Rollback creates a new revision; expired approval tokens remain expired.</div>
                                {(revisions[t.tool_name] ?? []).map((revision) => (
                                    <div key={revision.revision_id} className="d-flex align-items-center gap-2 small">
                                        <span>Revision {revision.revision_id} Â· {revision.revised_by} Â· {new Date(revision.revised_at * 1000).toLocaleString()}</span>
                                        <Button
                                            size="sm"
                                            variant="outline-warning"
                                            disabled={busyTool === t.tool_name}
                                            onClick={() => void onRollback(t, revision.revision_id)}
                                            data-testid="rollback-tool-policy"
                                        >
                                            Restore
                                        </Button>
                                    </div>
                                ))}
                            </div>
                        )}
                    </div>
                    );
                })
            )}
        </div>
    );
}
