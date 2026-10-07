# execlaw-plugin-host

Plugin registry + lifecycle. Phase 0 ships an in-memory registry; Phase 2
builds out the full hook-registry with isolation tiers and tool dispatch
per §4.2–§4.4.

## Implementation plan

All 154 enhancements are committed implementation scope under the
[implementation plan](../../docs/implementation-plan.md). This commitment is not a
claim that they have shipped: the plan owns implementation status, and the
[roadmap](../../docs/llm-harness-roadmap.md) owns acceptance criteria.

Relevant work for this component:

- [H027: transactional upgrades](../../docs/llm-harness-roadmap.md#enhancement-027).
- [H079: plugin conformance](../../docs/llm-harness-roadmap.md#enhancement-079).
- [H080: API negotiation](../../docs/llm-harness-roadmap.md#enhancement-080).
- [H081: in-flight version pinning](../../docs/llm-harness-roadmap.md#enhancement-081).
- [H082: publisher revocation](../../docs/llm-harness-roadmap.md#enhancement-082).
- [H083: hook execution contracts](../../docs/llm-harness-roadmap.md#enhancement-083).
- [H084: restricted WebAssembly trial](../../docs/llm-harness-roadmap.md#enhancement-084).
