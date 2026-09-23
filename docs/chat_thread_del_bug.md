# Chat Thread Deletion Bug Handoff

## Purpose

This document hands off an unresolved production bug to the next LLM or engineer.

**Problem:** Chat threads delete correctly in the local Windows deployment, but do not disappear from the TrueNAS deployment at `http://192.168.1.76:3031`. Several source and Docker rebuilds have been performed. The underlying production failure has not yet been captured reliably.

Do not assume this is an archive setting. The intended operation is a hard delete of the conversation, event history, transport mapping, and archive projection.

## Rules and Safety

- Read `AGENTS.md` and `docs/architecture.md` before editing.
- Do not delete files in `/mnt/AI_Pool/execlaw-source` to remove chats. That directory is source code, not application state.
- TrueNAS state is in the encrypted SQLCipher database:
  - Host: `/mnt/AI_Pool/execlaw/execlaw.db`
  - Container: `/var/lib/execlaw/execlaw.db`
  - Key: `/mnt/AI_Pool/execlaw/.execlaw/master.key`
- Do not use host `sqlite3` against the encrypted DB. Use the execlaw CLI inside the control-plane image, which opens SQLCipher with the matching key.
- Take an application backup before manual data repair.

## Current State

### Local deployment

A local development deployment is running on Windows:

- SPA: `http://127.0.0.1:5174/`
- API: `http://127.0.0.1:3031/`
- Disposable DB: `target-local-ui-verification/execlaw.db`
- The control plane can be started without Docker on PATH to bypass runner-image builds.

Local browser verification succeeded:

1. The target thread was `8f0cc2fb-847c-4c78-acab-26b5d6175e21`.
2. `DELETE /api/chats/8f0cc2fb-847c-4c78-acab-26b5d6175e21` returned `200` and `{"existed":true}`.
3. A follow-up `GET /api/chats` omitted that ID.
4. A full SPA reload rendered only the remaining thread.

### TrueNAS deployment

Deployment layout from `docs/truenas-docker.md`:

- Source checkout: `/mnt/AI_Pool/execlaw-source`
- Persistent data: `/mnt/AI_Pool/execlaw`
- Compose service: `execlaw`
- Image: `execlaw/control-plane:truenas`
- Control plane container: `execlaw-source-execlaw-1`
- UI/API: `http://192.168.1.76:3031`

Recent TrueNAS evidence:

- A successful no-cache Docker build completed.
- `execlaw/control-plane:truenas` was exported and the container was recreated.
- `docker compose ps -a` showed exactly one control-plane container binding `0.0.0.0:3031`.
- The checkout advanced through the deletion commits listed below.
- `docker compose logs --since 5m execlaw` showed sidecar image-pull failures but did not show a deletion handler log.

Sidecar failures for `execlaw/python-sandbox-fast:0.1.0` and `execlaw/web-scraper:0.2.0` are unrelated to conversation deletion.

## Relevant Code Paths

### Server route

`crates/server/src/chats.rs::delete_thread`

- Route: `DELETE /api/chats/{conversation_id}`.
- Requires authenticated user.
- Calls `ConversationStore::delete`.
- Cancels an in-flight turn and schedules best-effort Python sandbox workdir cleanup.
- Returns JSON `{ "conversation_id": ..., "existed": true|false }`.
- Current code logs:
  - `conversation deletion requested`
  - `conversation deleted`

### Deletion transaction

`crates/core/src/conversation.rs::ConversationStore::delete`

- Runs in one SQLite transaction.
- Deletes conversation-owned projections before events/conversation rows.
- Temporarily drops and restores the `memory_evidence_append_only_delete` trigger.
- Deletes archive projections in `message_archive_conversations` and `message_archive_messages`.

Current cleanup lists only direct `conversation_id` owners:

```rust
[
    "eval_flagged",
    "log_entries",
    "memory_reflections",
    "state_runs",
    "state_graphiti_jobs",
    "memory_jobs",
    "memory_evidence",
    "state_chain_runs",
]
```

and:

```rust
[
    "state_outbox",
    "state_attachments",
    "state_research_jobs",
    "state_skill_invocations",
    "state_routine_runs",
    "state_reply_drafts",
    "state_chain_plans",
    "state_event_integrity_heads",
    "state_events",
    "transport_conversations",
    "state_conversations",
]
```

