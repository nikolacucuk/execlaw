// Settings-style page body listing every pending cold-contact
// approval. Each row is a card with the message preview + action
// buttons. Polls `/api/admin/approvals` every 4s while mounted so a
// freshly-arrived approval surfaces without a manual refresh.

import { useCallback, useEffect, useState } from "react";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";
import {
    decideMemoryPromotion,
    listPendingApprovals,
    respondApproval,
    type ApprovalVerb,
    type MemoryPromotionSummary,
    type PendingApprovalSummary,
} from "../api/endpoints";
import { useAuth } from "../auth/AuthContext";
import { ErrorBanner } from "../components/ErrorBanner";

const POLL_INTERVAL_MS = 4_000;

interface ActionButton {
    verb: ApprovalVerb;
    label: string;
    icon: string;
    variant: string;
    title: string;
}

const ACTIONS: ReadonlyArray<ActionButton> = [
    {
        verb: "trust",
        label: "Trust",
        icon: "bi-shield-check",
        variant: "outline-success",
        title: "Admit as KnownTrusted: full safe-tools + memory access. Agent replies to the queued message.",
    },
    {
        verb: "trust_limited",
        label: "Limited",
        icon: "bi-shield-shaded",
        variant: "outline-warning",
        title: "Admit as KnownLimited with the selected topic scope. Agent replays the queued message.",
    },
    {
        verb: "claim_as_me",
        label: "This is me",
        icon: "bi-person-check",
        variant: "outline-primary",
        title: "Adds this handle to your My identities. Future inbound from this number resolves to you. Replays the queued message as a Controller turn.",
    },
    {
        verb: "ignore_once",
        label: "Ignore once",
        icon: "bi-shield-slash",
        variant: "outline-secondary",
        title: "Drop this message; re-prompt on the next inbound from this handle.",
    },
    {
        verb: "block",
        label: "Block",
        icon: "bi-shield-x",
        variant: "outline-danger",
        title: "Block universally — future inbound silently audit-logged.",
    },
];

const CHAIN_ACTIONS: ReadonlyArray<ActionButton> = [
    { verb: "approve", label: "Approve effect", icon: "bi-check-lg", variant: "outline-success", title: "Approve this exact signed effect" },
    { verb: "reject", label: "Reject", icon: "bi-x-lg", variant: "outline-danger", title: "Reject this pending effect" },
];

