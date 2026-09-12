CREATE TABLE config_local_endpoint_approvals (
    kind       TEXT NOT NULL CHECK (kind IN ('cidr', 'dns_name')),
    value      TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (kind, value)
);

CREATE TABLE state_local_endpoint_resolutions (
    endpoint_key      TEXT PRIMARY KEY,
    url               TEXT NOT NULL,
    classification    TEXT NOT NULL,
    resolved_addresses TEXT NOT NULL DEFAULT '[]',
    last_error        TEXT,
    resolved_at       INTEGER NOT NULL
);