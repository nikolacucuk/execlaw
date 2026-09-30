# execlaw-identity-api

Trait for identity-provider plugins (§2.14). Reference plugins —
`identity-local-address-book`, `identity-signal-safety-numbers`,
`identity-google-contacts` — will live out-of-tree and be installed via ZIP.

## Implementation plan

All 130 enhancements are committed implementation scope under the
[implementation plan](../../docs/implementation-plan.md). This commitment is not a
claim that they have shipped: the plan owns implementation status, and the
[roadmap](../../docs/llm-harness-roadmap.md) owns acceptance criteria.

Relevant work for this component:

- [H052: live authority checks](../../docs/llm-harness-roadmap.md#enhancement-052).
- [H089: reviewed entity resolution](../../docs/llm-harness-roadmap.md#enhancement-089).
- [H100: recipient identity](../../docs/llm-harness-roadmap.md#enhancement-100).
- [H102: cross-channel continuity](../../docs/llm-harness-roadmap.md#enhancement-102).
