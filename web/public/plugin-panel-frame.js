(() => {
    const root = document.getElementById("root");
    const nonce = root?.dataset.frameNonce;
    if (!root || !nonce) return;

    const pending = new Map();
    let nextId = 1;
    let started = false;
    const sendReady = () => parent.postMessage({ type: "panel_frame_ready", nonce }, "*");
    const readyTimer = setInterval(() => {
        if (started) clearInterval(readyTimer);
        else sendReady();
    }, 75);
    sendReady();

    const rejectPending = (message) => {
        for (const item of pending.values()) item.reject(new Error(message));
        pending.clear();
    };

    window.addEventListener("message", async (event) => {
        if (event.source !== parent || !event.data || event.data.nonce !== nonce) return;
        const data = event.data;
        if (data.type === "panel_rpc_response") {
            const call = pending.get(data.id);
            if (!call) return;
            pending.delete(data.id);
            data.ok ? call.resolve(data.result) : call.reject(new Error(data.error || "Panel RPC failed"));
            return;
        }
        if (data.type !== "panel_boot" || started || typeof data.source !== "string") return;
        started = true;
        clearInterval(readyTimer);

        const bridge = {
            fetchJson(method, path, body) {
                return new Promise((resolve, reject) => {
                    const id = String(nextId++);
                    pending.set(id, { resolve, reject });
                    parent.postMessage({ type: "panel_rpc_request", nonce, id, method, path, body }, "*");
                });
            },
            usePoll(fetcher, intervalMs) {
                const React = window.__execlawReact;
                const [value, setValue] = React.useState(null);
                const [error, setError] = React.useState(null);
                React.useEffect(() => {
                    let active = true;
                    const tick = async () => {
                        try {
                            const result = await fetcher();
                            if (active) { setValue(result); setError(null); }
                        } catch (cause) {
                            if (active) setError(cause instanceof Error ? cause.message : String(cause));
                        }
                    };
                    void tick();
                    if (intervalMs > 0) {
                        const timer = setInterval(() => void tick(), intervalMs);
                        return () => { active = false; clearInterval(timer); };
                    }
                    return () => { active = false; };
                }, [fetcher, intervalMs]);
                return { value, error };
            },
        };

        try {
            const url = URL.createObjectURL(new Blob([data.source], { type: "text/javascript" }));
            const panelModule = await import(url);
            URL.revokeObjectURL(url);
            const React = panelModule.React;
            const ReactDOM = panelModule.ReactDOM;
            window.__execlawReact = React;
            bridge.React = React;
            bridge.ReactDOM = ReactDOM;
            bridge.components = {
                ErrorBanner: ({ message, onDismiss, className }) => message
                    ? React.createElement("div", { role: "alert", className: "alert alert-danger " + (className || "") },
                        React.createElement("span", null, message),
                        React.createElement("button", { type: "button", className: "btn btn-sm", onClick: onDismiss, "aria-label": "Dismiss" }, "×"))
                    : null,
                SidecarStatusBlock: (props) => React.createElement("section", { className: "execlaw-card" },
                    React.createElement("strong", null, props.sidecarLabel),
                    React.createElement("div", null, props.status),
                    props.rpcUrl ? React.createElement("code", null, props.rpcUrl) : null,
                    props.fetchError ? React.createElement("div", { className: "alert alert-danger" }, props.fetchError) : null,
                    props.followupHint || null),
                Button: ({ children, variant, size, ...props }) => React.createElement("button", {
                    ...props,
                    className: "btn " + (variant ? "btn-" + variant : "") + " " + (size ? "btn-" + size : "") + " " + (props.className || ""),
                }, children),
            };
            if (typeof panelModule.default !== "function") throw new Error("Panel entry must default-export a React component");
            ReactDOM.createRoot(root).render(React.createElement(panelModule.default, { identity: data.identity, bridge }));
            parent.postMessage({ type: "panel_frame_mounted", nonce }, "*");
        } catch (error) {
            rejectPending("Panel failed to load");
            parent.postMessage({ type: "panel_frame_error", nonce, error: error instanceof Error ? error.message : String(error) }, "*");
        }
    });
})();
