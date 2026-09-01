// web-scraper — policy-aware sidecar proxy for advanced page scraping.
//
// This script keeps host/plugin concerns clear:
// - trust gating is enforced by manifest trust_floor + host dispatch
// - this layer validates required fields and applies admin policy
// - sidecar does fetch/render/extract work and returns JSON

fn tool_call(name, args, oauth) {
    if name == "scraper.fetch_page" { return fetch_page_impl(args); }
    if name == "scraper.extract" { return extract_impl(args); }
    if name == "scraper.follow_links" { return follow_links_impl(args); }
    if name == "scraper.session_close" { return session_close_impl(args); }
    throw "unknown tool '" + name + "'";
}

// =====================================================================
// Tool handlers
// =====================================================================

fn fetch_page_impl(args) {
    let cfg = read_config();
    let payload = #{};
    payload["url"] = require_string(args["url"], "url");
    payload["mode"] = pick_mode(args["mode"], "static");
    payload["timeout_ms"] = clamp_int(args["timeout_ms"], 1000, cfg.limits.fetch_timeout_max_ms, cfg.limits.fetch_timeout_default_ms);
    payload["max_chars"] = clamp_int(args["max_chars"], 512, cfg.limits.fetch_max_chars_max, cfg.limits.fetch_max_chars_default);
    payload["session_id"] = args["session_id"];
    payload["wait_for"] = args["wait_for"];
    payload["allowed_domains"] = effective_allowed_domains(cfg, args["allowed_domains"]);

    let base = require_sidecar_url();
    sidecar_http_post(base + "/v1/fetch", payload, #{});
}

fn extract_impl(args) {
    let cfg = read_config();
    let payload = #{};
    payload["url"] = require_string(args["url"], "url");
    payload["mode"] = pick_mode(args["mode"], "dynamic");
    payload["timeout_ms"] = clamp_int(args["timeout_ms"], 1000, cfg.limits.extract_timeout_max_ms, cfg.limits.extract_timeout_default_ms);
    payload["max_chars"] = clamp_int(args["max_chars"], 512, cfg.limits.extract_max_chars_max, cfg.limits.extract_max_chars_default);
    payload["session_id"] = args["session_id"];
    payload["fields"] = if args["fields"] != () { args["fields"] } else { [] };
    payload["include_links"] = if args["include_links"] != () { args["include_links"] } else { false };
    payload["main_text"] = if args["main_text"] != () { args["main_text"] } else { true };
    payload["allowed_domains"] = effective_allowed_domains(cfg, args["allowed_domains"]);

    let base = require_sidecar_url();
    sidecar_http_post(base + "/v1/extract", payload, #{});
}

fn follow_links_impl(args) {
    let cfg = read_config();
    let payload = #{};
    payload["seed_url"] = require_string(args["seed_url"], "seed_url");
    payload["mode"] = pick_mode(args["mode"], "static");
    payload["max_pages"] = clamp_int(args["max_pages"], 1, cfg.limits.crawl_max_pages_max, cfg.limits.crawl_max_pages_default);
    payload["max_depth"] = clamp_int(args["max_depth"], 0, cfg.limits.crawl_max_depth_max, cfg.limits.crawl_max_depth_default);
    payload["timeout_ms"] = clamp_int(args["timeout_ms"], 1000, cfg.limits.crawl_timeout_max_ms, cfg.limits.crawl_timeout_default_ms);
    payload["include_patterns"] = args["include_patterns"];
    payload["exclude_patterns"] = args["exclude_patterns"];
    payload["extract"] = args["extract"];
    payload["allowed_domains"] = effective_allowed_domains(cfg, args["allowed_domains"]);

    let base = require_sidecar_url();
    sidecar_http_post(base + "/v1/crawl", payload, #{});
}

fn session_close_impl(args) {
    let payload = #{};
    payload["session_id"] = require_string(args["session_id"], "session_id");
    let base = require_sidecar_url();
    sidecar_http_post(base + "/v1/session/close", payload, #{});
}

// =====================================================================
// Admin routes
// =====================================================================

fn admin_get_config(req) {
    let cfg = read_config();
    cfg
}

fn admin_set_config(req) {
    let body = if req.body != () { req.body } else { #{} };
    let cfg = read_config();

    cfg["domain_policy"] = if body["domain_policy"] != () { body["domain_policy"] } else { cfg["domain_policy"] };
    cfg["default_allowed_domains"] = if body["default_allowed_domains"] != () { normalize_domains(body["default_allowed_domains"]) } else { cfg["default_allowed_domains"] };

    cfg.limits.fetch_timeout_default_ms = clamp_int(body["fetch_timeout_default_ms"], 1000, cfg.limits.fetch_timeout_max_ms, cfg.limits.fetch_timeout_default_ms);
    cfg.limits.fetch_timeout_max_ms = clamp_int(body["fetch_timeout_max_ms"], 1000, 300000, cfg.limits.fetch_timeout_max_ms);
    cfg.limits.fetch_max_chars_default = clamp_int(body["fetch_max_chars_default"], 512, cfg.limits.fetch_max_chars_max, cfg.limits.fetch_max_chars_default);
    cfg.limits.fetch_max_chars_max = clamp_int(body["fetch_max_chars_max"], 512, 100000, cfg.limits.fetch_max_chars_max);

    cfg.limits.extract_timeout_default_ms = clamp_int(body["extract_timeout_default_ms"], 1000, cfg.limits.extract_timeout_max_ms, cfg.limits.extract_timeout_default_ms);
    cfg.limits.extract_timeout_max_ms = clamp_int(body["extract_timeout_max_ms"], 1000, 300000, cfg.limits.extract_timeout_max_ms);
    cfg.limits.extract_max_chars_default = clamp_int(body["extract_max_chars_default"], 512, cfg.limits.extract_max_chars_max, cfg.limits.extract_max_chars_default);
    cfg.limits.extract_max_chars_max = clamp_int(body["extract_max_chars_max"], 512, 200000, cfg.limits.extract_max_chars_max);

    cfg.limits.crawl_timeout_default_ms = clamp_int(body["crawl_timeout_default_ms"], 1000, cfg.limits.crawl_timeout_max_ms, cfg.limits.crawl_timeout_default_ms);
    cfg.limits.crawl_timeout_max_ms = clamp_int(body["crawl_timeout_max_ms"], 1000, 600000, cfg.limits.crawl_timeout_max_ms);
    cfg.limits.crawl_max_pages_default = clamp_int(body["crawl_max_pages_default"], 1, cfg.limits.crawl_max_pages_max, cfg.limits.crawl_max_pages_default);
    cfg.limits.crawl_max_pages_max = clamp_int(body["crawl_max_pages_max"], 1, 100, cfg.limits.crawl_max_pages_max);
    cfg.limits.crawl_max_depth_default = clamp_int(body["crawl_max_depth_default"], 0, cfg.limits.crawl_max_depth_max, cfg.limits.crawl_max_depth_default);
    cfg.limits.crawl_max_depth_max = clamp_int(body["crawl_max_depth_max"], 0, 6, cfg.limits.crawl_max_depth_max);

    if cfg["domain_policy"] != "allow_all" && cfg["domain_policy"] != "allow_list" {
        throw "web-scraper: domain_policy must be one of allow_all or allow_list";
    }
    if cfg["domain_policy"] == "allow_list" && cfg["default_allowed_domains"].len() == 0 {
        throw "web-scraper: allow_list policy requires at least one allowed domain";
    }

    vault_put("config", to_json_string(cfg));
    #{ "ok": true, "config": cfg }
}

fn admin_test(req) {
    let cfg = read_config();
    let base = sidecar_url("scraper");
    if base == () {
        return #{
            "ok": false,
            "sidecar_healthy": false,
            "error": "web-scraper sidecar is not healthy",
            "config": cfg,
        };
    }

    let health = sidecar_http_get(base + "/healthz", #{});
    let sample = #{};
    sample["url"] = "https://example.com";
    sample["mode"] = "static";
    sample["timeout_ms"] = clamp_int((), 1000, cfg.limits.fetch_timeout_max_ms, cfg.limits.fetch_timeout_default_ms);
    sample["max_chars"] = 1200;
    sample["allowed_domains"] = effective_allowed_domains(cfg, ["example.com"]);

    let fetch_probe = sidecar_http_post(base + "/v1/fetch", sample, #{});
    #{
        "ok": true,
        "sidecar_healthy": true,
        "health": health,
        "probe": #{
            "final_url": fetch_probe["final_url"],
            "status": fetch_probe["status"],
            "truncated": fetch_probe["truncated"],
        },
        "config": cfg,
    }
}

// =====================================================================
// Config + policy helpers
// =====================================================================

fn read_config() {
    let raw = ();
    try {
        raw = vault_get("config");
    } catch (e) {
        // Capability missing in tests; fallback config below.
    }

    let fallback = #{
        "domain_policy": "allow_all",
        "default_allowed_domains": [],
        "limits": #{
            "fetch_timeout_default_ms": 15000,
            "fetch_timeout_max_ms": 120000,
            "fetch_max_chars_default": 6000,
            "fetch_max_chars_max": 50000,
            "extract_timeout_default_ms": 30000,
            "extract_timeout_max_ms": 180000,
            "extract_max_chars_default": 12000,
            "extract_max_chars_max": 100000,
            "crawl_timeout_default_ms": 60000,
            "crawl_timeout_max_ms": 300000,
            "crawl_max_pages_default": 5,
            "crawl_max_pages_max": 25,
            "crawl_max_depth_default": 1,
            "crawl_max_depth_max": 3,
        },
    };

    if raw == () || raw == "" { return fallback; }

    let parsed = ();
    try {
        parsed = parse_json(raw);
    } catch (e) {
        log_warn("web-scraper: malformed config JSON in vault; using defaults");
    }
    if parsed == () || type_of(parsed) != "map" { return fallback; }

    for k in fallback.keys() {
        if parsed[k] == () { parsed[k] = fallback[k]; }
    }
    for k in fallback["limits"].keys() {
        if parsed["limits"][k] == () { parsed["limits"][k] = fallback["limits"][k]; }
    }
    parsed["default_allowed_domains"] = normalize_domains(parsed["default_allowed_domains"]);
    parsed
}

fn effective_allowed_domains(cfg, arg_domains) {
    let defaults = normalize_domains(cfg["default_allowed_domains"]);
    let call_domains = normalize_domains(arg_domains);

    if cfg["domain_policy"] == "allow_list" {
        if defaults.len() == 0 {
            throw "web-scraper: allow_list policy has no configured domains";
        }
        if call_domains.len() == 0 { return defaults; }
        let out = [];
        for d in call_domains {
            if list_contains(defaults, d) { out.push(d); }
        }
        if out.len() == 0 {
            throw "web-scraper: call domain list is outside configured allow list";
        }
        out
    } else {
        if call_domains.len() > 0 { return call_domains; }
        defaults
    }
}

fn normalize_domains(v) {
    if v == () || type_of(v) != "array" { return []; }
    let out = [];
    for item in v {
        if type_of(item) != "string" { continue; }
        let s = trim(lower(item));
        if s == "" { continue; }
        if !list_contains(out, s) { out.push(s); }
    }
    out
}

fn list_contains(arr, v) {
    for item in arr {
        if item == v { return true; }
    }
    false
}

fn pick_mode(v, fallback) {
    if v == () || type_of(v) != "string" { return fallback; }
    let s = lower(trim(v));
    if s == "static" || s == "dynamic" || s == "stealthy" { return s; }
    fallback
}

fn require_sidecar_url() {
    let base = sidecar_url("scraper");
    if base == () { throw "web-scraper sidecar is not healthy"; }
    base
}

fn require_string(v, field_name) {
    if v == () || type_of(v) != "string" || trim(v) == "" {
        throw "`" + field_name + "` is required and must be a non-empty string";
    }
    trim(v)
}

fn clamp_int(value, lo, hi, fallback) {
    if value == () { return fallback; }
    let n = value;
    let t = type_of(n);
    if t == "string" {
        let parsed = host_parse_int(n);
        if parsed == () { return fallback; }
        n = parsed;
    }
    if type_of(n) != "i64" && type_of(n) != "int" { return fallback; }
    if n < lo { return lo; }
    if n > hi { return hi; }
    n
}
