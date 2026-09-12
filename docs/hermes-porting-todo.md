# Hermes Porting Status (execlaw)

Historical status of requested carry-over work. The implementation items below
have shipped; this file remains as a provenance checklist rather than an active
roadmap.

## Completed in this change set

- Added HOT memory snapshot injection in prompt assembly.
- Enabled learning-loop defaults with migration for existing DBs.
- Added programmatic tool-chaining plugin scaffold.
- Added operator-facing rubric and security approval semantics docs.

## Completed implementation phase

- `chain.plan` produces deterministic persisted plans.
- `chain.execute` and `chain.resume` record per-step audit state and enforce budgets.
- Plans containing external effects halt for approval before execution continues.
- Migration `0012_chain_plans_runs.sql` and the core chain store persist plans, runs, and steps.
- Host integration tests cover approval halt/resume and replay-safe execution.

The host implements the live `chain.*` tools in
`crates/server/src/tool_chain_tool.rs`. The `plugins/tool-chain/main.rhai`
handlers are an obsolete scaffold and are not the implementation authority.

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
