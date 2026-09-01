import type {
    PluginPanelComponent,
    PluginPanelProps,
} from "@execlaw/plugin-ui";

const React = globalThis.execlawHost!.React;
const { useCallback, useEffect, useMemo, useState } = React;

interface AutoResearchConfig {
    metric_name: string;
    memory_budget_gb: number;
    min_gain_threshold: number;
}

interface AutoResearchTest {
    ok?: boolean;
    note?: string;
    probe_decision?: {
        decision?: string;
        reason?: string;
    };
    config?: AutoResearchConfig;
}

interface TsvPoint {
    idx: number;
    val: number;
    status: string;
}

const Panel: PluginPanelComponent = (props: PluginPanelProps) => {
    const { bridge } = props;
    const { Button, ErrorBanner } = bridge.components;

    const [loading, setLoading] = useState(true);
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [saved, setSaved] = useState<string | null>(null);

    const [metricName, setMetricName] = useState("val_bpb");
    const [memoryBudgetGb, setMemoryBudgetGb] = useState("48");
    const [minGainThreshold, setMinGainThreshold] = useState("0.001");

    const [testResult, setTestResult] = useState<
        | { kind: "idle" }
        | { kind: "ok"; message: string }
        | { kind: "err"; message: string }
    >({ kind: "idle" });

    const [tsvText, setTsvText] = useState(
        "commit\tval_bpb\tmemory_gb\tstatus\tdescription\n" +
            "a1\t1.0100\t40.0\tkeep\tbaseline\n" +
            "b2\t1.0030\t41.0\tkeep\tlr tune\n" +
            "c3\t1.0045\t41.2\tdiscard\tover-regularized\n" +
            "d4\t0.9998\t42.1\tkeep\twindow ablation"
    );

    const reload = useCallback(async () => {
        setLoading(true);
        setError(null);
        try {
            const cfg = await bridge.fetchJson<AutoResearchConfig>(
                "GET",
                "/api/admin/plugins/autoresearch/config"
            );
            setMetricName(cfg.metric_name ?? "val_bpb");
            setMemoryBudgetGb(String(cfg.memory_budget_gb ?? 48));
            setMinGainThreshold(String(cfg.min_gain_threshold ?? 0.001));
        } catch (e) {
            setError(e instanceof Error ? e.message : "could not load config");
        } finally {
            setLoading(false);
        }
    }, [bridge]);

    useEffect(() => {
        void reload();
    }, [reload]);

    const onSave = useCallback(async () => {
        setBusy(true);
        setSaved(null);
        setError(null);
        setTestResult({ kind: "idle" });

        const mem = Number(memoryBudgetGb);
        const gain = Number(minGainThreshold);
        if (!Number.isFinite(mem) || mem <= 0) {
            setError("Memory budget must be a positive number.");
            setBusy(false);
            return;
        }
        if (!Number.isFinite(gain) || gain < 0) {
            setError("Min gain threshold must be >= 0.");
            setBusy(false);
            return;
        }

        try {
            const cfg = await bridge.fetchJson<AutoResearchConfig>(
                "POST",
                "/api/admin/plugins/autoresearch/config",
                {
                    metric_name: metricName.trim() || "val_bpb",
                    memory_budget_gb: mem,
                    min_gain_threshold: gain,
                }
            );
            setMetricName(cfg.metric_name);
            setMemoryBudgetGb(String(cfg.memory_budget_gb));
            setMinGainThreshold(String(cfg.min_gain_threshold));
            setSaved("Saved AutoResearch defaults.");
        } catch (e) {
            setError(e instanceof Error ? e.message : "save failed");
        } finally {
            setBusy(false);
        }
    }, [bridge, memoryBudgetGb, metricName, minGainThreshold]);

    const onTest = useCallback(async () => {
        setBusy(true);
        setError(null);
        setTestResult({ kind: "idle" });
        try {
            const r = await bridge.fetchJson<AutoResearchTest>(
                "POST",
                "/api/admin/plugins/autoresearch/test"
            );
            if (!r.ok) {
                setTestResult({ kind: "err", message: "Self-test did not report ok." });
            } else {
                const d = r.probe_decision?.decision ?? "unknown";
                const why = r.probe_decision?.reason ?? "";
                setTestResult({ kind: "ok", message: `Self-test decision: ${d}. ${why}` });
            }
        } catch (e) {
            setTestResult({
                kind: "err",
                message: e instanceof Error ? e.message : String(e),
            });
        } finally {
            setBusy(false);
        }
    }, [bridge]);

    const points = useMemo(() => parseTsvPoints(tsvText), [tsvText]);
    const chart = useMemo(() => buildChart(points), [points]);

    if (loading) {
        return (
            <div className="d-flex align-items-center execlaw-muted">
                <span className="spinner-border spinner-border-sm me-2" role="status" aria-hidden />
                Loading…
            </div>
        );
    }

    return (
        <div data-testid="autoresearch-config-page">
            <ErrorBanner message={error} onDismiss={() => setError(null)} className="mb-3" />

            <div className="card mb-3">
                <div className="card-body">
                    <h5 className="h6 mb-2">AutoResearch defaults</h5>
                    <p className="execlaw-muted small mb-3">
                        These defaults are used by the AutoResearch plugin for candidate scoring and planning.
                    </p>

                    {saved && <div className="alert alert-success">{saved}</div>}

                    <div className="row g-2 mb-2">
                        <div className="col-md-4">
                            <label className="form-label execlaw-muted small mb-1">Metric name</label>
                            <input
                                className="form-control"
                                value={metricName}
                                onChange={(e: { target: { value: string } }) => setMetricName(e.target.value)}
                                placeholder="val_bpb"
                                data-testid="autoresearch-metric-name"
                            />
                        </div>
                        <div className="col-md-4">
                            <label className="form-label execlaw-muted small mb-1">Memory budget (GB)</label>
                            <input
                                type="number"
                                step="0.1"
                                className="form-control"
                                value={memoryBudgetGb}
                                onChange={(e: { target: { value: string } }) => setMemoryBudgetGb(e.target.value)}
                                data-testid="autoresearch-memory-budget"
                            />
                        </div>
                        <div className="col-md-4">
                            <label className="form-label execlaw-muted small mb-1">Min gain threshold</label>
                            <input
                                type="number"
                                step="0.0001"
                                className="form-control"
                                value={minGainThreshold}
                                onChange={(e: { target: { value: string } }) => setMinGainThreshold(e.target.value)}
                                data-testid="autoresearch-min-gain"
                            />
                        </div>
                    </div>

                    <div className="d-flex gap-2">
                        <Button variant="primary" size="sm" onClick={() => void onSave()} disabled={busy}>
                            Save defaults
                        </Button>
                        <Button variant="outline-secondary" size="sm" onClick={() => void onTest()} disabled={busy}>
                            Run self-test
                        </Button>
                    </div>
                </div>
            </div>

            <div className="card mb-3">
                <div className="card-body">
                    <h5 className="h6 mb-2">Tracking preview</h5>
                    <p className="execlaw-muted small mb-3">
                        Paste results.tsv rows to visualize metric trajectory. Lower is better for val_bpb-style metrics.
                    </p>
                    <textarea
                        className="form-control mb-3"
                        rows={8}
                        value={tsvText}
                        onChange={(e: { target: { value: string } }) => setTsvText(e.target.value)}
                        data-testid="autoresearch-tsv-preview"
                    />
                    <div className="border rounded p-2" style={{ background: "var(--bs-body-bg)" }}>
                        <svg viewBox="0 0 620 180" width="100%" height="180" role="img" aria-label="AutoResearch metric trend">
                            <rect x="0" y="0" width="620" height="180" fill="transparent" />
                            <line x1="30" y1="150" x2="600" y2="150" stroke="currentColor" strokeOpacity="0.25" />
                            <line x1="30" y1="20" x2="30" y2="150" stroke="currentColor" strokeOpacity="0.25" />
                            {chart.polyline && (
                                <polyline
                                    fill="none"
                                    stroke="#198754"
                                    strokeWidth="2.5"
                                    points={chart.polyline}
                                />
                            )}
                            {chart.dots.map((d, idx) => (
                                <circle key={idx} cx={d.x} cy={d.y} r="3" fill={d.color} />
                            ))}
                            <text x="35" y="16" fontSize="11" fill="currentColor" opacity="0.7">
                                best {chart.bestLabel}
                            </text>
                            <text x="520" y="16" fontSize="11" fill="currentColor" opacity="0.7">
                                worst {chart.worstLabel}
                            </text>
                        </svg>
                    </div>
                    <div className="execlaw-muted small mt-2" data-testid="autoresearch-tracking-stats">
                        Parsed runs: {points.length} | keeps: {points.filter((p) => p.status === "keep").length}
                    </div>
                </div>
            </div>

            {testResult.kind === "ok" && <div className="alert alert-success">{testResult.message}</div>}
            {testResult.kind === "err" && <div className="alert alert-danger">{testResult.message}</div>}
        </div>
    );
};

