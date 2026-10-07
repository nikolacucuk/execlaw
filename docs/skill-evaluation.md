# Held-out skill evaluation

## H015 baseline and H039 behavioral gate

The [implementation plan](implementation-plan.md) tracks H001-H154. H015's
text-only baseline reports required-term matches; by itself, that score does
not establish that a tool-using task succeeded. H039 extends evaluation with
isolated task execution, deterministic mock integrations, forbidden-action
checks, resource limits, parent/candidate comparison, and governed promotion.
H039 still depends on [H021 real-task benchmarks](llm-harness-roadmap.md#enhancement-021),
[H096 independently attributable evidence](llm-harness-roadmap.md#enhancement-096),
and [H111 protected holdouts and calibrated judges](llm-harness-roadmap.md#enhancement-111).

The Controller API is:

* `PUT /api/admin/skills/{name}/eval-suite` accepts 1–20 unique cases. Every
  case must define a behavioral workspace change or an expected mock
  integration call. Cases can include initial and expected workspace files,
  deterministic mock responses, exact call order, required and forbidden
  output terms, forbidden actions, output character limits, and output-token
  limits. Expected files and verifier rules remain outside the skill body and
  evaluated workspace.
* `POST /api/admin/skills/{name}/evaluate` runs each case against the configured
  local Standard inference backend in a temporary workspace. The closed tool
  catalog provides bounded file reads/writes and deterministic mock calls;
  it cannot perform live network effects. Traversal and secret paths are
  denied. Each case is bounded to eight inference rounds, 32 tool calls,
  60 seconds per inference request, 16 files, 64 KiB per file, 512 KiB total
  workspace, and a ten-minute suite run. The output-token budget is cumulative
  across rounds; missing backend token-usage records fail the case closed. Any
  denied/forbidden action, failed behavioral assertion, or resource-budget
  violation fails the case.
* Candidate and immediate parent versions run against the same cases and
  backend. Results record per-case behavioral match counts, action counts,
  output size, token use, suite hash, version body hash, evaluator version,
  model identity, and a backend fingerprint; prompts and generated output are
  not persisted in evaluation results. Promotion rechecks the current suite
  hash and requires every candidate case to pass. Rollback creates a new
  monotonic trial version, so it must pass the current suite before promotion.

Expected answers and verifier policy must stay outside the evaluated workspace
and skill-capture path. Merely repeating rubric terms cannot satisfy the
workspace or integration assertions. Integration effects are mock-only during
evaluation. Full live-model held-out qualification and promotion evidence are
still required before H039 can be marked qualified.

The implementation is in `crates/server/src/skills_admin.rs`,
`crates/skills/src/store.rs`, and migrations `0036_skill_eval_runs.sql`,
`0037_skill_eval_comparison_identity.sql`, `0063_skill_eval_behavioral_assertions.sql`,
and `0065_skill_eval_mock_workspaces.sql`.