### SPA deletion flow

`web/src/chat/Sidebar.tsx`

- Confirms deletion.
- Calls `deleteThread`.
- Requires `existed === true`.
- Removes the local thread and clears active thread if applicable.
- Refetches `GET /api/chats`.
- Raises an error if the deleted ID is returned by the server list.

`web/src/api/client.ts`

- Now recognizes both error shapes:
  - `{ "error": { "code": "...", "message": "..." } }`
  - `{ "error": "delete: sqlite error: ..." }`
- This was added because deletion route failures returned a string error but the SPA previously reduced them to generic `Internal Server Error`.

## Verified Bugs Fixed

### 1. Invalid direct durable-run-step delete

Local reproduction originally returned:

```text
delete: sqlite error: no such column: conversation_id in DELETE FROM state_run_steps WHERE conversation_id = ?1
```

Cause:

- `state_run_steps` has `run_id`, not `conversation_id`.
- It already cascades when `state_runs` is deleted.

Fix:

- Removed `state_run_steps` from the generic `DELETE ... WHERE conversation_id` list.

### 2. Invalid direct chain-run-step delete

After the first fix, local reproduction returned:

```text
delete: sqlite error: no such column: conversation_id in DELETE FROM state_chain_run_steps WHERE conversation_id = ?1
```

Cause:

- `state_chain_run_steps` has `run_id`, not `conversation_id`.
- It already cascades when `state_chain_runs` is deleted.

Fix:

- Removed `state_chain_run_steps` from the generic `DELETE ... WHERE conversation_id` list.

### 3. Hidden server error in the SPA

Cause:

- The deletion route uses `{ "error": "delete: ..." }`.
- `readServerMessage` only parsed object-shaped errors.

Fix:

- `web/src/api/client.ts` now returns the string error to callers.
- Test added in `web/src/__tests__/client.test.ts`.
- Focused test passed: `npm.cmd --prefix web test -- client.test.ts --maxWorkers=2` (15 tests).

## Commit History

Recent commits on `main` related to the bug include:

```text
6998cd6 CLI update
f70cdce Delition loging for chats
117cbb7 Fixing the thread delition
cc434c3 Deleting the Threads fix
0277251 Deleting Treads fix
5a5130e Fixing the delition of the execlaw thread
ed24466 Fixing execlaw Threds delition
0a0953e fixing threds delition and fixing updates for whats app and signal threads
817533a WhatsApp and Signal Threads independent
9d8f8df fixing the delition of the chats
eaabd5a Deling thread bug fix
```

Do not assume every commit was deployed to TrueNAS. Confirm with:

```bash
cd /mnt/AI_Pool/execlaw-source
git rev-parse --short HEAD
```

## What Was Tried and Why It Was Insufficient

### Successful local API/browser test

This proves the fixed code works against a freshly created/simple local state. It does **not** prove TrueNAS production DB state can satisfy all foreign-key/trigger constraints.

### Repeated TrueNAS Docker rebuilds

TrueNAS successfully ran variations of:

```bash
sudo docker compose build --no-cache execlaw runner-image
sudo docker compose up -d --force-recreate execlaw
```

The build output showed source copied and release binary built. This did not expose a production deletion error because no reliable post-click error/log capture occurred.

### Grepping the executable for table names

This was a bad diagnostic and must not be repeated. `state_run_steps` and `state_chain_run_steps` remain legitimate strings in migrations and run-management code after the delete fix. Their presence does not prove the obsolete `DELETE ... WHERE conversation_id` SQL remains.

### Raw server log filtering

Before `f70cdce`, successful deletion emitted no handler-specific log. An empty grep could mean no request, a success, stale browser routing, or log-window mismatch. The logging commit improves this but must be deployed.

### Browser developer tools

The user had difficulty opening F12 DevTools. Do not make progress depend on DevTools. Prefer the CLI recovery commands below or server logs after the logging revision is deployed.

### Manual host `sqlite3`

Not attempted as a fix. It is unsafe because TrueNAS DB is encrypted SQLCipher and depends on the matching master key.

## New CLI Recovery Commands

`6998cd6` adds these top-level commands:

```text
execlaw list-threads
execlaw delete-thread <conversation-id> --i-understand-this-deletes-history
```

They use execlaw's database-opening path and the same `ConversationStore::delete` transaction as the API.

