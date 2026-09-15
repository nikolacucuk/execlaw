fn tool_call(name, args, oauth) {
    if name == "obsidian.publish" {
        return publish_impl();
    }
    throw "unknown tool '" + name + "'";
}

fn admin_get_config(req) {
    let cfg = normalize_config(read_config());
    cfg["password"] = "<redacted>";
    cfg
}

fn admin_set_config(req) {
    let body = if req.body != () { req.body } else { #{} };
    let cfg = normalize_config(read_config());

    if body["couchdb_url"] != () { cfg["couchdb_url"] = require_string(body["couchdb_url"], "couchdb_url"); }
    if body["database"] != () { cfg["database"] = require_string(body["database"], "database"); }
    if body["username"] != () { cfg["username"] = require_string(body["username"], "username"); }
    if body["password"] != () && type_of(body["password"]) == "string" && body["password"] != "<redacted>" {
        cfg["password"] = body["password"];
    }
    if body["source_subdir"] != () { cfg["source_subdir"] = require_relative_path(body["source_subdir"], "source_subdir"); }
    if body["max_files"] != () { cfg["max_files"] = clamp_int(body["max_files"], 1, 10000, cfg["max_files"]); }
    if body["max_bytes"] != () { cfg["max_bytes"] = clamp_int(body["max_bytes"], 1, 52428800, cfg["max_bytes"]); }

    validate_config(cfg);
    vault_put("config", to_json_string(cfg));
    cfg["password"] = "<redacted>";
    #{ "ok": true, "config": cfg }
}

fn admin_publish(req) {
    publish_impl()
}

fn admin_test(req) {
    let base = sidecar_url("publisher");
    if base == () { throw "obsidian-livesync-publisher sidecar is not healthy"; }
    let cfg = normalize_config(read_config());
    sidecar_http_post(base + "/v1/check", #{ "source_subdir": cfg["source_subdir"] }, #{})
}

fn publish_impl() {
    let cfg = normalize_config(read_config());
    validate_config(cfg);
    let base = sidecar_url("publisher");
    if base == () { throw "obsidian-livesync-publisher sidecar is not healthy"; }
    let payload = #{};
    payload["couchdb_url"] = cfg["couchdb_url"];
    payload["database"] = cfg["database"];
    payload["username"] = cfg["username"];
    payload["password"] = cfg["password"];
    payload["source_subdir"] = cfg["source_subdir"];
    payload["max_files"] = cfg["max_files"];
    payload["max_bytes"] = cfg["max_bytes"];
    sidecar_http_post(base + "/v1/publish", payload, #{})
}

fn read_config() {
    let fallback = #{
        "couchdb_url": "http://couchdb-obsidian-livesync:5984",
        "database": "djenka_db",
        "username": "",
        "password": "",
        "source_subdir": "obsidian-vault/execlaw",
        "max_files": 1000,
        "max_bytes": 52428800,
    };
    let raw = ();
    try { raw = vault_get("config"); } catch (e) { return fallback; }
    if raw == () || raw == "" { return fallback; }
    let parsed = ();
    try { parsed = parse_json(raw); } catch (e) { return fallback; }
    if parsed == () || type_of(parsed) != "map" { return fallback; }
    for key in fallback.keys() {
        if parsed[key] == () { parsed[key] = fallback[key]; }
    }
    parsed
}

fn normalize_config(cfg) {
    if type_of(cfg["username"]) != "string" || cfg["username"] == "" {
        cfg["username"] = "ncucuk";
    }
    if cfg["source_subdir"] == "execlaw" { cfg["source_subdir"] = "obsidian-vault/execlaw"; }
    if type_of(cfg["source_subdir"]) != "string" || cfg["source_subdir"] == "" || starts_with(cfg["source_subdir"], "/") || cfg["source_subdir"].index_of("..") >= 0 || cfg["source_subdir"].index_of("\\") >= 0 {
        cfg["source_subdir"] = "obsidian-vault/execlaw";
    }
    cfg
}

fn validate_config(cfg) {
    if type_of(cfg["couchdb_url"]) != "string" || !starts_with(cfg["couchdb_url"], "http") {
        throw "obsidian-livesync-publisher: couchdb_url must be an http(s) URL";
    }
    if type_of(cfg["database"]) != "string" || cfg["database"] == "" { throw "database is required"; }
    if type_of(cfg["password"]) != "string" || cfg["password"] == "" {
        throw "CouchDB password is not configured. Open Obsidian LiveSync Publisher settings and save the password before publishing.";
    }
}

fn require_string(value, name) {
    if value == () || type_of(value) != "string" || value == "" { throw name + " is required"; }
    value
}

fn require_relative_path(value, name) {
    let path = require_string(value, name);
    if starts_with(path, "/") || path.index_of("..") >= 0 || path.index_of("\\") >= 0 {
        throw name + " must be a relative folder below /ai_pool, for example obsidian-vault/execlaw";
    }
    path
}

fn starts_with(value, prefix) {
    value.index_of(prefix) == 0
}

fn clamp_int(value, minimum, maximum, fallback) {
    if value == () || type_of(value) != "i64" { return fallback; }
    if value < minimum { return minimum; }
    if value > maximum { return maximum; }
    value
}
