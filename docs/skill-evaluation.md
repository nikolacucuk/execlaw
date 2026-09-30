# Held-out skill evaluation

## Current gate and committed extensions

The [implementation plan](implementation-plan.md) tracks H001-H130. The gate
below implements the current [H015](llm-harness-roadmap.md#enhancement-015)
text-evaluation baseline: a pass means required terms appeared, not that a
tool-using task succeeded. [Finding F16](llm-harness-roadmap.md#review-findings-and-unresolved-verification)
records that limit; no comparative capability or production-release claim
should be inferred from these scores alone.

[H039 executable skill evaluation and rollback](llm-harness-roadmap.md#enhancement-039)
extends the gate with isolated task execution, deterministic verifiers,
side-effect policy checks, and reversible promotion. It depends on
[H021 real-task benchmarks](llm-harness-roadmap.md#enhancement-021),
[H096 independently attributable evidence](llm-harness-roadmap.md#enhancement-096),
and [H111 protected holdouts and calibrated judges](llm-harness-roadmap.md#enhancement-111).
Expected answers and verifier policy must stay outside the evaluated workspace
and skill-capture path; repeating rubric terms must not manufacture task
success. The implementation tracker owns status and execution evidence for
these planned gates. This update changes neither promotion behavior nor test
results.

Trial skills can only be promoted after a Controller-configured held-out suite
passes against the exact current immutable version. The suite is stored apart
from the skill body in `state_skill_eval_cases`; evaluation records bind the
version body hash and a hash of the current suite in `state_skill_eval_runs`.
Changing either the skill or the suite invalidates the previous pass. A run
also records the latest comparable parent-version score and score delta when
the suite, evaluator version, model, endpoint, and wire protocol match.

The Controller API is:

* `PUT /api/admin/skills/{name}/eval-suite` with 1–20 cases. Each case has a
  unique `case_id`, task `prompt`, and 1–12 case-insensitively unique
  `required_terms`. Prompts, IDs, and rubric terms have size limits.
* `POST /api/admin/skills/{name}/evaluate` runs each case through the configured
  Standard local inference backend with the skill as system guidance and no
  tools. Prompts, expected terms, and generated outputs are not returned or
  persisted in run results; only per-case pass counts and aggregate score are
  retained. A case passes only when the output contains every required term.
  Runs record the immutable skill body hash, suite hash, evaluator version, model
  ID, and a hash of the endpoint/model/wire-protocol tuple; endpoint values and
  generated output are not stored. Increment the evaluator version when its
  prompt, rubric scoring, or inference settings change.
* `POST /api/admin/skills/{name}/promote` requires at least 80% of cases to
  pass for the current version and current suite. Legacy runs without backend
  identity cannot authorize promotion. The skill scanner remains mandatory on
  create and update.

Evaluation cases are Controller-authored; keep expected terms discriminative,
avoid including evaluation answers in skill text, and revise the suite when a
skill's intended behavior changes. A before score is reported only when the
parent skill version was evaluated against the same suite, evaluator version,
model ID, and endpoint/model/protocol fingerprint. The evaluator calls only the
configured local or explicitly approved endpoint. No cloud judge is used.

The implementation is in `crates/server/src/skills_admin.rs`,
`crates/skills/src/store.rs`, and migrations `0036_skill_eval_runs.sql` and
`0037_skill_eval_comparison_identity.sql`.
