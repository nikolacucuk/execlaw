-- Keep score comparisons tied to the same held-out suite and local backend.
ALTER TABLE state_skill_eval_runs ADD COLUMN suite_sha256 TEXT NOT NULL DEFAULT '';
ALTER TABLE state_skill_eval_runs ADD COLUMN model_id TEXT;
ALTER TABLE state_skill_eval_runs ADD COLUMN backend_fingerprint TEXT;

-- Earlier records overloaded evaluator_version with the suite hash. Preserve
-- that binding, while leaving model identity NULL so old runs cannot be used
-- as an apples-to-oranges before score.
UPDATE state_skill_eval_runs SET suite_sha256 = evaluator_version;

CREATE INDEX idx_skill_eval_runs_comparison
    ON state_skill_eval_runs(skill_name, version_id, suite_sha256, model_id, backend_fingerprint, created_at DESC);
