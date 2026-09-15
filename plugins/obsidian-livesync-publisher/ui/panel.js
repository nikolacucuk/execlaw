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

export default function ObsidianPublisherPanel({ bridge }) {
    const { Button, ErrorBanner } = bridge.components;
    const [config, setConfig] = useState({
        couchdb_url: "http://couchdb-obsidian-livesync:5984",
        database: "djenka_db",
        username: "ncucuk",
        password: "",
        max_files: 1000,
        max_bytes: 52428800,
    });
    const [loading, setLoading] = useState(true);
    const [saving, setSaving] = useState(false);
    const [publishing, setPublishing] = useState(false);
    const [message, setMessage] = useState(null);
    const [error, setError] = useState(null);

    useEffect(() => {
        let active = true;
        bridge.fetchJson("GET", path("/config"))
            .then((value) => {
                if (!active) return;
                setConfig((current) => ({ ...current, ...value, password: "" }));
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
            if (!body.password) delete body.password;
            const result = await bridge.fetchJson("POST", path("/config"), body);
            setConfig((current) => ({ ...current, ...result.config, password: "" }));
            setMessage("Configuration saved. The password is stored in the encrypted plugin vault.");
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
            setMessage(`Publish complete: ${result.files_seen ?? 0} files seen, ${result.metadata_written ?? 0} metadata documents written.`);
        } catch (value) {
            setError(String(value));
        } finally {
            setPublishing(false);
        }
    }

    if (loading) return React.createElement("div", { className: "execlaw-muted" }, "Loading publisher configuration...");

    const update = (key, value) => setConfig((current) => ({ ...current, [key]: value }));
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
        field("Maximum files per publish", String(config.max_files), (value) => update("max_files", Number(value) || 1000), "number"),
        field("Maximum bytes per publish", String(config.max_bytes), (value) => update("max_bytes", Number(value) || 52428800), "number"),
        React.createElement("div", { className: "d-flex gap-2" },
            React.createElement(Button, { onClick: save, disabled: saving }, saving ? "Saving..." : "Save configuration"),
            React.createElement(Button, { onClick: publish, disabled: publishing, variant: "primary" }, publishing ? "Publishing..." : "Publish now"),
        ),
    );
}
