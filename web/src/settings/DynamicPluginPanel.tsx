import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { useAuth } from "../auth/AuthContext";
import { ErrorBanner } from "../components/ErrorBanner";
import { listUiPanels, type UiPanelSummary } from "../api/endpoints";
import { randomUuid } from "../api/randomId";
import {
    isAllowedPanelRpc,
    isPanelFrameMessageTrusted,
    pluginPanelFrameDocument,
} from "../plugins/panel-sandbox";

interface Props {
    pluginId: string;
    pluginVersion: string;
    pluginDisplayName: string;
    onConfigChanged: () => void;
    staticFallback?: () => ReactNode;
}

type LoadState =
    | { kind: "loading" }
    | { kind: "no-panel" }
    | { kind: "ready"; source: string; policy: UiPanelSummary; nonce: string }
    | { kind: "error"; message: string };

const MAX_PANEL_RPC_BODY_BYTES = 1024 * 1024;

export function DynamicPluginPanel({
    pluginId,
    pluginVersion,
    pluginDisplayName,
    onConfigChanged: _onConfigChanged,
    staticFallback,
}: Props) {
    const { getAccessToken, status: authStatus } = useAuth();
    const [state, setState] = useState<LoadState>({ kind: "loading" });
    const [reloadKey, setReloadKey] = useState(0);
    const iframeRef = useRef<HTMLIFrameElement | null>(null);
    const triggerRetry = useCallback(() => setReloadKey((value) => value + 1), []);

    useEffect(() => {
        if (authStatus === "loading") {
            setState({ kind: "loading" });
            return;
        }
        let cancelled = false;
        setState({ kind: "loading" });
        void (async () => {
            try {
                const token = getAccessToken();
                if (!token) throw new Error("operator session is unavailable");
                const response = await listUiPanels(getAccessToken);
                const policy = (response.panels ?? []).find((panel) => panel.plugin_id === pluginId);
                if (!policy) {
                    if (!cancelled) setState({ kind: "no-panel" });
                    return;
                }
                const assetPath = `/api/admin/plugins/${encodeURIComponent(pluginId)}/${policy.entry}`;
                const asset = await fetch(assetPath, {
                    method: "GET",
                    headers: { Authorization: `Bearer ${token}` },
                });
                if (cancelled) return;
                if (asset.status === 404) {
                    setState({ kind: "no-panel" });
                    return;
                }
                if (!asset.ok) {
                    throw new Error(`panel asset request failed: ${asset.status} ${await asset.text()}`);
                }
                const source = await asset.text();
                if (cancelled) return;
                if (source.length > 8 * 1024 * 1024) throw new Error("panel bundle exceeds 8 MiB");
                setState({
                    kind: "ready",
                    source,
                    policy,
                    nonce: randomUuid(),
                });
            } catch (cause) {
                if (!cancelled) {
                    setState({
                        kind: "error",
                        message: cause instanceof Error ? cause.message : String(cause),
                    });
                }
            }
        })();
        return () => { cancelled = true; };
    }, [authStatus, getAccessToken, pluginId, reloadKey]);

    useEffect(() => {
        if (state.kind !== "ready") return;
        const frame = iframeRef.current;
        const target = frame?.contentWindow;
        if (!frame || !target) return;
        let bootSent = false;
        let disposed = false;
        const bootTimer = window.setTimeout(() => {
            if (!bootSent && !disposed) setState({
                kind: "error",
                message: "sandboxed panel frame did not start",
            });
        }, 15_000);

        const reply = (id: unknown, ok: boolean, payload: unknown) => {
            if (typeof id !== "string" || id.length > 96 || disposed) return;
            target.postMessage({
                type: "panel_rpc_response",
                nonce: state.nonce,
                id,
                ok,
                ...(ok ? { result: payload } : { error: String(payload) }),
            }, "*");
        };

        const handleMessage = async (event: MessageEvent) => {
            if (!isPanelFrameMessageTrusted(event, target, state.nonce)) return;
            const message = event.data as Record<string, unknown>;
            if (message.type === "panel_frame_ready") {
                if (bootSent) return;
                bootSent = true;
                window.clearTimeout(bootTimer);
                target.postMessage({
                    type: "panel_boot",
                    nonce: state.nonce,
                    source: state.source,
                    identity: {
                        id: pluginId,
                        displayName: pluginDisplayName,
                        version: pluginVersion,
                    },
                }, "*");
                return;
            }
            if (message.type === "panel_frame_error") {
                setState({ kind: "error", message: String(message.error ?? "Panel failed to load") });
                return;
            }
            if (message.type !== "panel_rpc_request") return;

            const id = message.id;
            const method = typeof message.method === "string" ? message.method : "";
            const path = typeof message.path === "string" ? message.path : "";
            if (!isAllowedPanelRpc(state.policy, method, path)) {
                reply(id, false, "Panel RPC is not declared in this plugin manifest");
                return;
            }

            let body: string | undefined;
            if (message.body !== undefined && message.body !== null) {
                try {
                    body = JSON.stringify(message.body);
                } catch {
                    reply(id, false, "Panel RPC body is not JSON serializable");
                    return;
                }
                if (!body || new TextEncoder().encode(body).byteLength > MAX_PANEL_RPC_BODY_BYTES) {
                    reply(id, false, "Panel RPC body exceeds 1 MiB");
                    return;
                }
            }
            const token = getAccessToken();
            if (!token) {
                reply(id, false, "operator session is unavailable");
                return;
            }
            try {
                const response = await fetch(path, {
                    method: method.toUpperCase(),
                    headers: {
                        Authorization: `Bearer ${token}`,
                        ...(body ? { "Content-Type": "application/json" } : {}),
                    },
                    ...(body ? { body } : {}),
                });
                const text = await response.text();
                let result: unknown = text;
                try { result = text.length ? JSON.parse(text) as unknown : undefined; } catch { /* return text */ }
                if (!response.ok) {
                    reply(id, false, `${method.toUpperCase()} ${path} returned ${response.status}: ${text}`);
                    return;
                }
                reply(id, true, result);
            } catch (cause) {
                reply(id, false, cause instanceof Error ? cause.message : String(cause));
            }
        };

        window.addEventListener("message", handleMessage);
        return () => {
            disposed = true;
            window.clearTimeout(bootTimer);
            window.removeEventListener("message", handleMessage);
        };
    }, [getAccessToken, pluginDisplayName, pluginId, pluginVersion, state]);

    if (state.kind === "loading") {
        return <div className="execlaw-muted small" data-testid="plugin-panel-loading">Loading plugin UI…</div>;
    }
    if (state.kind === "no-panel") {
        return staticFallback ? <>{staticFallback()}</> : (
            <div className="execlaw-card" data-testid="plugin-panel-no-ui">
                <div className="execlaw-card__title">No configuration UI available</div>
                <div className="execlaw-muted small">Plugin <code>{pluginId}</code> does not declare a panel entry.</div>
            </div>
        );
    }
    if (state.kind === "error") {
        return (
            <div data-testid="plugin-panel-error">
                <ErrorBanner message={state.message} onDismiss={triggerRetry} dismissAfterMs={0} />
                <button type="button" className="btn btn-sm btn-outline-secondary" onClick={triggerRetry} data-testid="plugin-panel-retry">
                    Retry
                </button>
            </div>
        );
    }

    return (
        <iframe
            ref={iframeRef}
            title={`${pluginDisplayName} settings panel`}
            sandbox="allow-scripts"
            referrerPolicy="no-referrer"
            srcDoc={pluginPanelFrameDocument(state.nonce, window.location.origin)}
            className="w-100 border-0"
            style={{ minHeight: "28rem" }}
            data-testid="plugin-panel-frame"
        />
    );
}
