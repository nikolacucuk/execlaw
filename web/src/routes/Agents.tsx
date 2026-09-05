import { useEffect, useState } from "react";
import Button from "react-bootstrap/Button";
import Form from "react-bootstrap/Form";
import { Sidebar } from "../chat/Sidebar";
import { useAuth } from "../auth/AuthContext";
import {
    createAgent,
    importAgentMarkdown,
    listAgentRuns,
    listAgents,
    pauseAgent,
    resumeAgent,
    sendAgentMessage,
    type AgentRun,
    type AgentView,
} from "../api/agents";

export function Agents() {
    const auth = useAuth();
    const token = auth.getAccessToken;
    const [agents, setAgents] = useState<AgentView[]>([]);
    const [selected, setSelected] = useState<string | null>(null);
    const [runs, setRuns] = useState<AgentRun[]>([]);
    const [message, setMessage] = useState("");
    const [form, setForm] = useState({ name: "", role_prompt: "", interval_secs: 300 });

    const refresh = async () => setAgents(await listAgents(token));

    useEffect(() => {
        void refresh();
    }, []);

    useEffect(() => {
        if (selected) void listAgentRuns(selected, token).then(setRuns);
    }, [selected]);

    async function save() {
        await createAgent(form, token);
        setForm({ name: "", role_prompt: "", interval_secs: 300 });
        await refresh();
    }

    async function importMarkdown(file: File | undefined) {
        if (!file) return;
        await importAgentMarkdown(await file.text(), token);
        await refresh();
    }

    async function toggle(agent: AgentView) {
        if (agent.paused) await resumeAgent(agent.id, token);
        else await pauseAgent(agent.id, token);
        await refresh();
    }

    async function enqueue() {
        if (!selected || !message.trim()) return;
        await sendAgentMessage(selected, message, token);
        setMessage("");
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
                            onChange={(event) => void importMarkdown((event.target as HTMLInputElement).files?.[0])}
                            aria-label="Load agent Markdown file"
                        />
                        <p className="small text-muted mt-2">Select an .agent.md file to store its instructions in the controller database.</p>
                        <h3 className="h5">Create agent</h3>
                        <div className="row g-2">
                            <div className="col-md-3"><Form.Control placeholder="Name" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} /></div>
                            <div className="col-md-6"><Form.Control placeholder="Role prompt" value={form.role_prompt} onChange={(e) => setForm({ ...form, role_prompt: e.target.value })} /></div>
                            <div className="col-md-2"><Form.Control type="number" min={5} value={form.interval_secs} onChange={(e) => setForm({ ...form, interval_secs: Number(e.target.value) })} /></div>
                            <div className="col-md-1"><Button onClick={() => void save()} aria-label="Create agent"><i className="bi bi-plus-lg" /></Button></div>
                        </div>
                    </section>
                    <div className="row g-4">
                        <div className="col-lg-5">
                            <h3 className="h5">Definitions</h3>
                            {agents.length === 0 && <p className="text-muted" data-testid="agents-empty">No agents configured.</p>}
                            {agents.map((agent) => (
                                <div key={agent.id} className={`w-100 text-start mb-2 p-3 border ${selected === agent.id ? "border-primary" : ""}`}>
                                    <button className="btn btn-link text-start p-0 text-decoration-none" onClick={() => setSelected(agent.id)}>
                                        <strong>{agent.name}</strong>
                                        <span className="d-block small text-muted">{agent.paused ? "Paused" : agent.enabled ? "Running" : "Disabled"} · every {agent.interval_secs}s</span>
                                        <span className="d-block small">{agent.last_run_status ?? "Never run"}</span>
                                    </button>
                                    <span className="d-block mt-2"><Button size="sm" variant="outline-secondary" onClick={() => void toggle(agent)}>{agent.paused ? "Resume" : "Pause"}</Button></span>
                                </div>
                            ))}
                        </div>
                        <div className="col-lg-7">
                            <h3 className="h5">Mailbox and runs</h3>
                            {selected ? <>
                                <div className="input-group mb-3"><Form.Control placeholder="Send a message to this agent" value={message} onChange={(e) => setMessage(e.target.value)} /><Button onClick={() => void enqueue()}>Send</Button></div>
                                {runs.map((run) => <div className="border-bottom py-2" key={run.id}><strong>{run.status}</strong> <span className="small text-muted">{new Date(run.started_at * 1000).toLocaleString()}</span>{run.output_text && <p className="mb-0">{run.output_text}</p>}{run.error && <p className="text-danger mb-0">{run.error}</p>}</div>)}
                            </> : <p className="text-muted">Select an agent to inspect its mailbox and run history.</p>}
                        </div>
                    </div>
                </div>
            </main>
        </div>
    );
}
