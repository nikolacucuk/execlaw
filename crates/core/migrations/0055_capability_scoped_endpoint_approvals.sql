ALTER TABLE config_local_endpoint_approvals RENAME TO config_local_endpoint_approvals_v1;

CREATE TABLE config_local_endpoint_approvals (
    scope TEXT NOT NULL CHECK (scope IN ('local_inference', 'private_integration')),
    kind TEXT NOT NULL CHECK (kind IN ('cidr', 'dns_name')),
    value TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (scope, kind, value)
);

-- Existing approvals were shared by all endpoint callers. Preserve them as
-- private-integration grants; local inference must be approved in its own scope.
INSERT INTO config_local_endpoint_approvals(scope, kind, value, created_at)
SELECT 'private_integration', kind, value, created_at
FROM config_local_endpoint_approvals_v1;

DROP TABLE config_local_endpoint_approvals_v1;