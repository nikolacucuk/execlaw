// AutoResearch plugin (script tier)
// Inspired by https://github.com/karpathy/autoresearch

fn on_enable() {
    log_info("autoresearch enabled");
}

fn on_disable() {
    log_info("autoresearch disabled");
}

fn tool_call(name, args, oauth) {
    if name == "autoresearch.bootstrap_run" { return bootstrap_run(args); }
    if name == "autoresearch.analyze_results_tsv" { return analyze_results_tsv(args); }
    if name == "autoresearch.score_candidate" { return score_candidate(args); }
    throw "unknown tool '" + name + "'";
}

fn admin_get_config(req) {
    let metric_name = get_setting("metric_name", "val_bpb");
    let memory_budget_gb = parse_float_optional(vault_get("memory_budget_gb"), 48.0);
    let min_gain_threshold = parse_float_optional(vault_get("min_gain_threshold"), 0.001);
    #{
        "metric_name": metric_name,
        "memory_budget_gb": memory_budget_gb,
        "min_gain_threshold": min_gain_threshold,
    }
}

fn admin_set_config(req) {
    if req.body == () { throw "missing request body"; }

    let metric_name = if req.body.metric_name != () { trim(`${req.body.metric_name}`) } else { "" };
    if metric_name == "" { metric_name = "val_bpb"; }

    let memory_budget_gb = parse_float_optional(req.body.memory_budget_gb, 48.0);
    let min_gain_threshold = parse_float_optional(req.body.min_gain_threshold, 0.001);

    if memory_budget_gb <= 0.0 { throw "memory_budget_gb must be greater than 0"; }
    if min_gain_threshold < 0.0 { throw "min_gain_threshold must be >= 0"; }

    vault_put("metric_name", metric_name);
    vault_put("memory_budget_gb", `${memory_budget_gb}`);
    vault_put("min_gain_threshold", `${min_gain_threshold}`);

    log_info("autoresearch config updated");
    admin_get_config(#{} )
}

fn admin_test(req) {
    let cfg = admin_get_config(#{});
    let baseline = 1.0000;
    let candidate = baseline - cfg.min_gain_threshold - 0.0002;

    let bootstrap_probe = bootstrap_run(#{
        "objective": "minimize val_bpb",
        "platform": "windows_rtx",
        "time_budget_minutes": 5,
    });

    let tsv_probe = analyze_results_tsv(#{
        "tsv": "commit\tval_bpb\tmemory_gb\tstatus\tdescription\na1\t1.0100\t40.0\tkeep\tbaseline\nb2\t1.0030\t41.0\tkeep\tlr tune\nc3\t1.0041\t41.2\tdiscard\toverfit\nd4\t0.9992\t42.0\tkeep\twindow ablation",
        "top_n": 3,
    });

    let score_probe = score_candidate(#{
        "baseline_val_bpb": baseline,
        "candidate_val_bpb": candidate,
        "baseline_memory_gb": 40.0,
        "candidate_memory_gb": 41.0,
        "complexity": "same",
        "min_gain_for_higher_complexity": cfg.min_gain_threshold,
    });
    #{
        "ok": true,
        "config": cfg,
        "tool_probes": #{
            "bootstrap_run": bootstrap_probe,
            "analyze_results_tsv": tsv_probe,
            "score_candidate": score_probe,
        },
        "probe_decision": score_probe,
        "note": "Self-test uses synthetic values only; no external calls are made.",
    }
}

