import type {
    PluginPanelComponent,
    PluginPanelProps,
} from "@execlaw/plugin-ui";

const React = globalThis.execlawHost!.React;
const { useCallback, useEffect, useState } = React;

type DomainPolicy = "allow_all" | "allow_list";

interface ScraperConfig {
    domain_policy: DomainPolicy;
    default_allowed_domains: string[];
    limits: {
        fetch_timeout_default_ms: number;
        fetch_timeout_max_ms: number;
        fetch_max_chars_default: number;
        fetch_max_chars_max: number;
        extract_timeout_default_ms: number;
        extract_timeout_max_ms: number;
        extract_max_chars_default: number;
        extract_max_chars_max: number;
        crawl_timeout_default_ms: number;
        crawl_timeout_max_ms: number;
        crawl_max_pages_default: number;
        crawl_max_pages_max: number;
        crawl_max_depth_default: number;
        crawl_max_depth_max: number;
    };
}

interface TestResponse {
    ok?: boolean;
    health?: Record<string, unknown>;
    probe?: {
        final_url?: string;
        status?: number;
        truncated?: boolean;
    };
}

const DEFAULT_CONFIG: ScraperConfig = {
    domain_policy: "allow_all",
    default_allowed_domains: [],
    limits: {
        fetch_timeout_default_ms: 15000,
        fetch_timeout_max_ms: 120000,
        fetch_max_chars_default: 6000,
        fetch_max_chars_max: 50000,
        extract_timeout_default_ms: 30000,
        extract_timeout_max_ms: 180000,
        extract_max_chars_default: 12000,
        extract_max_chars_max: 100000,
        crawl_timeout_default_ms: 60000,
        crawl_timeout_max_ms: 300000,
        crawl_max_pages_default: 5,
        crawl_max_pages_max: 25,
        crawl_max_depth_default: 1,
        crawl_max_depth_max: 3,
    },
};

function parseDomains(input: string): string[] {
    return Array.from(
        new Set(
            input
                .split(/[\n,\s]+/)
                .map((s) => s.trim().toLowerCase())
                .filter(Boolean),
        ),
    );
}

function toNumber(value: string, fallback: number): number {
    const n = Number(value);
    if (!Number.isFinite(n)) return fallback;
    return Math.trunc(n);
}

