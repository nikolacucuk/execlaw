# AutoResearch Loop Playbook

Use this skill when the operator wants autonomous, repeated ML experiments with strict keep/discard decisions.

## Intent

Adapt the spirit of karpathy/autoresearch to execlaw workflows:
- small editable surface per experiment
- fixed time budget per run
- single objective metric as source of truth
- keep if better, discard if not better
- log every run with clear rationale

## Suggested Loop

1. Define objective and constraints:
- Primary metric (for example: val_bpb, val_loss, latency)
- Max wall-clock per run
- Soft memory budget
- Simplicity preference (prefer simpler code for equal results)

2. Establish baseline run:
- Run the unmodified baseline once
- Log baseline metric and memory
- Freeze this as reference for candidate scoring

3. For each candidate experiment:
- Make one focused change
- Run with same time budget
- Record metric, memory, and short description
- Evaluate with autoresearch.score_candidate
- Keep only if justified by metric + complexity + memory tradeoff

4. Keep logs machine-readable:
- Prefer tab-separated rows with fields:
  commit, metric, memory_gb, status, description
- Use autoresearch.analyze_results_tsv periodically to detect drift

5. Prefer idea classes that produce interpretable deltas:
- architecture simplification
- optimizer hyperparameter sweeps
- batch/sequence tradeoffs
- schedule and regularization changes

## Guardrails

- Do not change evaluation code while optimizing the metric.
- Do not mix multiple unrelated ideas in one run.
- If a run crashes repeatedly, mark crash and move on.
- When gains are marginal and complexity rises, prefer discard.

## Execlaw integration hints

- Use deep-research tools for paper and baseline gathering.
- Use this plugin for experiment loop discipline and result triage.
- Promote repeatable successful patterns into reusable skills.