export function ApprovalsPage() {
    const auth = useAuth();
    const getToken = auth.getAccessToken;
    const [approvals, setApprovals] = useState<PendingApprovalSummary[] | null>(null);
    const [promotions, setPromotions] = useState<MemoryPromotionSummary[]>([]);
    const [error, setError] = useState<string | null>(null);
    const [busyApproval, setBusyApproval] = useState<string | null>(null);
    const [limitedScopes, setLimitedScopes] = useState<Record<string, string>>({});

    const refresh = useCallback(async () => {
        try {
            const r = await listPendingApprovals(getToken);
            setApprovals(r.approvals);
            setPromotions(r.memory_promotions ?? []);
            setError(null);
        } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
        }
    }, [getToken]);

    useEffect(() => {
        void refresh();
        const id = window.setInterval(() => {
            void refresh();
        }, POLL_INTERVAL_MS);
        return () => window.clearInterval(id);
    }, [refresh]);

    const onRespond = useCallback(
        async (approval: PendingApprovalSummary, verb: ApprovalVerb) => {
            const approvalId = approval.approval_id;
            setBusyApproval(approvalId);
            try {
                const allowedTopics = (limitedScopes[approvalId] ?? "")
                    .split(",")
                    .map((topic) => topic.trim())
                    .filter(Boolean);
                await respondApproval(approvalId, {
                    verb,
                    ...(verb === "trust_limited" ? { allowed_topics: allowedTopics } : {}),
                    approval_token: approval.approval_token,
                }, getToken);
                // Optimistic: drop this approval from the list. The
                // next poll re-confirms.
                setApprovals((prev) =>
                    prev ? prev.filter((a) => a.approval_id !== approvalId) : prev,
                );
                setError(null);
            } catch (e) {
                setError(e instanceof Error ? e.message : String(e));
            } finally {
                setBusyApproval(null);
                // Re-fetch so any server-side changes (e.g. claim_as_me
                // merging multiple approvals into one) are reflected.
                void refresh();
            }
        },
        [getToken, limitedScopes, refresh],
    );

    const onDecidePromotion = useCallback(
        async (proposalId: number, decision: "approve" | "reject") => {
            const busyKey = `memory:${proposalId}`;
            setBusyApproval(busyKey);
            try {
                await decideMemoryPromotion(proposalId, decision, getToken);
                setPromotions((prev) => prev.filter((proposal) => proposal.id !== proposalId));
                setError(null);
            } catch (e) {
                setError(e instanceof Error ? e.message : String(e));
            } finally {
                setBusyApproval(null);
                void refresh();
            }
        },
        [getToken, refresh],
    );

    if (approvals === null) {
        return (
            <div data-testid="approvals-page-body">
                <ErrorBanner
                    message={error}
                    onDismiss={() => setError(null)}
                    className="mb-3"
                />
                <div className="execlaw-muted small">Loading approvals…</div>
            </div>
        );
    }

    return (
        <div data-testid="approvals-page-body">
            <p className="execlaw-muted small mb-3">
                Cold contacts waiting on a trust decision. Approving an
                entry replays the queued first message through the agent
                — you don&apos;t need to ask the contact to re-send.
            </p>

            <ErrorBanner
                message={error}
                onDismiss={() => setError(null)}
                className="mb-3"
            />

            {approvals.length === 0 && promotions.length === 0 ? (
                <div
                    className="execlaw-card text-center execlaw-muted small"
                    data-testid="approvals-empty"
                    style={{ padding: "2rem" }}
                >
                    <i
                        className="bi bi-shield-check d-block mb-2"
                        style={{ fontSize: "1.5rem" }}
                        aria-hidden
                    />
                    No pending approvals. New trust or memory decisions will
                    appear here when they need review.
                </div>
            ) : (
                <>
                    {promotions.length > 0 && (
                        <section className="mb-4" aria-labelledby="memory-promotion-heading">
                            <h3 id="memory-promotion-heading" className="h6">Memory tier proposals</h3>
                            <ul className="list-unstyled mb-0">
                                {promotions.map((proposal) => (
                                    <li key={proposal.id} className="execlaw-card mb-3" data-testid="memory-promotion-row">
                                        <div className="d-flex align-items-start gap-2 mb-3">
                                            <i className="bi bi-memory execlaw-muted" aria-hidden />
                                            <div>
                                                <div><code>{proposal.key}</code> <span className="execlaw-muted">({proposal.scope}, {proposal.trust_class})</span></div>
                                                <div className="small mt-1">{proposal.from_tier} → {proposal.to_tier} · {proposal.reason} · proposed by {proposal.proposed_by}</div>
                                                <div className="execlaw-muted small mt-1">Source: <code>{proposal.evidence_ref}</code></div>
                                            </div>
                                        </div>
                                        <div className="d-flex gap-2">
                                            <Button size="sm" variant="outline-success" disabled={busyApproval !== null} onClick={() => void onDecidePromotion(proposal.id, "approve")} data-testid="memory-promotion-approve">
                                                <i className="bi bi-check-lg me-2" aria-hidden />Approve
                                            </Button>
                                            <Button size="sm" variant="outline-danger" disabled={busyApproval !== null} onClick={() => void onDecidePromotion(proposal.id, "reject")} data-testid="memory-promotion-reject">
                                                <i className="bi bi-x-lg me-2" aria-hidden />Reject
                                            </Button>
                                        </div>
                                    </li>
                                ))}
                            </ul>
                        </section>
                    )}
                    {approvals.length > 0 && (
                    <ul className="list-unstyled mb-0">
                    {approvals.map((a) => (
                        <li
                            key={a.approval_id}
                            className="execlaw-card mb-3"
                            data-testid="approval-row"
                        >
                            <div className="d-flex align-items-start gap-2 mb-2">
                                <i
                                    className="bi bi-shield-exclamation execlaw-muted"
                                    style={{ fontSize: "1.25rem" }}
                                    aria-hidden
                                />
                                <div className="flex-grow-1">
                                    <div className="execlaw-muted small mb-1">
                                        {a.kind === "effectful_chain" ? "Effect approval" : "Sender:"} <code>{a.sender_principal_id}</code>
                                    </div>
                                    <dl className="row small mb-2" data-testid="approval-context">
                                        <dt className="col-sm-2">Scope</dt>
                                        <dd className="col-sm-10">{a.scope}</dd>
                                        <dt className="col-sm-2">Reason</dt>
                                        <dd className="col-sm-10">{a.reason}</dd>
                                        <dt className="col-sm-2">Requested action</dt>
                                        <dd className="col-sm-10 mb-0">
                                            {a.canonical_actions?.length ? (
                                                <ol className="mb-0 ps-3" aria-label="Canonical actions requiring approval" data-testid="approval-canonical-actions">
                                                    {a.canonical_actions.map((action, index) => <li key={`${action.operation}-${index}`} className="mb-2">
                                                        <strong>{action.operation}</strong>
                                                        <dl className="row mb-0">
                                                            <dt className="col-sm-3">Target</dt>
                                                            <dd className="col-sm-9">{action.target ?? "Not specified"}</dd>
                                                            <dt className="col-sm-3">Changed fields</dt>
                                                            <dd className="col-sm-9"><ul className="mb-0">{action.changed_fields.map((field) => <li key={field.name}><code>{field.name}</code>: {field.value}</li>)}</ul></dd>
                                                            <dt className="col-sm-3">Reversible</dt>
                                                            <dd className="col-sm-9">{action.reversible ? "Yes" : "No"}</dd>
                                                            <dt className="col-sm-3">Approval scope</dt>
                                                            <dd className="col-sm-9">{action.approval_scope}</dd>
                                                        </dl>
                                                    </li>)}
                                                </ol>
                                            ) : a.kind === "effectful_chain" ? (
                                                <pre className="small mb-0" data-testid="approval-requested-action">{a.requested_action}</pre>
                                            ) : (
                                                <span data-testid="approval-requested-action">{a.requested_action}</span>
                                            )}
                                        </dd>
                                    </dl>
                                    <div
                                        style={{
                                            background: "rgba(0,0,0,0.05)",
                                            borderRadius: "0.5rem",
                                            padding: "0.5rem 0.75rem",
                                            fontStyle: "italic",
                                        }}
                                        data-testid="approval-row-text"
                                    >
                                        &ldquo;{truncate(a.original_text, 280)}&rdquo;
                                    </div>
                                    {a.kind !== "effectful_chain" && (
                                        <Form.Group className="mt-2" controlId={`limited-scope-${a.approval_id}`}>
                                            <Form.Label className="small mb-1">Limited-trust topic scope</Form.Label>
                                            <Form.Control
                                                size="sm"
                                                value={limitedScopes[a.approval_id] ?? ""}
                                                onChange={(event) => setLimitedScopes((current) => ({
                                                    ...current,
                                                    [a.approval_id]: event.target.value,
                                                }))}
                                                placeholder="Comma-separated topics; leave blank for no topic exceptions"
                                                aria-label={`Limited-trust topic scope for ${a.sender_principal_id}`}
                                                data-testid="approval-limited-scope"
                                            />
                                            <Form.Text>
                                                Used only if you choose Limited. Topics are saved on this principal&apos;s trust record.
                                            </Form.Text>
                                        </Form.Group>
                                    )}
                                </div>
                            </div>
                            <div className="d-flex gap-2 flex-wrap">
                                {(a.kind === "effectful_chain" ? CHAIN_ACTIONS : ACTIONS).map((act) => (
                                    <Button
                                        key={act.verb}
                                        size="sm"
                                        variant={act.variant}
                                        disabled={busyApproval !== null || (a.kind === "effectful_chain" && !a.approval_token)}
                                        title={act.title}
                                        onClick={() =>
                                            void onRespond(a, act.verb)
                                        }
                                        data-testid={`approval-row-verb-${act.verb}`}
                                    >
                                        <i
                                            className={`bi ${act.icon} me-2`}
                                            aria-hidden
                                        />
                                        {act.label}
                                    </Button>
                                ))}
                            </div>
                        </li>
                    ))}
                </ul>
                    )}
                </>
            )}
        </div>
    );
}

function truncate(s: string, n: number): string {
    if (s.length <= n) return s;
    return s.slice(0, n - 1) + "…";
}
