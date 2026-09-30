import * as React from "react";
import type { PluginPanelComponent } from "../../src/plugins/types";

const Panel: PluginPanelComponent = ({ bridge }) => {
    const [state, setState] = React.useState<Record<string, string>>({});
    React.useEffect(() => {
        let localStorageBlocked = false;
        let parentDomBlocked = false;
        let cookieBlocked = false;
        try { localStorage.getItem("execlaw.access_token"); } catch { localStorageBlocked = true; }
        try { parent.document.body; } catch { parentDomBlocked = true; }
        try { cookieBlocked = !document.cookie.includes("parent-cookie-secret"); } catch { cookieBlocked = true; }
        fetch("/api/admin/users")
            .then(() => setState((current) => ({ ...current, directFetch: "allowed" })))
            .catch(() => setState((current) => ({ ...current, directFetch: "blocked" })));
        bridge.fetchJson<{ status: string }>("GET", "/api/admin/plugins/fixture/status")
            .then((result) => setState((current) => ({ ...current, allowedRpc: result.status })))
            .catch((error) => setState((current) => ({ ...current, allowedRpc: error.message })));
        bridge.fetchJson("POST", "/api/admin/users", { role: "controller" })
            .then(() => setState((current) => ({ ...current, unrelatedRpc: "allowed" })))
            .catch(() => setState((current) => ({ ...current, unrelatedRpc: "blocked" })));
        setState((current) => ({
            ...current,
            localStorage: localStorageBlocked ? "blocked" : "readable",
            parentDom: parentDomBlocked ? "blocked" : "readable",
            cookies: cookieBlocked ? "blocked" : "readable",
        }));
    }, [bridge]);

    return React.createElement("pre", { id: "panel-results" }, JSON.stringify(state));
};

export default Panel;
