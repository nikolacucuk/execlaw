ALTER TABLE state_skill_eval_cases
    ADD COLUMN forbidden_terms_json TEXT NOT NULL DEFAULT '[]'
        CHECK (json_valid(forbidden_terms_json));
ALTER TABLE state_skill_eval_cases
    ADD COLUMN min_output_chars INTEGER NOT NULL DEFAULT 0
        CHECK (min_output_chars >= 0);
ALTER TABLE state_skill_eval_cases
    ADD COLUMN max_output_chars INTEGER NOT NULL DEFAULT 4000
        CHECK (max_output_chars BETWEEN 1 AND 20000);
ALTER TABLE state_skill_eval_cases
    ADD COLUMN max_output_tokens INTEGER NOT NULL DEFAULT 256
        CHECK (max_output_tokens BETWEEN 1 AND 1024);
