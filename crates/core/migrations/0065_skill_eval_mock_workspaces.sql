ALTER TABLE state_skill_eval_cases
    ADD COLUMN workspace_files_json TEXT NOT NULL DEFAULT '{}'
        CHECK (json_valid(workspace_files_json));
ALTER TABLE state_skill_eval_cases
    ADD COLUMN expected_workspace_files_json TEXT NOT NULL DEFAULT '{}'
        CHECK (json_valid(expected_workspace_files_json));
ALTER TABLE state_skill_eval_cases
    ADD COLUMN mock_integrations_json TEXT NOT NULL DEFAULT '{}'
        CHECK (json_valid(mock_integrations_json));
ALTER TABLE state_skill_eval_cases
    ADD COLUMN expected_integration_calls_json TEXT NOT NULL DEFAULT '[]'
        CHECK (json_valid(expected_integration_calls_json));
ALTER TABLE state_skill_eval_cases
    ADD COLUMN forbidden_actions_json TEXT NOT NULL DEFAULT '[]'
        CHECK (json_valid(forbidden_actions_json));
