# Obsidian Memory Instructions

Purpose: reduce token usage by reusing vault notes before broad file reads.

When to load this file
- Load for planning, architecture navigation, multi-step tasks, and session continuity.

Rules
- Read Graphify context first when graph artifacts exist.
- Read notes from .obsidian/permanent/ and .obsidian/logs/ before broad code scans.
- Write durable decisions to .obsidian/permanent/ and session outcomes to .obsidian/logs/latest-session.md.
- During chat-import flows, extract high-signal lessons into .obsidian/Patterns/, .obsidian/Mistakes/, .obsidian/Decisions/, and .obsidian/Context/.
- Regenerate .obsidian/Lessons-Index.md after lesson extraction.
- Run scripts/weekly_lessons_maintenance_report.py weekly and review the report before any cleanup.
- Keep one concept per permanent note and use YAML frontmatter.
- Prefer wikilinks between related notes.

Task start order
1. Graphify query/path/explain
2. Obsidian notes lookup
3. Targeted source reads

Do not
- Do not read large folders by default when graph or notes provide enough context.
- Do not overwrite notes without preserving existing decisions.
- Do not write low-signal boilerplate as lessons; apply quality filtering.
