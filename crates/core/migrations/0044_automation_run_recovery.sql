ALTER TABLE state_automation_runs ADD COLUMN definition_json TEXT;
UPDATE state_automation_runs
SET definition_json = (
    SELECT definition FROM state_automations
    WHERE state_automations.id = state_automation_runs.automation_id
)
WHERE definition_json IS NULL;

CREATE TABLE state_automation_run_claims (
    automation_id TEXT NOT NULL,
    event_id TEXT NOT NULL,
    run_id TEXT NOT NULL UNIQUE,
    PRIMARY KEY (automation_id, event_id)
);

INSERT OR IGNORE INTO state_automation_run_claims(automation_id, event_id, run_id)
SELECT automation_id, event_id, id
FROM state_automation_runs
ORDER BY started_at, id;
