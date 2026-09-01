# Hermes Porting TODO (execlaw)

Status of requested carry-over work and next implementation steps.

## Completed in this change set

- Added HOT memory snapshot injection in prompt assembly.
- Enabled learning-loop defaults with migration for existing DBs.
- Added programmatic tool-chaining plugin scaffold.
- Added operator-facing rubric and security approval semantics docs.

## TODOs (implementation phase)

- Wire chain.plan to deterministic planner output with persisted plan IDs.
- Add chain.execute runtime with per-step audit events and budget enforcement.
- Require approval token for plans containing external effects.
- Add host-side storage for chain plans/runs (new migration + store).
- Add integration tests for approval halt/resume and replay-safe execution.

## File checklist

- crates/server/src/chats/prompt.rs
- crates/server/src/chats.rs (tests)
- crates/core/src/skills_config.rs
- crates/core/src/migrations.rs
- crates/core/migrations/0011_enable_skills_learning_loop_defaults.sql
- plugins/tool-chain/plugin.toml
- plugins/tool-chain/main.rhai
- plugins/tool-chain/schemas/chain.plan.json
- plugins/tool-chain/schemas/chain.execute.json
- plugins/tool-chain/README.md
- docs/operator-decision-rubric.md
- docs/security.md
- docs/plugins.md
- README.md

## Test checklist

- cargo test -p execlaw-server assemble_system_prompt_injects_hot_memory_between_routing_and_context
- cargo test -p execlaw-core skills_config::tests::default_values_match_locked_design
- cargo test --workspace (full regression)
