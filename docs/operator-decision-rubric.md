# Operator Decision Rubric

This rubric standardizes how to decide between a plugin, MCP server, or host-core change.

## Implementation commitment

All H001-H130 are accepted work in the [implementation plan](implementation-plan.md).
Apply this rubric to placement and implementation design; it does not authorize
dropping a requirement from that plan. A poor placement score means redesign
the placement while retaining the intended outcome and acceptance gate.
Use [H040 workspace plugins](llm-harness-roadmap.md#enhancement-040),
[H065 effect contracts](llm-harness-roadmap.md#enhancement-065),
[H079 conformance](llm-harness-roadmap.md#enhancement-079),
[H080 API compatibility](llm-harness-roadmap.md#enhancement-080), and
[H083 hook semantics](llm-harness-roadmap.md#enhancement-083) when adding
surfaces. H084/H085 remain planned qualification trials. Record the owning
component, dependencies, and evidence in the ledger; retain host policy,
SQLite state, local inference, and outbox guarantees across every adapter.

## 1) Placement rubric: plugin vs MCP vs host core

Use a plugin when any answer below is yes:

- Needs trust_floor or capability gating by caller trust class.
- Needs OAuth token injection from host-managed accounts.
- Needs inbound hooks (transport, webhook, identity provider, alerts).
- Needs long-running supervised sidecars.
- Needs first-party lifecycle guarantees (install/enable/disable/audit).

Use an MCP server when all answers below are yes:

- Stateless request/response tool calls are enough.
- No trust-class semantics are required inside the integration.
- Host-side trust/capability enforcement still applies; MCP is not an exemption.
- No host-managed OAuth flow is required.
- The integration is third-party and operator-optional.

Use host core when any answer below is yes:

- The behavior is a platform invariant (event pairing, outbox idempotency, trust ladder).
- The feature must exist even with zero plugins installed.
- The change affects security boundaries or turn commit semantics.

## 2) Structured decision rubric (scorecard)

Score each option 0-2 for each criterion:

- Security boundary fit
- Operational complexity
- Upgrade safety
- Testability in CI
- Reversibility for operators

Interpretation:

- 8-10: preferred placement
- 5-7: acceptable but requires explicit rationale
- 0-4: reject and redesign

## 3) Programmatic tool chaining concept

Recommended placement: plugin-first scaffold, then selective host integration only where invariants require it.

- Plugin scaffold path: plugins/tool-chain
- Initial tools: chain.plan and chain.execute
- Host integration points later:
  - approval token checks for effectful chains
  - step-level outbox idempotency keys
  - replay-safe chain run records

## 4) Memory snapshot discipline

Operational contract:

- Keep a bounded HOT memory snapshot in the system prompt.
- Treat snapshot as read-only context for the current turn.
- Keep deterministic ordering and strict byte budget.

Current implementation uses global HOT entries filtered by readable trust classes and injects them between routing prose and per-turn context.

## 5) Learning loop defaults

Closed-loop behaviour should be on by default:

- auto_capture_enabled = true
- reuse_update_enabled = true

Use proposals for operator review before stable promotion.
