# Portable Graphiti + Obsidian + Graphify Instructions

Purpose: reusable instruction set for other workspaces where both temporal memory concepts and local token-saving workflows are useful.

## Scope

Use this in repositories where you want:

- graph-structured context lookup (Graphify)
- persistent local memory (.obsidian)
- optional stronger temporal memory concepts inspired by Graphiti
- self-evolving lesson extraction from coding sessions

## Core Positioning

Graphiti and Obsidian+Graphify solve different layers.

- Graphiti: temporal context graph engine with episode ingestion, validity windows, provenance, and hybrid retrieval.
- This portable local workflow: lightweight, repo-native memory using Graphify artifacts plus Obsidian markdown notes and scripts.

Use local workflow first for low-friction coding sessions. Introduce Graphiti where temporal fact memory and entity-edge retrieval are required.

## Graphiti Findings to Carry Forward

1. Keep an explicit ingestion unit (episode-like input) for memory writes.
2. Preserve provenance from derived lesson to source note/chat.
3. Use hybrid retrieval concepts: structure first, then note recall, then raw source reads.
4. Distinguish current truth from historical notes when you maintain long-running systems.
5. Plan for infra cost and operational complexity before adopting full graph database memory.

## High-Value Additions Ported Here

These were implemented in this repository and are portable:

1. Self-evolving lesson extraction stage in chat import pipeline.
2. Categorized lesson outputs:
   - .obsidian/Patterns/
   - .obsidian/Mistakes/
   - .obsidian/Decisions/
   - .obsidian/Context/
3. Quality filter to suppress low-signal snippets.
4. Stable lesson_hash for duplicate detection.
5. Auto-generated .obsidian/Lessons-Index.md.
6. Weekly review-only maintenance report for stale/duplicate lessons.

## Obsidian Structure Used

The implemented vault root is .obsidian with:

- .obsidian/permanent/
- .obsidian/logs/
- .obsidian/references/
- .obsidian/inbox/
- .obsidian/fleeting/
- .obsidian/templates/
- .obsidian/graphify/
- .obsidian/chats/
- .obsidian/Patterns/
- .obsidian/Mistakes/
- .obsidian/Decisions/
- .obsidian/Context/
- .obsidian/Lessons-Index.md

## Graphify Implementation Pattern Used

1. Prefer existing graph artifacts before rebuild:
   - graphify-out/graph.json
   - .graphify/graph.json
2. Query-first commands:
   - graphify query
   - graphify path
   - graphify explain
3. Rebuild only when requested or structurally stale.
4. Keep graphify-out/cache ignored.
5. Keep graph/report artifacts available for fast future sessions.

## Operational Scripts to Reuse

- scripts/copilot_to_obsidian.py
- scripts/sync_copilot_obsidian.ps1
- scripts/setup_copilot_obsidian_profile.ps1
- scripts/verify_copilot_obsidian_pipeline.ps1
- scripts/weekly_lessons_maintenance_report.py

## Suggested Session Policy

For each new task:

1. Query Graphify first for structure/relationships.
2. Read .obsidian logs/permanent/lessons second.
3. Read raw source files only for targeted implementation.
4. After work, sync/import chats and update lessons.
5. Run weekly maintenance report and review manually.

## Weekly Maintenance (Review-Only)

Run:

python scripts/weekly_lessons_maintenance_report.py --vault-dir .obsidian --stale-days 30

Output:

- .obsidian/references/weekly-lessons-maintenance.md

The report flags stale and duplicate lessons. It never auto-deletes notes.

## Porting Checklist for Another Workspace

1. Create .obsidian structure listed above.
2. Add Graphify query-first policy to workspace instructions.
3. Add chat import and lesson extraction scripts.
4. Add lessons index generation and duplicate hash logic.
5. Add verify and weekly maintenance scripts.
6. Add docs that describe proof-of-use checks.
