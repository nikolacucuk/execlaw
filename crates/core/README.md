# execlaw-core

The durability heart of execlaw.

Owns:

- `state_events` append-only event log (§2.3 of MIGRATION_PLAN.md)
- Conversation FSM (`state_conversations`, phase transitions)
- Turn-as-transaction commit (§2.4) — enforces the `tool_use`/`tool_result` pairing
  invariant (§2.2 axiom #3)
- Work-queue leases
- SQLite connection pool, WAL mode, SQLCipher pragmas, migration runner
- Per-connection `foreign_keys = ON` enforcement
- Governed memory-asset metadata, agent loadouts, local Wiki/CodeGraph derived
  indexes, FTS5 retrieval, and optional local embedding/RRF retrieval. The
  eligibility-aware search computes trust/scope/lifecycle/time candidates before
  ranking and accepts explicit injection-mode allowlists.

HOT and retrieved asset decisions are recorded as metadata-only receipts in
`state_turn_asset_loadouts`, keyed by conversation and user-event sequence.
Receipts include policy scope, asset/version/source hashes, binding mode,
retrieval rank, and injected character count; they do not duplicate asset
content. The Controller run inspector reads these receipts from the run trace.
Current model-context search is lexical; versioned local embeddings, reranking,
and rebuild qualification remain H038 work.

Intentionally has **zero** knowledge of:

- any transport (signal, email, voice) — those are plugins, see `transport-api`
- any inference backend — see `inference-api` + `runner-local`
- any cloud vendor SDK — never, per §0 axiom #1.

## Implementation plan

All 130 enhancements are committed implementation scope under the
[implementation plan](../../docs/implementation-plan.md). This commitment is not a
claim that they have shipped: the plan owns implementation status, and the
[roadmap](../../docs/llm-harness-roadmap.md) owns acceptance criteria.

Relevant work for this component:

- [H023: durable execution](../../docs/llm-harness-roadmap.md#enhancement-023).
- [H037: memory evidence](../../docs/llm-harness-roadmap.md#enhancement-037).
- [H038: trust-first retrieval](../../docs/llm-harness-roadmap.md#enhancement-038).
- [H048: projection deletion](../../docs/llm-harness-roadmap.md#enhancement-048).
- [H058: power-loss durability](../../docs/llm-harness-roadmap.md#enhancement-058).
- [H059: bounded database execution](../../docs/llm-harness-roadmap.md#enhancement-059).
- [H061: event versions](../../docs/llm-harness-roadmap.md#enhancement-061).
- [H062: projection rebuilds](../../docs/llm-harness-roadmap.md#enhancement-062).
- [H064: schema evolution](../../docs/llm-harness-roadmap.md#enhancement-064).
- [H077: artifact references](../../docs/llm-harness-roadmap.md#enhancement-077).