fn bootstrap_run(args) {
    let objective = normalize_text(args.objective, "objective");
    let platform = if args.platform != () && args.platform != "" { lower(`${args.platform}`) } else { "unknown" };
    let run_tag = if args.run_tag != () && args.run_tag != "" { `${args.run_tag}` } else { auto_tag() };
    let time_budget = parse_int_or_default(args.time_budget_minutes, 5);
    let metric_name = get_setting("metric_name", "val_bpb");
    let memory_budget_gb = parse_float_optional(vault_get("memory_budget_gb"), 48.0);
    let min_gain_threshold = parse_float_optional(vault_get("min_gain_threshold"), 0.001);

    let platform_tips = platform_recommendations(platform);
    let experiment_queue = base_experiment_queue(platform);

    #{
        "source": "karpathy/autoresearch-inspired",
        "run_tag": run_tag,
        "objective": objective,
        "platform": platform,
        "time_budget_minutes": time_budget,
        "defaults": #{
            "metric_name": metric_name,
            "memory_budget_gb": memory_budget_gb,
            "min_gain_threshold": min_gain_threshold,
        },
        "setup_checklist": [
            "Create a dedicated branch for this run tag.",
            "Lock objective and evaluation metric before experimenting.",
            "Record a baseline run with unchanged code.",
            "Use a tab-separated log file with columns: commit, metric, memory_gb, status, description.",
            "Keep one focused code change per experiment."
        ],
        "loop_rules": [
            "Keep if metric improves and complexity/memory cost is acceptable.",
            "Discard if metric is flat or worse.",
            "Crash rows are allowed; fix easy issues once, then move on.",
            "Do not change evaluation harness while comparing runs."
        ],
        "platform_tips": platform_tips,
        "experiment_queue": experiment_queue,
        "notes": [
            "This plugin provides decision support and analytics, not GPU execution.",
            "Use execlaw deep-research for literature gathering and this plugin for loop discipline."
        ]
    }
}

fn analyze_results_tsv(args) {
    let raw = normalize_text(args.tsv, "tsv");
    let top_n = parse_int_or_default(args.top_n, 5);

    let lines = raw.split("\n");
    if lines.len() < 2 {
        throw "results TSV must include a header and at least one data row";
    }

    let total = 0;
    let keep = 0;
    let discard = 0;
    let crash = 0;

    let baseline = ();
    let best = ();
    let kept_rows = [];

    let i = 1;
    while i < lines.len() {
        let line = trim(lines[i]);
        i += 1;
        if line == "" { continue; }

        let row = parse_tsv_row(line);
        if row == () { continue; }

        total += 1;
        let st = lower(row.status);
        if st == "keep" {
            keep += 1;
            kept_rows.push(row);
        } else if st == "discard" {
            discard += 1;
        } else if st == "crash" {
            crash += 1;
        }

        if baseline == () {
            baseline = row;
        }
        if best == () || row.val_bpb < best.val_bpb {
            best = row;
        }
    }

    if total == 0 {
        throw "no valid rows found in results TSV";
    }

    let keep_rate = if keep + discard > 0 {
        (keep * 100.0) / ((keep + discard) * 1.0)
    } else {
        0.0
    };

    let cumulative_gain = if baseline != () && best != () {
        baseline.val_bpb - best.val_bpb
    } else {
        0.0
    };

    let hits = top_kept_deltas(kept_rows, top_n);

    #{
        "total_experiments": total,
        "counts": #{"keep": keep, "discard": discard, "crash": crash},
        "keep_rate_pct": round6(keep_rate),
        "baseline": baseline,
        "best": best,
        "cumulative_gain": round6(cumulative_gain),
        "top_kept_deltas": hits,
        "trajectory_hint": trajectory_hint(kept_rows)
    }
}

