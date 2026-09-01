# Copilot Graphify + Obsidian Workspace Setup

This guide mirrors the Graphify and Obsidian workflow integrated in this repo.

## 1) Prerequisites

- Graphify CLI installed and available on PATH (`graphify` command)
- Python available for the Obsidian lesson scripts
- Node available for preview artifact sync

Optional env vars:

- `EXECLAW_GRAPHIFY_BIN` to override the Graphify executable path
- `EXECLAW_GRAPHITI_BASE_URL` and `EXECLAW_GRAPHITI_API_KEY` for Graphiti tool/admin routes

## 2) Obsidian Vault Structure

Required directories under `.obsidian/`:

- `permanent/`, `logs/`, `references/`, `inbox/`, `fleeting/`, `templates/`
- `graphify/`, `chats/`
- `Patterns/`, `Mistakes/`, `Decisions/`, `Context/`
- `Lessons-Index.md`

Verify scaffold:

```powershell
pwsh -File scripts/verify_copilot_obsidian_pipeline.ps1 -VaultDir .obsidian
```

## 3) Import Lessons From Transcript

```powershell
pwsh -File scripts/sync_copilot_obsidian.ps1 -VaultDir .obsidian -TranscriptPath <path-to-transcript.jsonl>
```

This runs:

- `scripts/copilot_to_obsidian.py` (extract, classify, dedupe by `lesson_hash`)
- `scripts/weekly_lessons_maintenance_report.py` (review-only stale/duplicate report)

## 4) Graphify Preview Sync

```bash
node scripts/graphify_sync_preview.mjs
```

This writes `web/src/generated/graphifyPreview.json` from `graphify-out/graph.json`.

## 5) One-Command Maintenance

From repo root:

```bash
npm run graph-memory:maintain
```

This executes `scripts/post_commit_graphify_memory.ps1` and attempts:

- `graphify update .`
- graph preview sync (`node scripts/graphify_sync_preview.mjs`)
- weekly lessons report update

## 6) Optional Git Hook

Install once:

```powershell
pwsh -File scripts/install_graphify_memory_hook.ps1
```

The post-commit hook calls the same maintenance script so graph/memory outputs stay current after commits.

## 7) Graphiti Validation (Admin API)

Use authenticated admin endpoints:

- `GET /api/admin/graphiti/health`
- `POST /api/admin/graphiti/test-call`

Example test payload:

```json
{
  "args": {
    "action": "search",
    "group_id": "demo",
    "query": "find policy",
    "top_k": 5
  }
}
```
