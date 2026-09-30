import type { UiPanelSummary } from "../api/endpoints";

const ALLOWED_METHODS = new Set(["GET", "POST", "PUT", "PATCH", "DELETE"]);
const ROUTE_SEGMENT = /^\{[A-Za-z][A-Za-z0-9_]*\}$/;

function decodePath(path: string): string[] | null {
    if (!path.startsWith("/") || path.startsWith("//") || path.includes("\\") || path.includes("#")) {
        return null;
    }
    const pathname = path.split("?", 1)[0];
    try {
        const decoded = pathname.split("/").map((part) => decodeURIComponent(part));
        if (decoded.some((part) => part === "." || part === ".." || part.includes("/") || part.includes("\\"))) {
            return null;
        }
        return decoded;
    } catch {
        return null;
    }
}

function matchesRouteTemplate(template: string, path: string): boolean {
    const expected = decodePath(template);
    const actual = decodePath(path);
    if (!expected || !actual || expected.length !== actual.length) return false;
    return expected.every((segment, index) =>
        ROUTE_SEGMENT.test(segment) ? actual[index].length > 0 : segment === actual[index],
    );
}

/** Return whether one frame request matches an exact manifest-declared RPC grant. */
export function isAllowedPanelRpc(
    policy: UiPanelSummary,
    method: string,
    path: string,
): boolean {
    const normalizedMethod = method.toUpperCase();
    if (!ALLOWED_METHODS.has(normalizedMethod)) return false;
    if (policy.rpc_routes.some((route) =>
        route.method.toUpperCase() === normalizedMethod && matchesRouteTemplate(route.path, path),
    )) return true;

    if (
        policy.rpc_capabilities.includes("own_plugin_settings") &&
        ["GET", "PUT"].includes(normalizedMethod) &&
        matchesRouteTemplate(
            `/api/admin/plugins/${policy.plugin_id}/settings/{key}`,
            path,
        )
    ) return true;
    if (
        policy.rpc_capabilities.includes("own_sidecar_status") &&
        normalizedMethod === "GET" &&
        path === `/api/admin/plugins/${policy.plugin_id}/sidecars`
    ) return true;
    if (
        policy.rpc_capabilities.includes("controller_identifiers") &&
        normalizedMethod === "GET" &&
        path === `/api/admin/plugins/${policy.plugin_id}/identifiers`
    ) return true;

    if (!policy.rpc_capabilities.includes("own_oauth_accounts")) return false;
    const actual = decodePath(path);
    if (!actual) return false;
    const [empty, api, admin, oauth, clients, pluginId, account, action] = actual;
    if (empty !== "" || api !== "api" || admin !== "admin" || oauth !== "oauth" || clients !== "clients") {
        return false;
    }
    if (pluginId !== policy.plugin_id || !account) return false;
    const declaredAccounts = policy.oauth_accounts ?? [];
    if (!declaredAccounts.includes(account)) return false;
    if (action === undefined) return ["GET", "PUT", "DELETE"].includes(normalizedMethod);
    return ["connect", "disconnect"].includes(action) && normalizedMethod === "POST";
}

/** Accept frame messages only from the mounted sandbox and its boot nonce. */
export function isPanelFrameMessageTrusted(
    event: Pick<MessageEvent, "source" | "data">,
    frame: Window | null,
    nonce: string,
): boolean {
    return event.source === frame && Boolean(event.data) && event.data.nonce === nonce;
}

/** Static sandbox bootstrap. Plugin source arrives later over a nonce-bound message. */
export function pluginPanelFrameDocument(nonce: string, hostOrigin: string): string {
    const nonceValue = nonce.replace(/[^A-Za-z0-9-]/g, "");
    const scriptUrl = new URL("/plugin-panel-frame.js", hostOrigin).href;
    const scriptCspSource = scriptUrl.replace(/&/g, "&amp;").replace(/"/g, "&quot;");
    return `<!doctype html>
<html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src ${scriptCspSource} blob:; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; connect-src 'none'; form-action 'none'; base-uri 'none'; object-src 'none'; frame-src 'none'">
<style>
html,body,#root{margin:0;min-height:100%;font:14px/1.45 system-ui,sans-serif;color:#212529;background:#fff}
body{padding:1rem}.execlaw-card{border:1px solid #dee2e6;border-radius:.5rem;padding:1rem;margin-bottom:1rem}
.execlaw-card__title{font-weight:600;margin-bottom:.75rem}.execlaw-muted{color:#6c757d}.small{font-size:.875em}
.d-flex{display:flex}.flex-wrap{flex-wrap:wrap}.gap-2{gap:.5rem}.gap-3{gap:1rem}.mb-1{margin-bottom:.25rem}.mb-2{margin-bottom:.5rem}.mb-3{margin-bottom:1rem}.mt-2{margin-top:.5rem}
.form-control,.form-select{display:block;width:100%;box-sizing:border-box;padding:.375rem .75rem;border:1px solid #ced4da;border-radius:.375rem;background:#fff;color:#212529}
.form-label{display:inline-block;margin-bottom:.5rem}.form-check{display:flex;align-items:center;gap:.5rem}.form-switch{display:inline-flex}.form-check-input{width:2em;height:1em}
.btn{display:inline-block;padding:.375rem .75rem;border:1px solid transparent;border-radius:.375rem;background:#f8f9fa;color:#212529;cursor:pointer}.btn:disabled{opacity:.6;cursor:default}
.btn-primary{background:#0d6efd;color:white}.btn-danger{background:#dc3545;color:white}.btn-outline-primary{border-color:#0d6efd;color:#0d6efd}.btn-outline-secondary{border-color:#6c757d;color:#6c757d}.btn-outline-danger{border-color:#dc3545;color:#dc3545}.btn-outline-success{border-color:#198754;color:#198754}.btn-outline-warning{border-color:#ffc107;color:#664d03}
.alert{padding:.75rem 1rem;border-radius:.375rem}.alert-danger{background:#f8d7da;color:#842029}.alert-success{background:#d1e7dd;color:#0f5132}
</style></head><body><div id="root" data-frame-nonce="${nonceValue}"></div><script src="${scriptCspSource}" defer></script></body></html>`;
}
