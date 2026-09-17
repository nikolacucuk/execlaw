const React = globalThis.execlawHost.React;
const { useEffect, useState } = React;

const pluginId = "obsidian-livesync-publisher";
const path = (suffix) => `/api/admin/plugins/${pluginId}${suffix}`;

function field(label, value, onChange, type = "text", help = "") {
    return React.createElement(
        "label",
        { className: "mb-3 d-block" },
        React.createElement("span", { className: "form-label" }, label),
        React.createElement("input", {
            className: "form-control",
            type,
            value: value ?? "",
            onChange: (event) => onChange(event.target.value),
            autoComplete: type === "password" ? "new-password" : "off",
        }),
        help ? React.createElement("span", { className: "form-text" }, help) : null,
    );
}

function normalizeSource(value) {
    let source = String(value ?? "").trim().replaceAll("\\", "/");
    const prefix = "/mnt/AI_Pool/";
    if (source.startsWith(prefix)) source = source.slice(prefix.length);
    if (source === "execlaw") source = "obsidian-vault/execlaw";
    return source;
}

export default function ObsidianPublisherPanel({ bridge }) {
    const { Button, ErrorBanner } = bridge.components;
    const [config, setConfig] = useState({
        couchdb_url: "http://couchdb-obsidian-livesync:5984",
        database: "djenka_db",
        username: "ncucuk",
        password: "",
        livesync_passphrase: "",
        path_obfuscation_passphrase: "",
        source_subdir: "obsidian-vault/execlaw",
        max_files: 1000,
        max_bytes: 52428800,
    });
    const [loading, setLoading] = useState(true);
    const [saving, setSaving] = useState(false);
    const [publishing, setPublishing] = useState(false);
    const [checking, setChecking] = useState(false);
    const [message, setMessage] = useState(null);
    const [error, setError] = useState(null);

    useEffect(() => {
        let active = true;
        bridge.fetchJson("GET", path("/config"))
            .then((value) => {
                if (!active) return;
                setConfig((current) => ({
                    ...current,
                    ...value,
                    password: value.password === "<redacted>" ? "<redacted>" : "",
                    livesync_passphrase: value.livesync_passphrase === "<redacted>" ? "<redacted>" : "",
                    path_obfuscation_passphrase: value.path_obfuscation_passphrase === "<redacted>" ? "<redacted>" : "",
                }));
            })
            .catch((value) => active && setError(String(value)))
            .finally(() => active && setLoading(false));
        return () => { active = false; };
    }, []);

    async function save() {
        setSaving(true);
        setMessage(null);
        setError(null);
        try {
            const body = { ...config };
            body.source_subdir = normalizeSource(body.source_subdir);
            for (const key of ["password", "livesync_passphrase", "path_obfuscation_passphrase"]) {
                if (!body[key]) delete body[key];
            }
            const result = await bridge.fetchJson("POST", path("/config"), body);
            setConfig((current) => ({
                ...current,
                ...result.config,
                password: "<redacted>",
                livesync_passphrase: "<redacted>",
                path_obfuscation_passphrase: "<redacted>",
            }));
            setMessage("Configuration saved. Password is stored in the encrypted plugin vault.");
        } catch (value) {
            setError(String(value));
        } finally {
            setSaving(false);
        }
    }

    async function publish() {
        setPublishing(true);
        setMessage(null);
        setError(null);
        try {
            const result = await bridge.fetchJson("POST", path("/publish"), {});
            setMessage(`Publish complete: ${result.files_seen ?? 0} files seen, ${result.metadata_written ?? 0} metadata documents written, ${result.files_unchanged ?? 0} unchanged, ${result.chunks_written ?? 0} content chunks written.`);
        } catch (value) {
            setError(String(value));
        } finally {
            setPublishing(false);
        }
    }

    async function checkSource() {
        setChecking(true);
        setMessage(null);
        setError(null);
        try {
            const result = await bridge.fetchJson("GET", path("/test"));
            setMessage(`Source ready: ${result.source}; ${result.markdown_files} markdown file(s) found.`);
        } catch (value) {
            setError(String(value));
        } finally {
            setChecking(false);
        }
    }

    if (loading) return React.createElement("div", { className: "execlaw-muted" }, "Loading publisher configuration...");

    const update = (key, value) => setConfig((current) => ({ ...current, [key]: value }));
    const sourceHelp = "Relative to /ai_pool. Use obsidian-vault/execlaw (or obsidian_vault/execlaw if that is the actual TrueNAS folder), not /mnt/AI_Pool/...";
    return React.createElement(
        "div",
        { className: "execlaw-plugin-panel" },
        error ? React.createElement(ErrorBanner, { message: error, dismissAfterMs: 0, onDismiss: () => setError(null) }) : null,
        message ? React.createElement("div", { className: "alert alert-success", role: "status" }, message) : null,
        React.createElement("p", { className: "execlaw-muted" }, "Publish markdown from the read-only TrueNAS export directory into the existing LiveSync CouchDB database. The publisher never edits CouchDB files directly and never deletes remote documents."),
        field("CouchDB URL", config.couchdb_url, (value) => update("couchdb_url", value), "url", "Use the CouchDB service name when execlaw and CouchDB share a Docker network."),
        field("Database", config.database, (value) => update("database", value)),
        field("Username", config.username, (value) => update("username", value), "text", "Defaults to ncucuk from the current CouchDB deployment."),
        field("Password", config.password, (value) => update("password", value), "password", "Leave blank only when a password was already saved."),
        field("LiveSync encryption passphrase", config.livesync_passphrase, (value) => update("livesync_passphrase", value), "password", "Required for the connected encrypted LiveSync profile. Stored in the encrypted plugin vault."),
        field("Path obfuscation passphrase", config.path_obfuscation_passphrase, (value) => update("path_obfuscation_passphrase", value), "password", "Leave blank to use the LiveSync encryption passphrase."),
        field("Vault source folder", config.source_subdir, (value) => update("source_subdir", value), "text", "Relative to /ai_pool. Use obsidian-vault/execlaw, or enter /mnt/AI_Pool/obsidian-vault/execlaw and it will be normalized."),
        field("Maximum files per publish", String(config.max_files), (value) => update("max_files", Number(value) || 1000), "number"),
        field("Maximum bytes per publish", String(config.max_bytes), (value) => update("max_bytes", Number(value) || 52428800), "number"),
        React.createElement("div", { className: "d-flex gap-2" },
            React.createElement(Button, { onClick: save, disabled: saving }, saving ? "Saving..." : "Save configuration"),
            React.createElement(Button, { onClick: checkSource, disabled: checking }, checking ? "Checking..." : "Check source"),
            React.createElement(Button, { onClick: publish, disabled: publishing, variant: "primary" }, publishing ? "Publishing..." : "Publish now"),
        ),
    );
}
