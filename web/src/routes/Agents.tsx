import { useEffect, useState } from "react";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";
import { Sidebar } from "../chat/Sidebar";
import { useAuth } from "../auth/AuthContext";
import { WsClient, type WsEvent } from "../api/ws";
import { listChildTasks, type ChildTaskTrace } from "../api/runs";
import { ArtifactDownloadLink } from "../components/ArtifactDownloadLink";
import { AgentRunCompletionReview } from "./AgentRunCompletionReview";
import {
    createAgent,
    importAgentMarkdown,
    listAgentRuns,
    listAgentScheduleFires,
    listAgents,
    pauseAgent,
    resumeAgent,
    sendAgentMessage,
    updateAgent,
    previewAgent,
    listAgentReplyDrafts,
    editAgentReplyDraft,
    rejectAgentReplyDraft,
    getAgentOwnership,
    takeOverAgentConversation,
    handBackAgentConversation,
    type AgentRun,
    type AgentScheduleFire,
    type AgentView,
    type AgentCriterion,
    type AgentReplyDraft,
    type AgentOwnership,
    type AgentPreviewResult,
    type AgentPreviewEvent,
} from "../api/agents";

export function Agents() {
    const auth = useAuth();
    const token = auth.getAccessToken;
    const [agents, setAgents] = useState<AgentView[]>([]);
    const [selected, setSelected] = useState<string | null>(null);
    const [runs, setRuns] = useState<AgentRun[]>([]);
    const [scheduleFires, setScheduleFires] = useState<AgentScheduleFire[]>([]);
    const [message, setMessage] = useState("");
    const [formError, setFormError] = useState<string | null>(null);
    const [form, setForm] = useState({ name: "", role_prompt: "", interval_secs: 300, triggerJson: "{}", criteria: "", verifiers: "", artifacts: "", deliveryRequired: false });
    const [markdownFile, setMarkdownFile] = useState<File | null>(null);
    const [importStatus, setImportStatus] = useState<string | null>(null);
    const [childTasks, setChildTasks] = useState<ChildTaskTrace[]>([]);
    const [childTaskError, setChildTaskError] = useState<string | null>(null);
    const [drafts, setDrafts] = useState<AgentReplyDraft[]>([]);
    const [draftEdits, setDraftEdits] = useState<Record<string, string>>({});
    const [draftError, setDraftError] = useState<string | null>(null);
    const [owners, setOwners] = useState<Record<string, AgentOwnership | null>>({});
    const [previewEventsJson, setPreviewEventsJson] = useState("[]");
    const [previewExpected, setPreviewExpected] = useState("");
    const [previewOutcomes, setPreviewOutcomes] = useState("");
    const [previewRunModel, setPreviewRunModel] = useState(false);
    const [previewResult, setPreviewResult] = useState<AgentPreviewResult | null>(null);
    const [previewError, setPreviewError] = useState<string | null>(null);

    const refresh = async () => setAgents(await listAgents(token));

    const refreshRuns = async (agentId: string) => {
        setRuns(await listAgentRuns(agentId, token));
    };
    const refreshScheduleFires = async (agentId: string) => setScheduleFires(await listAgentScheduleFires(agentId, token));
    const refreshDrafts = async () => setDrafts(await listAgentReplyDrafts(token));

    const refreshChildTasks = async () => {
        try {
            setChildTasks(await listChildTasks(token));
            setChildTaskError(null);
        } catch (error) {
            setChildTaskError(error instanceof Error ? error.message : "Delegated run tree could not be loaded.");
        }
    };

    useEffect(() => {
        void refresh();
        void refreshChildTasks();
        void refreshDrafts();
    }, []);

    useEffect(() => {
        if (!selected) {
            setRuns([]);
            setScheduleFires([]);
            return;
        }
        void refreshRuns(selected);
        void refreshScheduleFires(selected);
        const client = new WsClient({
            onEvent: (event: WsEvent) => {
                if (event.kind !== "agent_run_changed" || event.agent_id !== selected) return;
                void refreshRuns(selected);
                void refreshScheduleFires(selected);
                void refresh();
                void refreshDrafts();
            },
        });
        client.open();
        return () => client.close();
    }, [selected]);

    async function save() {
        setFormError(null);
        try {
        const acceptance_criteria: AgentCriterion[] = form.criteria.split("\n").map((line) => line.trim()).filter(Boolean).map((line) => {
            const separator = line.indexOf("=");
            if (separator < 1 || !line.slice(separator + 1).trim()) throw new Error("Each criterion must be id=description.");
            return { criterion_id: line.slice(0, separator).trim(), description: line.slice(separator + 1).trim(), required: true };
        });
        for (const line of form.verifiers.split("\n").map((item) => item.trim()).filter(Boolean)) {
            const first = line.indexOf("|");
            const second = line.indexOf("|", first + 1);
            if (first < 1 || second <= first + 1) throw new Error("Each output check must be criterion-id|/json/path|expected JSON.");
            const criterionId = line.slice(0, first).trim();
            const pointer = line.slice(first + 1, second).trim();
            const expected = JSON.parse(line.slice(second + 1).trim());
            const criterion = acceptance_criteria.find((item) => item.criterion_id === criterionId);
            if (!criterion || criterion.verifier) throw new Error(`Output check has an unknown or duplicate criterion: ${criterionId}`);
            criterion.verifier = { step_id: "agent:output", json_pointer: pointer, expected };
        }
        const required_artifacts = form.artifacts.split("\n").map((line) => line.trim()).filter(Boolean).map((line) => {
            const separator = line.indexOf("=");
            if (separator < 1 || !line.slice(separator + 1).trim()) throw new Error("Each artifact must be id=description.");
            return { artifact_id: line.slice(0, separator).trim(), description: line.slice(separator + 1).trim() };
        });
        const hasContract = acceptance_criteria.length > 0 || required_artifacts.length > 0 || form.deliveryRequired;
        await createAgent({
            name: form.name,
            role_prompt: form.role_prompt,
            interval_secs: form.interval_secs,
            trigger: JSON.parse(form.triggerJson) as Record<string, unknown>,
            ...(hasContract ? { completion_contract: { acceptance_criteria, required_artifacts, delivery_required: form.deliveryRequired } } : {}),
        }, token);
        setForm({ name: "", role_prompt: "", interval_secs: 300, triggerJson: "{}", criteria: "", verifiers: "", artifacts: "", deliveryRequired: false });
        await refresh();
        } catch (error) {
            setFormError(error instanceof Error ? error.message : "Could not save the agent definition.");
        }
    }

    async function importMarkdown() {
        if (!markdownFile) return;
        setImportStatus(null);
        try {
            const imported = await importAgentMarkdown(await markdownFile.text(), token);
            setMarkdownFile(null);
            setImportStatus(`Loaded ${imported.name}.`);
            await refresh();
        } catch (error) {
            setImportStatus(error instanceof Error ? error.message : "Agent Markdown could not be loaded.");
        }
    }

    async function toggle(agent: AgentView) {
        if (agent.paused) await resumeAgent(agent.id, token);
        else await pauseAgent(agent.id, token);
        await refresh();
    }

    async function setReplyMode(agent: AgentView, reply_mode: "draft" | "automatic") {
        await updateAgent(agent.id, { ...agent, reply_mode }, token);
        await refresh();
    }

    async function enqueue() {
        if (!selected || !message.trim()) return;
        await sendAgentMessage(selected, message, token);
        setMessage("");
        await refreshRuns(selected);
        await refresh();
    }

    async function runPreview() {
        if (!selected) return;
        setPreviewError(null);
        try {
            const agent = agents.find((item) => item.id === selected);
            if (!agent) return;
            const events = JSON.parse(previewEventsJson) as AgentPreviewEvent[];
            if (!Array.isArray(events)) throw new Error("Preview events must be a JSON array.");
            const expected = previewExpected.split(",").map((item) => item.trim()).filter(Boolean);
            const outcomes = Object.fromEntries(previewOutcomes.split("\n").map((line) => line.trim()).filter(Boolean).map((line) => {
                const [eventId, kind] = line.split("=").map((part) => part.trim());
                if (!eventId || !kind) throw new Error("Expected outcomes must use event-id=kind.");
                return [eventId, kind];
            }));
            setPreviewResult(await previewAgent(agent.id, agent.trigger, events, expected, outcomes, previewRunModel, token));
        } catch (error) {
            setPreviewError(error instanceof Error ? error.message : "Preview failed.");
        }
    }

    async function editDraft(draft: AgentReplyDraft) {
        setDraftError(null);
        try {
            await editAgentReplyDraft(draft.id, draft.revision, draftEdits[draft.id] ?? draft.draft_text, token);
            await refreshDrafts();
        } catch (error) {
            setDraftError(error instanceof Error ? error.message : "Draft update failed.");
        }
    }

    async function rejectDraft(draft: AgentReplyDraft) {
        setDraftError(null);
        try {
            await rejectAgentReplyDraft(draft.id, draft.revision, token);
            await refreshDrafts();
        } catch (error) {
            setDraftError(error instanceof Error ? error.message : "Draft rejection failed.");
        }
    }

    async function takeOverDraft(draft: AgentReplyDraft) {
        setDraftError(null);
        try {
            const owner = await takeOverAgentConversation(draft.conversation_id, draft.channel, draft.recipient, token);
            setOwners((current) => ({ ...current, [draft.id]: owner }));
        } catch (error) {
            setDraftError(error instanceof Error ? error.message : "Takeover failed.");
        }
    }

    async function handBackDraft(draft: AgentReplyDraft, owner: AgentOwnership) {
        setDraftError(null);
        try {
            await handBackAgentConversation(draft.conversation_id, draft.channel, draft.recipient, owner.generation, token);
            setOwners((current) => ({ ...current, [draft.id]: null }));
        } catch (error) {
            setDraftError(error instanceof Error ? error.message : "Hand-back failed.");
        }
    }

    async function inspectOwner(draft: AgentReplyDraft) {
        setDraftError(null);
        try {
            const owner = await getAgentOwnership(draft.conversation_id, draft.channel, draft.recipient, token);
            setOwners((current) => ({ ...current, [draft.id]: owner }));
        } catch (error) {
            setDraftError(error instanceof Error ? error.message : "Ownership lookup failed.");
        }
    }

    return (
        <div className="execlaw-shell">
            <Sidebar onNewThread={() => {}} />
            <main className="execlaw-main">
                <header className="execlaw-main__head">
                    <h2 className="h6 mb-0"><i className="bi bi-people me-2" aria-hidden />Always-on agents</h2>
                </header>
                <div className="execlaw-page execlaw-page--scroll">
                    <section className="mb-4">
                        <h3 className="h5">Load agent Markdown</h3>
                        <Form.Control
                            type="file"
                            accept=".md,text/markdown,text/plain"
                            onChange={(event) => setMarkdownFile((event.target as HTMLInputElement).files?.[0] ?? null)}
                            aria-label="Load agent Markdown file"
                        />
                        <Button className="mt-2" disabled={!markdownFile} onClick={() => void importMarkdown()}>
                            <i className="bi bi-upload me-2" aria-hidden />Load into execlaw
                        </Button>
                        {importStatus && <p className="small mt-2" role="status">{importStatus}</p>}
                        <p className="small text-muted mt-2">Select an .agent.md file to store its instructions in the controller database.</p>
                        <h3 className="h5">Create agent</h3>
                        <div className="row g-2">
                            <div className="col-md-3"><Form.Control placeholder="Name" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} /></div>
                            <div className="col-md-6"><Form.Control placeholder="Role prompt" value={form.role_prompt} onChange={(e) => setForm({ ...form, role_prompt: e.target.value })} /></div>
                            <div className="col-md-2"><Form.Control type="number" min={5} value={form.interval_secs} onChange={(e) => setForm({ ...form, interval_secs: Number(e.target.value) })} /></div>
                            <div className="col-md-1"><Button onClick={() => void save()} aria-label="Create agent"><i className="bi bi-plus-lg" /></Button></div>
                            <div className="col-12"><Form.Label htmlFor="agent-trigger-json" className="small">Trigger and schedule JSON</Form.Label><Form.Control id="agent-trigger-json" as="textarea" rows={3} value={form.triggerJson} onChange={(event) => setForm({ ...form, triggerJson: event.target.value })} /><Form.Text>Example: {`{"event_only":true,"channel":"whatsapp","group_ids":["group@g.us"],"keywords":["camper"]}`}. For a timed agent, add {`"schedule":{"cron":"0 8 * * *","timezone":"America/Vancouver","overlap":"skip","catchup_secs":3600}`}.</Form.Text></div>
                            <div className="col-md-6"><Form.Label htmlFor="agent-completion-criteria" className="small">Acceptance criteria (one required criterion per line: id=description)</Form.Label><Form.Control id="agent-completion-criteria" as="textarea" rows={2} value={form.criteria} onChange={(e) => setForm({ ...form, criteria: e.target.value })} /></div>
                            <div className="col-md-6"><Form.Label htmlFor="agent-output-verifiers" className="small">Structured output checks (criterion-id|/json/path|expected JSON)</Form.Label><Form.Control id="agent-output-verifiers" as="textarea" rows={2} value={form.verifiers} onChange={(e) => setForm({ ...form, verifiers: e.target.value })} /></div>
                            <div className="col-md-6"><Form.Label htmlFor="agent-completion-artifacts" className="small">Required artifacts (one per line: id=description)</Form.Label><Form.Control id="agent-completion-artifacts" as="textarea" rows={2} value={form.artifacts} onChange={(e) => setForm({ ...form, artifacts: e.target.value })} /></div>
                            <div className="col-12"><Form.Check id="agent-delivery-required" label="Require a Controller delivery confirmation for each run" checked={form.deliveryRequired} onChange={(e) => setForm({ ...form, deliveryRequired: e.target.checked })} /></div>
                        </div>
                        {formError && <div className="alert alert-danger py-2 mt-2" role="alert">{formError}</div>}
                    </section>
                    <div className="row g-4">
                        <div className="col-lg-5">
                            <h3 className="h5">Definitions</h3>
                            {agents.length === 0 && <p className="text-muted" data-testid="agents-empty">No agents configured.</p>}
                            {agents.map((agent) => (
                                <div key={agent.id} className={`w-100 text-start mb-2 p-3 border ${selected === agent.id ? "border-primary" : ""}`}>
                                    <button className="btn btn-link text-start p-0 text-decoration-none" onClick={() => setSelected(agent.id)}>
                                        <strong>{agent.name}</strong>
                                        <span className="d-block small text-muted">{agent.paused ? "Paused" : agent.enabled ? "Running" : "Disabled"} · {agent.trigger.event_only ? "on matching inbound event" : `every ${agent.interval_secs}s`}</span>
                                        <span className="d-block small text-muted">{agent.trigger.channel ? String(agent.trigger.channel) : "any channel"}{agent.trigger.group_only ? " · groups only" : ""}{Array.isArray(agent.trigger.keywords) && agent.trigger.keywords.length > 0 ? ` · ${agent.trigger.keywords.join(", ")}` : ""}</span>
                                        <span className="d-block small text-muted">{agent.reply_mode === "automatic" ? "Automatic replies enabled" : "Draft replies require review"}</span>
                                        <span className="d-block small">{agent.last_run_status ?? "Never run"}</span>
                                        <span className="d-block small text-muted">Definition v{agent.definition_version}{agent.schedule_next_at ? ` · next scheduled ${new Date(agent.schedule_next_at * 1000).toLocaleString()}` : ""}</span>
                                    </button>
                                    <span className="d-flex gap-2 mt-2">
                                        <Button size="sm" variant="outline-secondary" onClick={() => void toggle(agent)}>{agent.paused ? "Resume" : "Pause"}</Button>
                                        <Form.Select
                                            aria-label={`Reply mode for ${agent.name}`}
                                            size="sm"
                                            className="w-auto"
                                            value={agent.reply_mode}
                                            onChange={(event) => void setReplyMode(agent, event.target.value as "draft" | "automatic")}
                                        >
                                            <option value="draft">Review before sending</option>
                                            <option value="automatic">Send automatically</option>
                                        </Form.Select>
                                    </span>
                                </div>
                            ))}
                        </div>
                        <div className="col-lg-7">
                            <h3 className="h5">Mailbox and runs</h3>
                            {selected ? <>
                                <div className="input-group mb-3"><Form.Control placeholder="Send a message to this agent" value={message} onChange={(e) => setMessage(e.target.value)} /><Button onClick={() => void enqueue()}>Send</Button></div>
                                {runs.map((run) => <div className="border-bottom py-2" key={run.id} id={`agent-run-${run.id}`}><strong>{(run.outcome_kind ?? (run.status === "not_applicable" ? "irrelevant" : run.status)).replaceAll("_", " ")}</strong> <span className="small text-muted">{new Date(run.started_at * 1000).toLocaleString()}</span>{run.checkpoint?.mailbox_count ? <p className="small text-muted mb-1">Inbound event: {String(run.checkpoint.mailbox_count)} mailbox item(s)</p> : null}{run.output_text && <pre className="mb-0 small text-wrap">{run.output_text}</pre>}{run.error && <p className="text-danger mb-0">{run.error}</p>}{run.completion && <AgentRunCompletionReview agentId={run.agent_id} runId={run.id} report={run.completion} token={token} onUpdated={() => { void refreshRuns(run.agent_id); }} />}</div>)}
                                {scheduleFires.length > 0 && <div className="mt-3"><h4 className="h6">Schedule fires</h4><ol className="small">{scheduleFires.map((fire) => <li key={`${fire.agent_id}:${fire.due_at}`}>{new Date(fire.due_at * 1000).toLocaleString()}: {fire.status}{fire.reason ? ` (${fire.reason.replaceAll("_", " ")})` : ""}</li>)}</ol></div>}
                            </> : <p className="text-muted">Select an agent to inspect its mailbox and run history.</p>}
                        </div>
                    </div>
                    <section className="mt-4" aria-labelledby="agent-preview-heading">
                        <h3 id="agent-preview-heading" className="h5">Trigger preview</h3>
                        <p className="small text-muted">Paste captured or synthetic event JSON to check admission. Preview records no effects and makes no model call.</p>
                        <Form.Control as="textarea" rows={4} aria-label="Preview events JSON" value={previewEventsJson} onChange={(event) => setPreviewEventsJson(event.target.value)} />
                        <Form.Control className="mt-2" aria-label="Expected matching event IDs" placeholder="Expected event IDs, comma separated" value={previewExpected} onChange={(event) => setPreviewExpected(event.target.value)} />
                        <Form.Control className="mt-2" as="textarea" rows={2} aria-label="Expected outcomes" placeholder="One event-id=outcome-kind per line" value={previewOutcomes} onChange={(event) => setPreviewOutcomes(event.target.value)} />
                        <Form.Check className="mt-2" label="Also run the local model (up to 20 matched events; no tools or effects)" checked={previewRunModel} onChange={(event) => setPreviewRunModel(event.target.checked)} />
                        <Button size="sm" className="mt-2" disabled={!selected} onClick={() => void runPreview()}>Preview selected agent</Button>
                        {previewError && <p role="alert" className="text-danger small mt-2">{previewError}</p>}
                        {previewResult && <div className="small mt-2" role="status">Matched {previewResult.matched}; false positives {previewResult.false_positives}; missed {previewResult.missed}; effects {previewResult.effect_count}. Model mismatches {previewResult.outcome_mismatches}; missing drafts {previewResult.missing_drafts}; irrelevant {previewResult.irrelevant_count}; slowest {previewResult.max_latency_ms} ms.<ul>{previewResult.decisions.map((decision) => <li key={`${decision.source}:${decision.event_id}`}>{decision.event_id}: {decision.matched ? "matched" : "filtered"} ({decision.reason})</li>)}</ul>{previewResult.model_results.map((result) => <p key={result.event_id}>{result.event_id}: {result.outcome_kind ?? result.error} ({result.latency_ms} ms)</p>)}</div>}
                    </section>
                    <section className="mt-4" aria-labelledby="agent-draft-inbox-heading">
                        <div className="d-flex align-items-center justify-content-between"><h3 id="agent-draft-inbox-heading" className="h5">Reply draft inbox</h3><Button size="sm" variant="outline-secondary" onClick={() => void refreshDrafts()}>Refresh</Button></div>
                        {draftError && <p role="alert" className="text-danger small">{draftError}</p>}
                        {drafts.length === 0 && <p className="small text-muted">No pending drafts.</p>}
                        {drafts.map((draft) => {
                            const owner = owners[draft.id];
                            return <article key={draft.id} className="border rounded p-3 mb-2">
                                <div className="d-flex justify-content-between"><strong>{draft.agent_id}.agent.md</strong><span className={draft.stale_at ? "text-warning" : "text-muted"}>{draft.stale_at ? "Stale · regenerate before sending" : `Draft v${draft.revision}`}</span></div>
                                <p className="small mb-1">Inbound: {draft.inbound_text}</p>
                                <Form.Control as="textarea" rows={3} aria-label={`Edit draft ${draft.id}`} value={draftEdits[draft.id] ?? draft.draft_text} onChange={(event) => setDraftEdits((current) => ({ ...current, [draft.id]: event.target.value }))} />
                                <div className="d-flex flex-wrap gap-2 mt-2"><Button size="sm" variant="outline-primary" disabled={!!draft.stale_at} onClick={() => void editDraft(draft)}>Save revision</Button><Button size="sm" variant="outline-secondary" onClick={() => void rejectDraft(draft)}>Reject</Button><a className="btn btn-sm btn-outline-success" href={`/chat/${encodeURIComponent(draft.conversation_id)}`}>Review in thread</a><Button size="sm" variant="outline-secondary" onClick={() => void inspectOwner(draft)}>Check owner</Button>{owner?.owner_kind === "controller" ? <Button size="sm" variant="outline-secondary" onClick={() => void handBackDraft(draft, owner)}>Hand back</Button> : <Button size="sm" variant="outline-warning" onClick={() => void takeOverDraft(draft)}>Take over</Button>}</div>
                                {owner && <p className="small text-muted mb-0 mt-2">Owner: {owner.owner_kind === "controller" ? "Controller" : owner.agent_id} · generation {owner.generation}</p>}
                            </article>;
                        })}
                    </section>
                    <section className="mt-4" aria-labelledby="delegated-runs-heading">
                        <div className="d-flex justify-content-between align-items-center gap-2 mb-2">
                            <h3 id="delegated-runs-heading" className="h5 mb-0">Delegated child runs</h3>
                            <Button size="sm" variant="outline-secondary" onClick={() => void refreshChildTasks()}>Refresh</Button>
                        </div>
                        {childTaskError && <div className="alert alert-warning py-2" role="status">{childTaskError}</div>}
                        {childTasks.length === 0 && !childTaskError && <p className="small text-muted">No delegated child runs have been recorded.</p>}
                        {childTasks.map((task) => <article key={task.child_run_id} className="border rounded p-3 mb-2">
                            <div className="d-flex justify-content-between flex-wrap gap-2"><strong>{task.status}</strong><small className="text-muted">{new Date(task.created_at * 1000).toLocaleString()}</small></div>
                            <p className="mb-1 mt-2">{task.task.task ?? "Delegated task"}</p>
                            {task.task.context && <details className="small"><summary>Task context</summary><pre className="text-wrap mt-1 mb-1">{task.task.context}</pre></details>}
                            <div className="small text-muted">Child <code>{task.child_run_id}</code> Â· parent <code>{task.parent_run_id}</code></div>
                            <div className="small text-muted">Token budget {task.tokens_used ?? 0}/{task.budget_tokens} used Â· tool authority {JSON.stringify(task.trust_ceiling)} Â· dependencies {task.dependencies.length ? task.dependencies.join(", ") : "none"}</div>
                            {task.result_artifact_id && <div className="small">Result artifact: <ArtifactDownloadLink artifactId={task.result_artifact_id} /></div>}
                            <a className="small" href="/settings/runs">Open execution inspector</a>
                        </article>)}
                    </section>
                </div>
            </main>
        </div>
    );
}
