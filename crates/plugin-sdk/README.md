# execlaw-plugin-sdk

Manifest parsing (`plugin.toml`) and ZIP-staging for execlaw's
**hook-declaration** plugin model (§4.2, 2026-04-23 locked).

A plugin is a plugin. No typed "kinds" — a manifest declares which hook
points it attaches to (tools, transport, identity_provider, inference_backend,
hardware_probe, oauth_accounts, ui_panels, chat_components, event_subscriptions,
alert_sources, health_checks, skills, services). One plugin can attach to many.

## Implementation plan

All 154 enhancements are committed implementation scope under the
[implementation plan](../../docs/implementation-plan.md). This commitment is not a
claim that they have shipped: the plan owns implementation status, and the
[roadmap](../../docs/llm-harness-roadmap.md) owns acceptance criteria.

Relevant work for this component:

- [H065: tool effect declarations](../../docs/llm-harness-roadmap.md#enhancement-065).
- [H079: author conformance](../../docs/llm-harness-roadmap.md#enhancement-079).
- [H080: API compatibility](../../docs/llm-harness-roadmap.md#enhancement-080).
- [H083: hook contracts](../../docs/llm-harness-roadmap.md#enhancement-083).
- [H084: restricted WebAssembly trial](../../docs/llm-harness-roadmap.md#enhancement-084).
