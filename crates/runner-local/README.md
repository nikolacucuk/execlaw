# execlaw-runner-local

The one runner. Speaks OpenAI-compatible API to whichever local backend the
operator has configured. **No cloud SDKs** — see §0 axiom #1.

Phase 1 deliverable: full agent loop (session hydration, tool dispatch,
compaction, interrupt/resume). Phase 0 is the stub.

## Implementation plan

All 130 enhancements are committed implementation scope under the
[implementation plan](../../docs/implementation-plan.md). This commitment is not a
claim that they have shipped: the plan owns implementation status, and the
[roadmap](../../docs/llm-harness-roadmap.md) owns acceptance criteria.

Relevant work for this component:

- [H022: verified task completion](../../docs/llm-harness-roadmap.md#enhancement-022).
- [H023: executor recovery parity](../../docs/llm-harness-roadmap.md#enhancement-023).
- [H031: per-round context budgets](../../docs/llm-harness-roadmap.md#enhancement-031).
- [H032: compaction provenance](../../docs/llm-harness-roadmap.md#enhancement-032).
- [H042: durable child runs](../../docs/llm-harness-roadmap.md#enhancement-042).
- [H043: steering and cancellation](../../docs/llm-harness-roadmap.md#enhancement-043).
- [H068: independent tool parallelism](../../docs/llm-harness-roadmap.md#enhancement-068).
- [H113: non-progress detection](../../docs/llm-harness-roadmap.md#enhancement-113).
