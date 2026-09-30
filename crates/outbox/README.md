# execlaw-outbox

The outbox crate drains leased `state_outbox` rows through registered
`Dispatcher` implementations. Dispatchers declare whether the sink accepts the
framework idempotency key or supports status lookup, and return typed
`NotAccepted` versus `OutcomeUnknown` failures. Definite failures follow the
retry budget; ambiguous failures retry only for an idempotent sink, reconcile
when status lookup is available, and otherwise park in `unknown` without an
automatic retry.

Controllers can inspect metadata-only unknown-effect rows with
`GET /api/admin/outbox/unknown` and resolve one with
`POST /api/admin/outbox/{outbox_id}/resolution`. The request chooses
`authorize_retry` or `confirm_delivered` and supplies an evidence reference;
there is no automatic retry while an outcome remains unknown. Actor and
evidence are stored in the append-only delivery timeline. The crate includes
the `schedule.wakeup` dispatcher and tested mock sinks; production transport
sink integration remains tracked under H024.

## Implementation plan

All 130 enhancements are committed implementation scope under the
[implementation plan](../../docs/implementation-plan.md). This commitment is not a
claim that they have shipped: the plan owns implementation status, and the
[roadmap](../../docs/llm-harness-roadmap.md) owns acceptance criteria.

Relevant work for this component:

- [H024: effect reconciliation](../../docs/llm-harness-roadmap.md#enhancement-024).
- [H055: outbound data checks](../../docs/llm-harness-roadmap.md#enhancement-055).
- [H067: compensating actions](../../docs/llm-harness-roadmap.md#enhancement-067).
- [H069: durable timers](../../docs/llm-harness-roadmap.md#enhancement-069).
- [H071: dead-letter redrive](../../docs/llm-harness-roadmap.md#enhancement-071).