fn score_candidate(args) {
    let crashed = if args.crashed != () { args.crashed == true } else { false };
    if crashed {
        return #{
            "decision": "crash",
            "should_keep": false,
            "reason": "Run crashed. Log crash and move to the next candidate unless fix is trivial."
        };
    }

    let baseline_val = parse_float_required(args.baseline_val_bpb, "baseline_val_bpb");
    let candidate_val = parse_float_required(args.candidate_val_bpb, "candidate_val_bpb");
    let baseline_mem = parse_float_optional(args.baseline_memory_gb, 0.0);
    let candidate_mem = parse_float_optional(args.candidate_memory_gb, baseline_mem);
    let complexity = if args.complexity != () && args.complexity != "" { lower(`${args.complexity}`) } else { "same" };

    let default_min_gain = parse_float_optional(vault_get("min_gain_threshold"), 0.001);
    let default_mem_budget = parse_float_optional(vault_get("memory_budget_gb"), 48.0);
    let min_gain_high_complexity = parse_float_optional(args.min_gain_for_higher_complexity, default_min_gain);
    let max_mem_growth_pct = if args.max_memory_growth_pct != () {
        parse_float_optional(args.max_memory_growth_pct, 25.0)
    } else {
        infer_max_growth_pct_from_budget(baseline_mem, default_mem_budget)
    };

    let delta = baseline_val - candidate_val;
    let improved = delta > 0.0;

    let mem_growth_pct = if baseline_mem > 0.0 {
        ((candidate_mem - baseline_mem) / baseline_mem) * 100.0
    } else {
        0.0
    };

    let mem_ok = mem_growth_pct <= max_mem_growth_pct;
    let complexity_ok = if complexity == "higher" {
        delta >= min_gain_high_complexity
    } else {
        true
    };

    let decision = "discard";
    let reason = "No metric improvement.";

    if improved && mem_ok && complexity_ok {
        decision = "keep";
        reason = "Metric improved with acceptable memory and complexity tradeoff.";
    } else if improved && (!mem_ok || !complexity_ok) {
        decision = "review";
        reason = "Metric improved, but memory or complexity cost is high; keep only if strategically important.";
    }

    #{
        "decision": decision,
        "should_keep": decision == "keep",
        "delta_val_bpb": round6(delta),
        "improved": improved,
        "memory_growth_pct": round6(mem_growth_pct),
        "memory_within_budget": mem_ok,
        "complexity": complexity,
        "complexity_threshold_pass": complexity_ok,
        "applied_min_gain_threshold": min_gain_high_complexity,
        "applied_max_memory_growth_pct": max_mem_growth_pct,
        "reason": reason
    }
}

// ---------- helpers ----------

fn parse_tsv_row(line) {
    let cols = line.split("\t");
    if cols.len() < 5 { return (); }

    let commit = trim(cols[0]);
    let val_bpb = host_parse_float(trim(cols[1]));
    let mem = host_parse_float(trim(cols[2]));
    let status = trim(cols[3]);

    if val_bpb == () { return (); }
    if mem == () { return (); }

    // Re-join in case description included tabs.
    let desc = cols[4];
    let j = 5;
    while j < cols.len() {
        desc += "\t" + cols[j];
        j += 1;
    }

    #{
        "commit": commit,
        "val_bpb": val_bpb,
        "memory_gb": mem,
        "status": status,
        "description": trim(desc)
    }
}

fn top_kept_deltas(kept_rows, top_n) {
    let out = [];
    if kept_rows.len() < 2 { return out; }

    let i = 1;
    while i < kept_rows.len() {
        let prev = kept_rows[i - 1];
        let cur = kept_rows[i];
        out.push(#{
            "commit": cur.commit,
            "description": cur.description,
            "val_bpb": cur.val_bpb,
            "delta": round6(prev.val_bpb - cur.val_bpb)
        });
        i += 1;
    }

    // Select top N by delta using a simple repeated max scan.
    let selected = [];
    let n = if top_n < out.len() { top_n } else { out.len() };
    let k = 0;
    while k < n {
        let best_idx = -1;
        let best_delta = -1e18;
        let m = 0;
        while m < out.len() {
            let d = out[m].delta;
            if d > best_delta {
                best_delta = d;
                best_idx = m;
            }
            m += 1;
        }
        if best_idx < 0 { break; }
        selected.push(out[best_idx]);
        out.remove(best_idx);
        k += 1;
    }
    selected
}

