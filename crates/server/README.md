# execlaw-server

Axum HTTP + WebSocket server. Ships:

- `/api/health` — liveness.
- `/api/setup`, `/api/login`, `/api/token/refresh`, `/api/logout` — JWT
  (Ed25519) + admin-password auth per §7.1, §8.3.
- `/api/openapi.json`, `/api/asyncapi.json`, `/api/docs` — Swagger UI +
  AsyncAPI viewer bundle per §8.4.

WS `/api/stream` — the live event stream — is scheduled for Phase 1; the
event vocabulary is already documented in `spec/asyncapi.yaml`.

## Implementation plan

All 154 enhancements are committed implementation scope under the
[implementation plan](../../docs/implementation-plan.md). This commitment is not a
claim that they have shipped: the plan owns implementation status, and the
[roadmap](../../docs/llm-harness-roadmap.md) owns acceptance criteria.

Relevant work for this component:

- [H023: execution recovery](../../docs/llm-harness-roadmap.md#enhancement-023).
- [H024: idempotency and unknown-effect resolution](../../docs/llm-harness-roadmap.md#enhancement-024).
- [H026: route authorization](../../docs/llm-harness-roadmap.md#enhancement-026).
- [H030: hierarchical inference admission](../../docs/llm-harness-roadmap.md#enhancement-030).
- [H035: execution inspection](../../docs/llm-harness-roadmap.md#enhancement-035).
- [H037: per-turn memory provenance](../../docs/llm-harness-roadmap.md#enhancement-037).
- [H038: trust-first retrieval](../../docs/llm-harness-roadmap.md#enhancement-038).
- [H045: MCP interoperability](../../docs/llm-harness-roadmap.md#enhancement-045).
- [H070: routine scheduling](../../docs/llm-harness-roadmap.md#enhancement-070).
- [H124: workflow revisions](../../docs/llm-harness-roadmap.md#enhancement-124).
- [H125: durable workflow nodes](../../docs/llm-harness-roadmap.md#enhancement-125).
- [H126: effect-free scenarios](../../docs/llm-harness-roadmap.md#enhancement-126).
