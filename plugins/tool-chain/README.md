# tool-chain plugin scaffold

This plugin is a manifest-first skeleton for the programmatic tool chaining concept.

## Tool surface

- chain.plan
- chain.execute
- chain.resume

## Trust and capability defaults

- chain.plan: trust_floor = KnownLimited, latency = low.
- chain.execute: trust_floor = Controller, latency = high, required_capabilities = ["tools.sensitive"].
- chain.resume: trust_floor = Controller, latency = medium, required_capabilities = ["tools.sensitive"].

All three are host-implemented and therefore respect both:

- Settings -> Plugins ON/OFF for `tool-chain`.
- Settings -> Tools enabled/disabled policy per tool name.

## Implementation checklist

- Implement deterministic plan generation in main.rhai with stable step IDs.
- Persist plan artifacts in a host-side table keyed by conversation_id + turn_seq.
- Require explicit approval token for chain.execute when any step has external effect.
- Emit tool_use/tool_result pairs for each executed step in a single turn commit.
- Enforce max step count and wall-clock budget before dispatching each step.
- Add replay-safe idempotency key derivation for effectful steps via outbox semantics.

## Test cases

- Plan schema validation: rejects empty objective and max_steps > 12.
- Trust floor gating: KnownLimited can call chain.plan, cannot call chain.execute.
- Capability gate: chain.execute denied without tools.sensitive capability.
- Approval gate: effectful plans produce approval request and halt until approved.
- Tool pairing invariant: each executed step commits tool_use + tool_result together.
- Failure propagation: one failed step records deterministic stop reason and partial outputs.
