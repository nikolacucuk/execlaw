import { useCallback, useEffect, useState, type FormEvent } from "react";
import Button from "react-bootstrap/Button";
import {
    listDurableTurnControls,
    submitDurableTurnControl,
    type DurableTurnControl,
    type TurnControlKind,
} from "../api/endpoints";
import { randomUuid } from "../api/randomId";

const REFRESH_MS = 4_000;

interface Props {
    conversationId: string;
    getToken: () => string | null;
}

function formatTime(value: number): string {
    return new Date(value * 1000).toLocaleTimeString();
}

function readPendingKeys(storageKey: string): Record<string, string> {
    try {
        const saved = sessionStorage.getItem(storageKey);
        return saved ? JSON.parse(saved) as Record<string, string> : {};
    } catch {
        return {};
    }
}

function writePendingKeys(storageKey: string, keys: Record<string, string>): void {
    try {
        sessionStorage.setItem(storageKey, JSON.stringify(keys));
    } catch {
        // The server still rejects reuse of a key with different intent.
    }
}

export function DurableTurnControls({ conversationId, getToken }: Props) {
    const [controls, setControls] = useState<DurableTurnControl[]>([]);
    const [text, setText] = useState("");
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const storageKey = `execlaw.turn-controls.pending.${conversationId}`;

    const refresh = useCallback(async () => {
        try {
            setControls(await listDurableTurnControls(conversationId, getToken));
            setError(null);
        } catch (cause) {
            setError((cause as Error).message || "Could not load turn controls");
        }
    }, [conversationId, getToken]);

    useEffect(() => {
        void refresh();
        const timer = window.setInterval(() => void refresh(), REFRESH_MS);
        return () => window.clearInterval(timer);
    }, [refresh]);

    const submit = async (kind: TurnControlKind) => {
        if ((kind === "steer" || kind === "queue_next_turn") && !text.trim()) {
            setError("Enter the operator message first.");
            return;
        }
        const intentText = kind === "steer" || kind === "queue_next_turn" ? text : "";
        const intentKey = JSON.stringify([kind, intentText]);
        const pending = readPendingKeys(storageKey);
        const idempotencyKey = pending[intentKey] ?? randomUuid();
        if (!pending[intentKey]) {
            pending[intentKey] = idempotencyKey;
            writePendingKeys(storageKey, pending);
        }
        setBusy(true);
        try {
            const record = await submitDurableTurnControl(
                conversationId,
                kind,
                getToken,
                idempotencyKey,
                intentText || undefined,
            );
            setControls((current) => [
                ...current.filter((item) => item.control_id !== record.control_id),
                record,
            ].slice(-50));
            if (kind === "steer" || kind === "queue_next_turn") setText("");
            delete pending[intentKey];
            writePendingKeys(storageKey, pending);
            setError(null);
        } catch (cause) {
            setError((cause as Error).message || "Could not deliver turn control");
        } finally {
            setBusy(false);
        }
    };

    const submitSteering = (event: FormEvent<HTMLFormElement>) => {
        event.preventDefault();
        void submit("steer");
    };

    return (
        <details className="mb-2" data-testid="durable-turn-controls">
            <summary className="small fw-semibold">Steer, pause, or queue a message</summary>
            <div className="border rounded p-2 mt-2">
                {error && <div className="small text-danger" role="alert">{error}</div>}
                <form onSubmit={submitSteering}>
                    <label className="visually-hidden" htmlFor="turn-control-text">Operator steering or next-turn message</label>
                    <textarea
                        id="turn-control-text"
                        className="form-control form-control-sm"
                        rows={2}
                        maxLength={16_384}
                        value={text}
                        onChange={(event) => setText(event.target.value)}
                        placeholder="A correction applies at the next safe model boundary."
                        disabled={busy}
                    />
                    <div className="d-flex flex-wrap gap-2 mt-2">
                        <Button size="sm" type="submit" variant="outline-primary" disabled={busy || !text.trim()}>Steer now</Button>
                        <Button size="sm" type="button" variant="outline-secondary" disabled={busy || !text.trim()} onClick={() => void submit("queue_next_turn")}>Queue next turn</Button>
                        <Button size="sm" type="button" variant="outline-secondary" disabled={busy} onClick={() => void submit("pause")}>Pause at boundary</Button>
                        <Button size="sm" type="button" variant="outline-secondary" disabled={busy} onClick={() => void submit("resume")}>Resume</Button>
                        <Button size="sm" type="button" variant="outline-danger" disabled={busy} onClick={() => void submit("cancel")}>Cancel runner turn</Button>
                    </div>
                </form>
                {controls.length > 0 && <ul className="list-unstyled small mt-3 mb-0" aria-label="Durable turn control history">
                    {controls.slice(-8).reverse().map((control) => {
                        const message = typeof control.payload.text === "string" ? control.payload.text : "";
                        return <li key={control.control_id} className="border-top pt-1 mt-1">
                            <span className="text-capitalize">{control.kind.replaceAll("_", " ")}</span>
                            {message && <span>: {message.length > 160 ? `${message.slice(0, 157)}…` : message}</span>}
                            <span className="text-muted"> · {control.status} · {formatTime(control.updated_at)}</span>
                        </li>;
                    })}
                </ul>}
            </div>
        </details>
    );
}
