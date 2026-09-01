# Graphify Integration Instructions

Purpose: make the workspace's Graphify outputs the first-class source for architecture/tracing queries, and avoid expensive rebuilds unless explicitly requested.

When to load this file
- Load when the user asks architecture, trace, relationship, or "where is" questions.

Primary rules for assistants
- Use persisted graph files first (token-saving): prefer `graphify-out/graph.json` then `.graphify/graph.json`.
- If a persisted graph exists, do NOT run a full graph rebuild (detection+AST+semantic extraction) automatically.
- When answering, use `/graphify query "<question>"`, `graphify path "<A>" "<B>"`, or `graphify explain "<concept>"` against the existing graphfile and return concise subgraph slices.
- Cite nodes/paths using workspace-relative file links back to the source files found in node metadata when possible.

When to rebuild the graph
- Rebuild only when explicitly requested by the user (phrases: "rebuild the graph", "refresh graphify", "re-run detection"), or when the graph file is missing and the user asks for graph-backed answers.
- When rebuilding, prefer the workspace task `Build Graphify` (see `.vscode/tasks.json`) or run:

```powershell
.venv\Scripts\python.exe tools/agent/build_graphify.py --config graphify.toml
```

Performance and token guidance
- For large corpora, prefer returning a focused subgraph (path between nodes, small community subgraph, or top-N god nodes) instead of re-extracting whole-graph semantics.
- If you must run a heavy build, warn the user first and offer to run an incremental/targeted query instead.

Useful files
- `graphify-out/graph.json` — first place to look for persisted graph
- `.graphify/graph.json` — alternate persisted location
- `graphify-out/GRAPH_REPORT.md` — human-readable analysis and god nodes
- `graphify.toml` — graphify config; `tools/agent/build_graphify.py` — build script

User-visible commands (examples)
- Query: `/graphify query "What connects main() to the tile code?"`
- Path: `graphify path "main()" "tile_pkg"`
- Rebuild (explicit): `python tools/agent/build_graphify.py --config graphify.toml`

Notes for maintainers
- The graph files are typically gitignored; ensure the team keeps `graphify-out/graph.json` in the workspace or a CI artifact if consistent availability is required.
- Adding the provided VS Code `Build Graphify` task simplifies rebuilds for new sessions.
