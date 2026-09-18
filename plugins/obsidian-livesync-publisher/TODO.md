# Obsidian LiveSync Publisher Investigation TODO

This checklist is a handoff for the next investigator. Items marked complete
are supported by code or terminal evidence from this session. Items marked
open still require execution or proof on TrueNAS.

## Confirmed

- [x] Keep `/mnt/AI_Pool/obsidian_notes` out of all publisher mounts.
- [x] Use CouchDB HTTP API rather than editing CouchDB files.
- [x] Use `/mnt/AI_Pool/obsidian-vault/execlaw` as the human-readable source.
- [x] Use `/mnt/AI_Pool -> /ai_pool` as the intended sidecar mount.
- [x] Use `obsidian-vault/execlaw` as the relative source setting.
- [x] Normalize `/mnt/AI_Pool/obsidian-vault/execlaw` entered in the UI.
- [x] Persist `source_subdir` during settings save.
- [x] Preserve a redacted saved-password indicator in the UI.
- [x] Add **Check source** before **Publish now**.
- [x] Add source-directory and traversal tests.
- [x] Add a fake-CouchDB HTTP test for chunk and metadata writes.
- [x] Build ZIPs with POSIX entry names, avoiding Windows backslash entries.
- [x] Bump the sidecar image tag from `0.1.0` to `0.1.1`.
- [x] Add host hydration logic to reread staged `plugin.toml` and refresh stale
  `state_plugins.manifest_toml`.
- [x] Confirm the staged `0.1.14` manifest contained `/mnt/AI_Pool -> /ai_pool`.
- [x] Confirm CouchDB logs show authenticated `200 ok` traffic to `djenka_db`.
- [x] Rebuild the TrueNAS control-plane image with the hydration and mount
  forwarding fixes.
- [x] Confirm the publisher container has a non-empty `/mnt/AI_Pool ->
  /ai_pool` read-only bind.
- [x] Confirm the sidecar sees
  `/ai_pool/obsidian-vault/execlaw/livesync-test.md`.
- [x] Configure `EXECLAW_SIDECAR_NETWORK=ix-obsidian_default` and confirm the
  publisher shares that network with `couchdb-obsidian-livesync`.
- [x] Run **Check source** successfully.
- [x] Run **Publish now** successfully: one source file and one metadata
  document written.
- [x] Confirm CouchDB persisted
  `f:obsidian-vault/execlaw/livesync-test.md` with a referenced `h:` chunk.

## Remaining validation

- [ ] Re-run **Publish now** without changing the source and confirm zero
  metadata documents are written.
- [ ] Modify `livesync-test.md`, publish again, and confirm its metadata `_rev`
  and child `h:` hash change.
- [ ] Read the published `h:` chunk through CouchDB's HTTP API and confirm its
  `data` equals the Markdown source.
- [ ] Confirm the note appears with the expected content in the connected
  Obsidian LiveSync client.

## Completed TrueNAS evidence

```bash
grep -n "staged plugin directory is the installed artifact" \
  crates/plugin-host/src/host.rs

sudo docker compose exec execlaw \
  sh -lc 'grep -A5 -B2 "services.mounts" /var/lib/execlaw/plugins/obsidian-livesync-publisher-*/plugin.toml'

sudo docker ps -a \
  --format '{{.Names}}\t{{.Image}}\t{{.Status}}' \
  | grep -E 'obsidian|publisher'

PUBLISHER=execlaw-sidecar-obsidian-livesync-publisher-publisher
sudo docker inspect "$PUBLISHER" --format '{{json .Mounts}}'
sudo docker exec "$PUBLISHER" \
  sh -lc 'find /ai_pool/obsidian-vault/execlaw -maxdepth 2 -type f -name "*.md" -print'
```

Observed results: the sidecar has a read-only `/mnt/AI_Pool -> /ai_pool` bind,
the test Markdown file is visible, and both it and CouchDB are on
`ix-obsidian_default`. CouchDB returned a persisted `plain` metadata document
for `f:obsidian-vault/execlaw/livesync-test.md` whose `children` array refers
to an `h:` content chunk.

## Open compatibility work

- [ ] Test against the exact `djenka_db` LiveSync settings: E2EE, path
  obfuscation, chunk size, and hash compatibility.
- [ ] Test a disposable CouchDB database with a real Obsidian client in both
  directions before using the existing vault.
- [ ] Replace the CouchDB administrator account with a database-scoped user.
- [ ] Add explicit CouchDB status/auth diagnostics without logging secrets.
- [ ] Decide whether remote deletion/pruning is needed; current behavior is
  intentionally additive and non-destructive.
- [ ] Run the Python sidecar unit/integration tests inside the sidecar image:

  ```bash
  python -m unittest discover -s sidecar -p 'test_*.py'
  ```