fn trajectory_hint(kept_rows) {
    if kept_rows.len() < 3 {
        return "Insufficient kept-history for trend detection.";
    }
    let a = kept_rows[kept_rows.len() - 3].val_bpb;
    let b = kept_rows[kept_rows.len() - 2].val_bpb;
    let c = kept_rows[kept_rows.len() - 1].val_bpb;

    if c < b && b < a {
        return "Recent kept runs are consistently improving.";
    }
    if c > b && b > a {
        return "Recent kept runs are regressing; consider reverting to simpler ideas.";
    }
    "Recent kept runs are mixed; run tighter ablations before larger changes."
}

fn platform_recommendations(platform) {
    if platform == "h100" {
        return [
            "Keep 5-minute fixed budgets for rapid iteration.",
            "Use architecture and optimizer experiments before heavy data changes.",
            "Track MFU and VRAM to avoid hidden throughput regressions."
        ];
    }
    if platform == "consumer_nvidia" || platform == "windows_rtx" {
        return [
            "Prefer smaller depth and smaller total batch size first.",
            "Use simpler attention/window patterns before exotic kernels.",
            "Trim eval tokens if validation dominates runtime."
        ];
    }
    if platform == "macos_mlx" || platform == "amd" {
        return [
            "Expect different kernel bottlenecks; favor simpler architectures.",
            "Reduce sequence length and model depth aggressively for stable runs.",
            "Prioritize reproducible baseline and crash-free loop discipline."
        ];
    }
    [
        "Start from conservative model size and fixed runtime budget.",
        "Tune one lever per run so deltas are attributable.",
        "Capture both metric and memory for every run."
    ]
}

fn base_experiment_queue(platform) {
    if platform == "consumer_nvidia" || platform == "windows_rtx" || platform == "macos_mlx" || platform == "amd" {
        return [
            "Baseline unchanged run",
            "Reduce sequence length and re-balance batch size",
            "Learning-rate scaling sweep",
            "Batch size and accumulation sweep",
            "Model depth and width simplification",
            "Attention/window pattern ablation",
            "Regularization and warmdown schedule tuning"
        ];
    }
    [
        "Baseline unchanged run",
        "Learning-rate scaling sweep",
        "Batch size and accumulation sweep",
        "Model depth and width simplification",
        "Attention/window pattern ablation",
        "Regularization and warmdown schedule tuning"
    ]
}

fn normalize_text(value, name) {
    if value == () { throw "missing required arg '" + name + "'"; }
    let s = trim(`${value}`);
    if s == "" { throw "arg '" + name + "' must not be empty"; }
    s
}

fn get_setting(key, fallback) {
    let v = vault_get(key);
    if v == () { return fallback; }
    let s = trim(`${v}`);
    if s == "" { return fallback; }
    s
}

fn infer_max_growth_pct_from_budget(baseline_mem, budget_mem) {
    if baseline_mem <= 0.0 || budget_mem <= 0.0 { return 25.0; }
    let pct = ((budget_mem - baseline_mem) / baseline_mem) * 100.0;
    if pct < 0.0 { return 0.0; }
    pct
}

fn parse_float_required(value, name) {
    if value == () { throw "missing required arg '" + name + "'"; }
    let n = host_parse_float(trim(`${value}`));
    if n == () { throw "arg '" + name + "' must be numeric"; }
    n
}

fn parse_float_optional(value, fallback) {
    if value == () { return fallback; }
    let n = host_parse_float(trim(`${value}`));
    if n == () { return fallback; }
    n
}

fn parse_int_or_default(value, fallback) {
    if value == () { return fallback; }
    let n = host_parse_int(trim(`${value}`));
    if n == () { return fallback; }
    n
}

fn round6(x) {
    // Keep helper as a stable extension point in case tighter rounding
    // is needed later.
    x
}

fn auto_tag() {
    let secs = now();
    "run-" + `${secs}`
}
