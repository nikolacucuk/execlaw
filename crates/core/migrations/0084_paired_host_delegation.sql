CREATE TABLE paired_operator_hosts (
    peer_id TEXT PRIMARY KEY,
    endpoint TEXT NOT NULL,
    inference_endpoint TEXT NOT NULL,
    public_key_hex TEXT NOT NULL CHECK(length(public_key_hex) = 64),
    key_fingerprint_sha256 TEXT NOT NULL CHECK(length(key_fingerprint_sha256) = 64),
    allowed_capabilities_json TEXT NOT NULL,
    allowed_data_labels_json TEXT NOT NULL,
    paired_by TEXT NOT NULL,
    paired_at INTEGER NOT NULL,
    active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0, 1))
);

CREATE TABLE delegated_host_tasks (
    task_id TEXT PRIMARY KEY,
    peer_id TEXT NOT NULL REFERENCES paired_operator_hosts(peer_id),
    task_json TEXT NOT NULL,
    authority_json TEXT NOT NULL,
    data_labels_json TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','accepted','running','completed','failed','cancelled')),
    artifacts_json TEXT NOT NULL DEFAULT '[]',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(peer_id, task_id)
);

CREATE INDEX delegated_host_tasks_peer_status
    ON delegated_host_tasks(peer_id, status, updated_at);
