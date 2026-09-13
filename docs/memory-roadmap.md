# Memory Capability Roadmap

Status: implementation started 2026-09-12.

This roadmap records the TencentDB Agent Memory-inspired work without replacing
Execlaw's event log, evidence model, trust ladder, or local-only inference rule.

## Phase 1: Finish Governed Lifecycle

- [x] HOT memory slot is bounded and trust-filtered.
- [x] Tier, hit, and last-used fields are persisted.
- [x] Promotion proposals are approval-gated and idempotent.
- [x] Reflection rows are append-only and event anchored.
- [ ] Run promotion and demotion sweepers from the server lifecycle.
- [ ] Add the post-turn planner reflection trigger and heuristic gate.
- [ ] Add the SPA approval queue for pending memory promotions.

## Phase 2: Unified Memory-Asset Registry

- [x] Add metadata-only `memory_assets` registry.
- [x] Register asset types for memory, skills, Wiki, code graph, and research.
- [x] Track owner scope, visibility, trust floor, status, version, provenance,
  expiry, and usage.
- [ ] Register existing skills and Graphiti resources during installation/startup.
- [ ] Add controller-only HTTP CRUD and audit events for asset metadata.

## Phase 3: Task and Agent Loadouts

- [x] Add `memory_asset_bindings` with hot, discoverable, and tool-only modes.
- [x] Add priority and per-asset character budgets.
- [x] Add bounded loadout reads.
- [ ] Resolve loadouts into the per-turn tool catalog and HOT prompt block.
- [ ] Enforce visibility and trust-floor checks before asset ranking or injection.
- [ ] Add SPA controls for assigning assets to agents and tasks.

## Phase 4: Bounded Hybrid Retrieval

- [x] Add SQLite FTS5 asset search with sanitized queries.
- [x] Add optional local embedding persistence keyed by model and source hash.
- [x] Add lexical/vector reciprocal-rank fusion with bounded results.
- [ ] Add FTS indexes for approved memory assertions and conversation fragments.
- [ ] Add a capability-gated `memory.search` tool with evidence references.
- [ ] Add retrieval metrics and evaluation cases for recall, leakage, and token cost.

## Phase 5: Local Wiki and CodeGraph Assets

- [x] Add local Wiki and CodeGraph metadata tables.
- [x] Add Wiki page and code node/edge derived-index tables.
- [x] Add indexes for symbol, caller, and callee lookup.
- [ ] Add a local documentation ingester with revision and source hashes.
- [ ] Add a local Rust/TypeScript/Rhai symbol and reference indexer.
- [ ] Expose read-only `wiki.search`, `wiki.read_page`, `code.search`,
  `code.callers`, `code.callees`, and `code.impact` tools.
- [ ] Mark indexes stale when their source revision changes.

## Architectural Constraints

- SQLite and the append-only event log remain authoritative.
- Extracted memory remains pending until evidence and policy approve it.
- Asset visibility never replaces Execlaw trust-class or capability checks.
- Embeddings are derived data; they never authorize access and must be invalidated
  when the source hash or embedding model changes.
- Wiki and CodeGraph queries are read-only model tools. Ingestion, sync, delete,
  and sharing remain controller-authorized operations.
- No cloud LLM or cloud memory service is introduced.

## Implementation Record

Migration `0025_memory_assets_knowledge.sql` and
`crates/core/src/memory_assets.rs` provide the first vertical slice: governed
asset metadata, loadouts, SQLite FTS5 search, optional local embeddings with
RRF, and local Wiki/CodeGraph derived-index storage. The remaining unchecked
items are runtime wiring and UI work, not permission to bypass the constraints
above.
