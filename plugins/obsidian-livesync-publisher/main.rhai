fn tool_call(name, args, oauth) {
    if name == "obsidian.publish" {
        return publish_impl();
    }
    throw "unknown tool '" + name + "'";
}

fn admin_get_config(req) {
    let cfg = read_config();
    cfg["password"] = "<redacted>";
    cfg
}

fn admin_set_config(req) {
    let body = if req.body != () { req.body } else { #{} };
    let cfg = read_config();

    if body["couchdb_url"] != () { cfg["couchdb_url"] = require_string(body["couchdb_url"], "couchdb_url"); }
    if body["database"] != () { cfg["database"] = require_string(body["database"], "database"); }
    if body["username"] != () { cfg["username"] = require_string(body["username"], "username"); }
    if body["password"] != () && type_of(body["password"]) == "string" && body["password"] != "<redacted>" {
        cfg["password"] = body["password"];
    }
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

fn publish_impl() {
    let cfg = read_config();
    validate_config(cfg);
    let base = sidecar_url("publisher");
    if base == () { throw "obsidian-livesync-publisher sidecar is not healthy"; }
    let payload = #{};
    payload["couchdb_url"] = cfg["couchdb_url"];
    payload["database"] = cfg["database"];
    payload["username"] = cfg["username"];
    payload["password"] = cfg["password"];
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

fn validate_config(cfg) {
    if type_of(cfg["couchdb_url"]) != "string" || !starts_with(cfg["couchdb_url"], "http") {
        throw "obsidian-livesync-publisher: couchdb_url must be an http(s) URL";
    }
    if type_of(cfg["database"]) != "string" || cfg["database"] == "" { throw "database is required"; }
    if type_of(cfg["username"]) != "string" || cfg["username"] == "" { throw "username is required"; }
    if type_of(cfg["password"]) != "string" || cfg["password"] == "" { throw "password is required"; }
}

fn require_string(value, name) {
    if value == () || type_of(value) != "string" || value == "" { throw name + " is required"; }
    value
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