function parseTsvPoints(tsv: string): TsvPoint[] {
    const lines = tsv
        .split("\n")
        .map((x) => x.trim())
        .filter((x) => x.length > 0);
    if (lines.length < 2) return [];

    const out: TsvPoint[] = [];
    for (let i = 1; i < lines.length; i += 1) {
        const cols = lines[i].split("\t");
        if (cols.length < 4) continue;
        const v = Number(cols[1]);
        if (!Number.isFinite(v)) continue;
        out.push({ idx: out.length, val: v, status: String(cols[3]).toLowerCase() });
    }
    return out;
}

function buildChart(points: TsvPoint[]) {
    if (points.length === 0) {
        return {
            polyline: "",
            dots: [] as Array<{ x: number; y: number; color: string }>,
            bestLabel: "-",
            worstLabel: "-",
        };
    }

    const min = Math.min(...points.map((p) => p.val));
    const max = Math.max(...points.map((p) => p.val));
    const span = Math.max(1e-9, max - min);

    const left = 30;
    const top = 20;
    const w = 570;
    const h = 130;

    const step = points.length > 1 ? w / (points.length - 1) : 0;
    const dots = points.map((p, i) => {
        const x = left + i * step;
        const y = top + ((p.val - min) / span) * h;
        const color = p.status === "keep" ? "#0d6efd" : p.status === "discard" ? "#dc3545" : "#fd7e14";
        return { x, y, color };
    });

    const polyline = dots.map((d) => `${d.x},${d.y}`).join(" ");

    return {
        polyline,
        dots,
        bestLabel: min.toFixed(6),
        worstLabel: max.toFixed(6),
    };
}

export default Panel;
