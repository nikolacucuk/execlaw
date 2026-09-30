-- Held-out skill evaluation records are bound to an immutable version hash.
CREATE TABLE state_skill_eval_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    skill_name TEXT NOT NULL,
    version_id INTEGER NOT NULL REFERENCES state_skill_versions(id),
    body_sha256 TEXT NOT NULL,
    evaluator_version TEXT NOT NULL,
    passed INTEGER NOT NULL CHECK (passed IN (0, 1)),
    score REAL NOT NULL CHECK (score >= 0.0 AND score <= 1.0),
    before_score REAL CHECK (before_score >= 0.0 AND before_score <= 1.0),
    results_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_skill_eval_runs_version
    ON state_skill_eval_runs(version_id, created_at DESC);
CREATE TABLE state_skill_eval_cases (
    skill_name TEXT NOT NULL,
    case_id TEXT NOT NULL,
    prompt TEXT NOT NULL,
    required_terms_json TEXT NOT NULL,
    PRIMARY KEY (skill_name, case_id)
);

