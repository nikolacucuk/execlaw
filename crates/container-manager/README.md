# execlaw-container-manager

Single Rust crate owning every Docker interaction and the tiered hardware
profile detection (§5.3).

Phase 0 scope: Tier 1 sysfs reads (pure Rust, no vendor tooling in the
control plane) + data shapes for Tiers 2–4. Bollard integration + probe
containers land in Phase 2.

## Implementation plan

All 154 enhancements are committed implementation scope under the
[implementation plan](../../docs/implementation-plan.md). This commitment is not a
claim that they have shipped: the plan owns implementation status, and the
[roadmap](../../docs/llm-harness-roadmap.md) owns acceptance criteria.

Relevant work for this component:

- [H030: resource scheduling](../../docs/llm-harness-roadmap.md#enhancement-030).
- [H072: runtime isolation profiles](../../docs/llm-harness-roadmap.md#enhancement-072).
- [H073: cross-platform isolation](../../docs/llm-harness-roadmap.md#enhancement-073).
- [H074: process and storage budgets](../../docs/llm-harness-roadmap.md#enhancement-074).
- [H075: safe draining](../../docs/llm-harness-roadmap.md#enhancement-075).
- [H078: hardware qualification](../../docs/llm-harness-roadmap.md#enhancement-078).