const Panel: PluginPanelComponent = (props: PluginPanelProps) => {
    const { bridge } = props;
    const { ErrorBanner, Button } = bridge.components;

    const [loading, setLoading] = useState(true);
    const [saving, setSaving] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [savedNotice, setSavedNotice] = useState<string | null>(null);
    const [testNotice, setTestNotice] = useState<string | null>(null);

    const [domainPolicy, setDomainPolicy] = useState<DomainPolicy>("allow_all");
    const [domainText, setDomainText] = useState("");

    const [limits, setLimits] = useState<Record<string, string>>({});

    const apply = useCallback((cfg: ScraperConfig) => {
        setDomainPolicy(cfg.domain_policy ?? "allow_all");
        setDomainText((cfg.default_allowed_domains ?? []).join("\n"));
        setLimits({
            fetch_timeout_default_ms: String(cfg.limits.fetch_timeout_default_ms),
            fetch_timeout_max_ms: String(cfg.limits.fetch_timeout_max_ms),
            fetch_max_chars_default: String(cfg.limits.fetch_max_chars_default),
            fetch_max_chars_max: String(cfg.limits.fetch_max_chars_max),
            extract_timeout_default_ms: String(cfg.limits.extract_timeout_default_ms),
            extract_timeout_max_ms: String(cfg.limits.extract_timeout_max_ms),
            extract_max_chars_default: String(cfg.limits.extract_max_chars_default),
            extract_max_chars_max: String(cfg.limits.extract_max_chars_max),
            crawl_timeout_default_ms: String(cfg.limits.crawl_timeout_default_ms),
            crawl_timeout_max_ms: String(cfg.limits.crawl_timeout_max_ms),
            crawl_max_pages_default: String(cfg.limits.crawl_max_pages_default),
            crawl_max_pages_max: String(cfg.limits.crawl_max_pages_max),
            crawl_max_depth_default: String(cfg.limits.crawl_max_depth_default),
            crawl_max_depth_max: String(cfg.limits.crawl_max_depth_max),
        });
    }, []);

    const reload = useCallback(async () => {
        setLoading(true);
        setError(null);
        try {
            const cfg = await bridge.fetchJson<ScraperConfig>(
                "GET",
                "/api/admin/plugins/web-scraper/config",
            );
            apply({ ...DEFAULT_CONFIG, ...cfg, limits: { ...DEFAULT_CONFIG.limits, ...(cfg.limits ?? {}) } });
        } catch (e) {
            setError(e instanceof Error ? e.message : "Could not load config");
            apply(DEFAULT_CONFIG);
        } finally {
            setLoading(false);
        }
    }, [apply, bridge]);

    useEffect(() => {
        void reload();
    }, [reload]);

    const onSave = useCallback(async () => {
        setSaving(true);
        setError(null);
        setSavedNotice(null);
        setTestNotice(null);
        try {
            const payload = {
                domain_policy: domainPolicy,
                default_allowed_domains: parseDomains(domainText),
                fetch_timeout_default_ms: toNumber(
                    limits.fetch_timeout_default_ms ?? "",
                    DEFAULT_CONFIG.limits.fetch_timeout_default_ms,
                ),
                fetch_timeout_max_ms: toNumber(
                    limits.fetch_timeout_max_ms ?? "",
                    DEFAULT_CONFIG.limits.fetch_timeout_max_ms,
                ),
                fetch_max_chars_default: toNumber(
                    limits.fetch_max_chars_default ?? "",
                    DEFAULT_CONFIG.limits.fetch_max_chars_default,
                ),
                fetch_max_chars_max: toNumber(
                    limits.fetch_max_chars_max ?? "",
                    DEFAULT_CONFIG.limits.fetch_max_chars_max,
                ),
                extract_timeout_default_ms: toNumber(
                    limits.extract_timeout_default_ms ?? "",
                    DEFAULT_CONFIG.limits.extract_timeout_default_ms,
                ),
                extract_timeout_max_ms: toNumber(
                    limits.extract_timeout_max_ms ?? "",
                    DEFAULT_CONFIG.limits.extract_timeout_max_ms,
                ),
                extract_max_chars_default: toNumber(
                    limits.extract_max_chars_default ?? "",
                    DEFAULT_CONFIG.limits.extract_max_chars_default,
                ),
                extract_max_chars_max: toNumber(
                    limits.extract_max_chars_max ?? "",
                    DEFAULT_CONFIG.limits.extract_max_chars_max,
                ),
                crawl_timeout_default_ms: toNumber(
                    limits.crawl_timeout_default_ms ?? "",
                    DEFAULT_CONFIG.limits.crawl_timeout_default_ms,
                ),
                crawl_timeout_max_ms: toNumber(
                    limits.crawl_timeout_max_ms ?? "",
                    DEFAULT_CONFIG.limits.crawl_timeout_max_ms,
                ),
                crawl_max_pages_default: toNumber(
                    limits.crawl_max_pages_default ?? "",
                    DEFAULT_CONFIG.limits.crawl_max_pages_default,
                ),
                crawl_max_pages_max: toNumber(
                    limits.crawl_max_pages_max ?? "",
                    DEFAULT_CONFIG.limits.crawl_max_pages_max,
                ),
                crawl_max_depth_default: toNumber(
                    limits.crawl_max_depth_default ?? "",
                    DEFAULT_CONFIG.limits.crawl_max_depth_default,
                ),
                crawl_max_depth_max: toNumber(
                    limits.crawl_max_depth_max ?? "",
                    DEFAULT_CONFIG.limits.crawl_max_depth_max,
                ),
            };
            await bridge.fetchJson("POST", "/api/admin/plugins/web-scraper/config", payload);
            setSavedNotice("Saved.");
            await reload();
        } catch (e) {
            setError(e instanceof Error ? e.message : "Save failed");
        } finally {
            setSaving(false);
        }
    }, [bridge, domainPolicy, domainText, limits, reload]);

    const onTest = useCallback(async () => {
        setSaving(true);
        setError(null);
        setTestNotice(null);
        try {
            const r = await bridge.fetchJson<TestResponse>(
                "POST",
                "/api/admin/plugins/web-scraper/test",
            );
            if (!r.ok) {
                setTestNotice("Test did not return ok=true.");
            } else {
                const status = r.probe?.status ?? "?";
                setTestNotice(`Sidecar responded; probe status ${status}.`);
            }
        } catch (e) {
            setError(e instanceof Error ? e.message : "Test failed");
        } finally {
            setSaving(false);
        }
    }, [bridge]);

    const updateLimit = (k: string, v: string) => {
        setLimits((prev) => ({ ...prev, [k]: v }));
    };

    if (loading) {
        return (
            <div className="d-flex align-items-center execlaw-muted">
                <span className="spinner-border spinner-border-sm me-2" role="status" aria-hidden />
                Loading...
            </div>
        );
    }

    const L = [
        ["fetch_timeout_default_ms", "Fetch timeout default (ms)"],
        ["fetch_timeout_max_ms", "Fetch timeout max (ms)"],
        ["fetch_max_chars_default", "Fetch max chars default"],
        ["fetch_max_chars_max", "Fetch max chars max"],
        ["extract_timeout_default_ms", "Extract timeout default (ms)"],
        ["extract_timeout_max_ms", "Extract timeout max (ms)"],
        ["extract_max_chars_default", "Extract max chars default"],
        ["extract_max_chars_max", "Extract max chars max"],
        ["crawl_timeout_default_ms", "Crawl timeout default (ms)"],
        ["crawl_timeout_max_ms", "Crawl timeout max (ms)"],
        ["crawl_max_pages_default", "Crawl max pages default"],
        ["crawl_max_pages_max", "Crawl max pages max"],
        ["crawl_max_depth_default", "Crawl max depth default"],
        ["crawl_max_depth_max", "Crawl max depth max"],
    ] as const;

    return (
        <div>
            <ErrorBanner message={error} onDismiss={() => setError(null)} className="mb-3" />

            <div className="card mb-3">
                <div className="card-body">
                    <h5 className="h6 mb-2">Domain Policy</h5>
                    <p className="execlaw-muted small mb-3">
                        allow_all permits any public domain unless a call supplies allowed_domains. allow_list enforces this plugin-level allow list for every tool call.
                    </p>

                    {savedNotice && <div className="alert alert-success">{savedNotice}</div>}
                    {testNotice && <div className="alert alert-info">{testNotice}</div>}

                    <div className="mb-2">
                        <label className="form-label execlaw-muted small mb-1">Policy</label>
                        <select
                            className="form-select"
                            value={domainPolicy}
                            onChange={(e: { target: { value: DomainPolicy } }) => setDomainPolicy(e.target.value)}
                        >
                            <option value="allow_all">allow_all</option>
                            <option value="allow_list">allow_list</option>
                        </select>
                    </div>

                    <div className="mb-2">
                        <label className="form-label execlaw-muted small mb-1">Default allowed domains</label>
                        <textarea
                            className="form-control"
                            rows={5}
                            placeholder="example.com\nnews.ycombinator.com"
                            value={domainText}
                            onChange={(e: { target: { value: string } }) => setDomainText(e.target.value)}
                        />
                        <div className="form-text execlaw-muted">
                            One domain per line (or comma-separated). Used as call default, and mandatory when policy is allow_list.
                        </div>
                    </div>
                </div>
            </div>

            <div className="card mb-3">
                <div className="card-body">
                    <h5 className="h6 mb-2">Per-Tool Limits</h5>
                    <div className="row g-2">
                        {L.map(([key, label]) => (
                            <div className="col-md-6" key={key}>
                                <label className="form-label execlaw-muted small mb-1">{label}</label>
                                <input
                                    type="number"
                                    className="form-control"
                                    value={limits[key] ?? ""}
                                    onChange={(e: { target: { value: string } }) => updateLimit(key, e.target.value)}
                                />
                            </div>
                        ))}
                    </div>
                </div>
            </div>

            <div className="d-flex gap-2">
                <Button onClick={() => void onSave()} disabled={saving}>Save</Button>
                <Button onClick={() => void onTest()} disabled={saving} variant="secondary">Test Sidecar</Button>
            </div>
        </div>
    );
};

export default Panel;
