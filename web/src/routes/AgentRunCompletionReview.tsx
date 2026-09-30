import { useState } from "react";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";
import { confirmAgentDelivery, recordAgentArtifact, recordAgentCriterion, type AgentCompletionReport } from "../api/agents";
import { CompletionEvidenceRef } from "../components/CompletionEvidenceRef";

export function AgentRunCompletionReview({
    agentId,
    runId,
    report,
    token,
    onUpdated,
}: {
    agentId: string;
    runId: string;
    report: AgentCompletionReport;
    token: () => string | null;
    onUpdated: () => void;
}) {
    const [statuses, setStatuses] = useState<Record<string, "passed" | "failed" | "blocked">>({});
    const [evidence, setEvidence] = useState<Record<string, string>>({});
    const [details, setDetails] = useState<Record<string, string>>({});
    const [error, setError] = useState<string | null>(null);

    async function saveCriterion(id: string) {
        setError(null);
        try {
            const status = statuses[id] ?? "passed";
            const evidence_refs = (evidence[id] ?? "").split("\n").map((value) => value.trim()).filter(Boolean);
            await recordAgentCriterion(agentId, runId, id, { status, evidence_refs, detail: details[id]?.trim() || null }, token);
            onUpdated();
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : "Could not record criterion evidence.");
        }
    }

    async function saveArtifact(id: string, present: boolean) {
        setError(null);
        try {
            await recordAgentArtifact(agentId, runId, id, {
                present,
                evidence_ref: present ? evidence[id]?.trim() || null : null,
                detail: details[id]?.trim() || null,
            }, token);
            onUpdated();
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : "Could not record artifact evidence.");
        }
    }

    async function saveDelivery() {
        setError(null);
        try {
            await confirmAgentDelivery(agentId, runId, evidence.delivery ?? "", token);
            onUpdated();
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : "Could not confirm delivery.");
        }
    }

    return <section className="border rounded p-2 mt-2" aria-label="Agent run completion evidence">
        <div className="d-flex justify-content-between"><strong>Task outcome</strong><span className="text-capitalize">{report.status.replaceAll("_", " ")}</span></div>
        <p className="small text-muted mb-2">A successful model turn remains unverified until the required checks, artifacts, and delivery evidence are recorded.</p>
        {error && <div className="alert alert-danger py-1 small" role="alert">{error}</div>}
        {report.contract.acceptance_criteria.map((criterion) => {
            const recorded = report.verifications.find((item) => item.criterion_id === criterion.criterion_id);
            const status = statuses[criterion.criterion_id] ?? (recorded?.status === "failed" || recorded?.status === "blocked" ? recorded.status : "passed");
            return <div className="border-top py-2" key={criterion.criterion_id}>
                <strong className="small">{criterion.description}</strong>{!criterion.required && <span className="badge text-bg-light ms-2">optional</span>}
                <div className="small text-muted">Status: {recorded?.status ?? "pending"}{recorded?.detail && ` · ${recorded.detail}`}</div>
                {recorded?.evidence_refs.map((reference, index) => <span className="small me-2" key={`${index}-${reference}`}><CompletionEvidenceRef reference={reference} /></span>)}
                <div className="d-flex gap-2 mt-1">
                    <Form.Select aria-label={`Result for ${criterion.description}`} size="sm" value={status} onChange={(event) => setStatuses((current) => ({ ...current, [criterion.criterion_id]: event.target.value as "passed" | "failed" | "blocked" }))}>
                        <option value="passed">Passed</option><option value="failed">Failed</option><option value="blocked">Blocked</option>
                    </Form.Select>
                    <Form.Control size="sm" aria-label={`Evidence for ${criterion.description}`} placeholder="Evidence refs, one per line" as="textarea" rows={1} value={evidence[criterion.criterion_id] ?? ""} onChange={(event) => setEvidence((current) => ({ ...current, [criterion.criterion_id]: event.target.value }))} />
                    <Button size="sm" variant="outline-primary" disabled={status === "passed" && !(evidence[criterion.criterion_id]?.trim() || recorded?.evidence_refs.length)} onClick={() => void saveCriterion(criterion.criterion_id)}>Record</Button>
                </div>
                <Form.Control size="sm" className="mt-1" aria-label={`Verifier detail for ${criterion.description}`} placeholder="Optional verifier detail" value={details[criterion.criterion_id] ?? ""} onChange={(event) => setDetails((current) => ({ ...current, [criterion.criterion_id]: event.target.value }))} />
            </div>;
        })}
        {report.contract.required_artifacts.map((artifact) => {
            const recorded = report.artifacts.find((item) => item.artifact_id === artifact.artifact_id);
            return <div className="border-top py-2" key={artifact.artifact_id}>
                <strong className="small">{artifact.description}</strong>
                <div className="small text-muted">{recorded?.present ? "Present" : "Not verified"}{recorded?.evidence_ref && <> · <CompletionEvidenceRef reference={recorded.evidence_ref} /></>}</div>
                <div className="d-flex gap-2 mt-1"><Form.Control size="sm" aria-label={`Evidence for ${artifact.description}`} placeholder="Artifact path or evidence reference" value={evidence[artifact.artifact_id] ?? ""} onChange={(event) => setEvidence((current) => ({ ...current, [artifact.artifact_id]: event.target.value }))} /><Button size="sm" variant="outline-primary" disabled={!evidence[artifact.artifact_id]?.trim()} onClick={() => void saveArtifact(artifact.artifact_id, true)}>Present</Button><Button size="sm" variant="outline-secondary" onClick={() => void saveArtifact(artifact.artifact_id, false)}>Missing</Button></div>
            </div>;
        })}
        {report.contract.delivery_required && <div className="border-top py-2"><strong className="small">External delivery</strong><div className="small text-muted">{report.delivery_confirmed ? "Confirmed" : "Confirmation required"}{report.delivery_evidence_ref && <> · <CompletionEvidenceRef reference={report.delivery_evidence_ref} /></>}</div><div className="d-flex gap-2 mt-1"><Form.Control size="sm" aria-label="Delivery evidence" placeholder="Durable receipt or delivery reference" value={evidence.delivery ?? ""} onChange={(event) => setEvidence((current) => ({ ...current, delivery: event.target.value }))} /><Button size="sm" variant="outline-primary" disabled={!evidence.delivery?.trim()} onClick={() => void saveDelivery()}>Confirm</Button></div></div>}
        {report.unfinished.length > 0 && <ul className="small text-danger mb-0 mt-2">{report.unfinished.map((item) => <li key={item}>{item}</li>)}</ul>}
    </section>;
}
