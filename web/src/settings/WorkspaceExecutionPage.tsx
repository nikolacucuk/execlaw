import { useCallback, useEffect, useState } from "react";
import { Alert, Button, Form, Spinner } from "react-bootstrap";
import { useAuth } from "../auth/AuthContext";
import {
    getWorkspaceExecutionConfig,
    putWorkspaceExecutionConfig,
    type WorkspaceExecutionConfig,
} from "../api/endpoints";

const DEFAULT_SERVERS = JSON.stringify(
    {
        rust: ["rust-analyzer"],
        typescript: ["typescript-language-server", "--stdio"],
    },
    null,
    2,
);

export function WorkspaceExecutionPage() {
    const auth = useAuth();
    const token = auth.getAccessToken;
    const [config, setConfig] = useState<WorkspaceExecutionConfig | null>(null);
    const [imageReference, setImageReference] = useState("");
    const [languageServers, setLanguageServers] = useState(DEFAULT_SERVERS);
    const [approveLocal, setApproveLocal] = useState(false);
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [notice, setNotice] = useState<string | null>(null);

    const load = useCallback(async () => {
        setBusy(true);
        setError(null);
        try {
            const next = await getWorkspaceExecutionConfig(token);
            setConfig(next);
            setImageReference(next.image_reference ?? "");
            if (Object.keys(next.language_servers).length > 0) {
                setLanguageServers(JSON.stringify(next.language_servers, null, 2));
            }
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : String(cause));
        } finally {
            setBusy(false);
        }
    }, [token]);

    useEffect(() => {
        void load();
    }, [load]);

    async function save(event: React.FormEvent<HTMLFormElement>) {
        event.preventDefault();
        setBusy(true);
        setError(null);
        setNotice(null);
        try {
            const parsed = JSON.parse(languageServers) as unknown;
            if (
                parsed === null ||
                Array.isArray(parsed) ||
                typeof parsed !== "object" ||
                Object.values(parsed).some(
                    (argv) =>
                        !Array.isArray(argv) ||
                        argv.length === 0 ||
                        argv.some((value) => typeof value !== "string"),
                )
            ) {
                throw new Error(
                    "Language servers must map language IDs to non-empty argv arrays.",
                );
            }
            const next = await putWorkspaceExecutionConfig(token, {
                image_reference: imageReference.trim(),
                language_servers: parsed as Record<string, string[]>,
                approve_local_image: approveLocal,
            });
            setConfig(next);
            setApproveLocal(false);
            setNotice(
                next.approval === "controller_local_digest"
                    ? "This exact local image digest was recorded as Controller-approved."
                    : "Workspace toolchain configuration saved.",
            );
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : String(cause));
        } finally {
            setBusy(false);
        }
    }

    return (
        <section className="execlaw-settings__page" aria-labelledby="workspace-execution-title">
            <h1 id="workspace-execution-title" className="h4">
                Workspace execution
            </h1>
            <p className="text-muted">
                Configure the digest-pinned image used for terminal jobs and
                language-server diagnostics. Jobs run with networking disabled
                against a sanitized, read-only snapshot of the durable run checkout.
            </p>
            {error && <Alert variant="danger" role="alert">{error}</Alert>}
            {notice && <Alert variant="success" role="status">{notice}</Alert>}
            {config?.image_reference && (
                <p className="small text-muted">
                    Current image: <code>{config.image_reference}</code>
                    {config.updated_by ? ` · saved by ${config.updated_by}` : ""}
                </p>
            )}
            <Form onSubmit={save}>
                <Form.Group className="mb-3" controlId="workspace-toolchain-image">
                    <Form.Label>Toolchain image reference</Form.Label>
                    <Form.Control
                        value={imageReference}
                        onChange={(event) => setImageReference(event.target.value)}
                        placeholder="ghcr.io/owner/execlaw-workspace-toolchain@sha256:…"
                        autoComplete="off"
                        required
                    />
                    <Form.Text>
                        Production references must be digest-pinned and provenance verified.
                        A local image ID is accepted only with the Controller approval below.
                    </Form.Text>
                </Form.Group>
                <Form.Group className="mb-3" controlId="workspace-language-servers">
                    <Form.Label>Language-server argv map</Form.Label>
                    <Form.Control
                        as="textarea"
                        rows={8}
                        value={languageServers}
                        onChange={(event) => setLanguageServers(event.target.value)}
                        spellCheck={false}
                        aria-describedby="workspace-language-servers-help"
                    />
                    <Form.Text id="workspace-language-servers-help">
                        JSON object from language ID to argv, for example
                        <code> {`{"rust":["rust-analyzer"]}`} </code>.
                        Commands run inside the selected offline image.
                    </Form.Text>
                </Form.Group>
                <Form.Check
                    className="mb-3"
                    id="workspace-approve-local-image"
                    type="checkbox"
                    checked={approveLocal}
                    onChange={(event) => setApproveLocal(event.target.checked)}
                    label="Approve this exact local image digest for this installation"
                />
                <div className="d-flex gap-2">
                    <Button type="submit" disabled={busy || auth.status !== "authenticated"}>
                        {busy ? <><Spinner size="sm" className="me-2" />Saving…</> : "Save toolchain"}
                    </Button>
                    <Button type="button" variant="outline-secondary" onClick={() => void load()} disabled={busy}>
                        Reload
                    </Button>
                </div>
            </Form>
        </section>
    );
}