Important: `execlaw` is not installed on the TrueNAS host shell. Run it through the rebuilt container:

```bash
sudo docker compose run --rm --no-deps execlaw list-threads \
  --db /var/lib/execlaw/execlaw.db
```

The literal placeholder `<conversation-id>` is not a command. Replace it with an actual ID printed by `list-threads`.

## Safe Manual Cleanup Procedure

This is intended only after the `6998cd6` CLI revision is deployed.

1. Confirm revision and rebuild:

```bash
cd /mnt/AI_Pool/execlaw-source
git pull --ff-only
git rev-parse --short HEAD
sudo docker compose build --no-cache execlaw runner-image
```

2. Stop the control plane. Do not edit its DB while it runs:

```bash
sudo docker compose stop execlaw
```

3. Create an application-consistent encrypted backup:

```bash
sudo docker compose run --rm --no-deps execlaw backup \
  --db /var/lib/execlaw/execlaw.db \
  --to /var/lib/execlaw/backups/pre-thread-cleanup-$(date +%F-%H%M%S).db
```

The explicit `--db` is required: the image sets `HOME=/var/lib/execlaw`,
but the CLI default is `$HOME/.execlaw/execlaw.db`, not the database path
passed to `serve`. Stop if backup fails; do not proceed to deletion.
Retain the matching `.execlaw/master.key` securely with your recovery materials.

4. List the stored conversations:

```bash
sudo docker compose run --rm --no-deps execlaw list-threads \
  --db /var/lib/execlaw/execlaw.db
```

Output is tab-separated:

```text
conversation-id    display-name-or-(unnamed)    last-activity-unix-seconds
```

5. Delete **one** selected old ID. Replace `REAL-ID` exactly; do not include angle brackets:

```bash
sudo docker compose run --rm --no-deps execlaw delete-thread "REAL-ID" \
  --db /var/lib/execlaw/execlaw.db \
  --i-understand-this-deletes-history
```

6. Start the service and hard-refresh the browser:

```bash
sudo docker compose up -d execlaw
```

Expected CLI result:

```text
conversation REAL-ID deleted
```

If it reports a SQL/database error, preserve the full message. That is the next specific production-only constraint to fix in `ConversationStore::delete`.

## Recommended Next Investigation

1. Do not add more generic table names to the delete loops without a concrete production error. The prior two failures were caused by adding tables that do not own `conversation_id`.
2. Deploy `6998cd6` and use `list-threads`/`delete-thread` via `docker compose run` to bypass the browser entirely.
3. If CLI deletion fails, add a test fixture that reproduces the exact dependent table/row relationship indicated by the error.
4. If CLI deletion succeeds but UI deletion does not, focus on API auth/routing and the SPA request, not the database.
5. Consider replacing the manual table lists in `ConversationStore::delete` with a tested dependency-aware deletion plan. SQLite does not provide a safe generic "delete every table containing conversation_id" mechanism; explicit ordered ownership remains preferable.

## Validation Record

Completed:

- Local browser deletion: API `200`, deleted thread absent from next `GET /api/chats`, sidebar correct after full reload.
- Local focused SPA client test for string-shaped errors: 15 passing tests.
- CLI additions were type-checked with `cargo check --locked -p execlaw --target-dir target-cli-delete-check` and had no editor diagnostics.
- `graphify update .` run after code changes.

Not completed:

- A production TrueNAS deletion request with the latest deployed error visibility captured.
- A production TrueNAS CLI `delete-thread` result using the new `6998cd6` command.
- Full Rust workspace test suite after the CLI additions.

## Important Paths

| Path | Purpose |
|---|---|
| `crates/core/src/conversation.rs` | Hard deletion transaction |
| `crates/server/src/chats.rs` | HTTP delete route and audit logs |
| `web/src/chat/Sidebar.tsx` | Delete UI and post-delete list verification |
| `web/src/api/client.ts` | Server error parsing |
| `crates/cli/src/main.rs` | CLI recovery commands |
| `crates/core/migrations/0012_chain_plans_runs.sql` | Chain run / step foreign-key relationship |
| `crates/core/migrations/0017_durable_runs.sql` | Durable run / step foreign-key relationship |
| `docs/truenas-docker.md` | TrueNAS data directory and Compose deployment |
| `AGENTS.md` | Repo rules and architecture constraints |
