import { useCallback, useEffect, useState } from "react";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";
import {
    createEndpointApproval,
    listEndpointApprovals,
    revokeEndpointApproval,
    type EndpointApproval,
    type EndpointApprovalKind,
    type EndpointApprovalScope,
} from "../api/endpoints";
import { useAuth } from "../auth/AuthContext";
import { ErrorBanner } from "../components/ErrorBanner";

export function NetworkAccessPage() {
    const auth = useAuth();
    const getToken = auth.getAccessToken;
    const [approvals, setApprovals] = useState<EndpointApproval[] | null>(null);
    const [scope, setScope] = useState<EndpointApprovalScope>("local_inference");
    const [kind, setKind] = useState<EndpointApprovalKind>("cidr");
    const [value, setValue] = useState("");
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [notice, setNotice] = useState<string | null>(null);
    const canEdit = auth.user?.role === "controller";

    const refresh = useCallback(async () => {
        try {
            setApprovals(await listEndpointApprovals(getToken));
            setError(null);
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : "Could not load endpoint approvals");
        }
    }, [getToken]);

    useEffect(() => {
        if (canEdit) void refresh();
    }, [canEdit, refresh]);

    const add = async () => {
        const trimmed = value.trim();
        if (!trimmed || busy) return;
        setBusy(true);
        setError(null);
        setNotice(null);
        try {
            await createEndpointApproval({ scope, kind, value: trimmed }, getToken);
            setValue("");
            setNotice("Endpoint approval added to the selected capability scope.");
            await refresh();
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : "Could not add endpoint approval");
        } finally {
            setBusy(false);
        }
    };

    const revoke = async (approval: EndpointApproval) => {
        if (busy) return;
        setBusy(true);
        setError(null);
        setNotice(null);
        try {
            await revokeEndpointApproval(approval, getToken);
            setNotice("Endpoint approval revoked.");
            await refresh();
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : "Could not revoke endpoint approval");
        } finally {
            setBusy(false);
        }
    };

    if (!canEdit) {
        return <div role="alert">Controller role required to manage network approvals.</div>;
    }

    return (
        <div data-testid="settings-network-access">
            <h3 className="h6">Network access</h3>
            <p className="small text-muted">
                Approvals are scoped. Local inference grants do not authorize private integrations;
                public web fetching uses a separate public-only policy. Sidecar HTTP also requires
                a port published by the sidecar supervisor.
            </p>
            <ErrorBanner message={error} onDismiss={() => setError(null)} className="mb-3" />
            {notice && <div role="status" className="alert alert-success py-2">{notice}</div>}
            <section className="card mb-3" aria-labelledby="network-approval-heading">
                <div className="card-header" id="network-approval-heading">Add an endpoint approval</div>
                <div className="card-body">
                    <div className="row g-2 align-items-end">
                        <div className="col-md-4">
                            <Form.Label htmlFor="endpoint-approval-scope">Capability</Form.Label>
                            <Form.Select
                                id="endpoint-approval-scope"
                                value={scope}
                                onChange={(event) => setScope(event.target.value as EndpointApprovalScope)}
                            >
                                <option value="local_inference">Local inference</option>
                                <option value="private_integration">Private integration</option>
                            </Form.Select>
                        </div>
                        <div className="col-md-3">
                            <Form.Label htmlFor="endpoint-approval-kind">Approval type</Form.Label>
                            <Form.Select
                                id="endpoint-approval-kind"
                                value={kind}
                                onChange={(event) => setKind(event.target.value as EndpointApprovalKind)}
                            >
                                <option value="cidr">IP range (CIDR)</option>
                                <option value="dns_name">Exact DNS name</option>
                            </Form.Select>
                        </div>
                        <div className="col-md-3">
                            <Form.Label htmlFor="endpoint-approval-value">Value</Form.Label>
                            <Form.Control
                                id="endpoint-approval-value"
                                value={value}
                                onChange={(event) => setValue(event.target.value)}
                                placeholder={kind === "cidr" ? "192.168.1.0/24" : "service.home.arpa"}
                                autoComplete="off"
                            />
                        </div>
                        <div className="col-md-2 d-grid">
                            <Button disabled={busy || !value.trim()} onClick={() => void add()}>
                                Add approval
                            </Button>
                        </div>
                    </div>
                </div>
            </section>
            <section aria-labelledby="network-approval-list-heading">
                <h4 className="h6" id="network-approval-list-heading">Approved destinations</h4>
                {approvals === null ? (
                    <div className="small text-muted">Loading approvals…</div>
                ) : approvals.length === 0 ? (
                    <div className="small text-muted">No private endpoint approvals are configured.</div>
                ) : (
                    <ul className="list-group">
                        {approvals.map((approval) => (
                            <li
                                className="list-group-item d-flex align-items-center gap-2"
                                key={`${approval.scope}:${approval.kind}:${approval.value}`}
                            >
                                <code className="flex-grow-1">{approval.value}</code>
                                <span className="badge text-bg-secondary">{approval.scope.replaceAll("_", " ")}</span>
                                <span className="small">{approval.kind === "cidr" ? "CIDR" : "DNS"}</span>
                                <Button
                                    size="sm"
                                    variant="outline-danger"
                                    disabled={busy}
                                    aria-label={`Revoke ${approval.scope} ${approval.kind} ${approval.value}`}
                                    onClick={() => void revoke(approval)}
                                >
                                    Revoke
                                </Button>
                            </li>
                        ))}
                    </ul>
                )}
            </section>
        </div>
    );
}
