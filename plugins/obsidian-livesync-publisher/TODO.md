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

## Current blocker

- [ ] Confirm the TrueNAS control-plane Docker image contains the
  `host.rs` hydration fix. Earlier TrueNAS builds pulled commits that changed
  only plugin files.
- [ ] Confirm the running registered sidecar has a non-empty `mounts` list.
- [ ] Confirm `docker inspect` no longer returns `Mounts: []`.
- [ ] Confirm the sidecar sees
  `/ai_pool/obsidian-vault/execlaw/livesync-test.md`.
- [ ] Confirm `/healthz` returns HTTP 200 from the running sidecar.
- [ ] Run **Check source** and record the returned path/file count.
- [ ] Run **Publish now** and record `files_seen`, `metadata_written`,
  `files_unchanged`, and `chunks_written`.

## Exact evidence to collect next

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
sudo docker logs "$PUBLISHER"
sudo docker exec "$PUBLISHER" \
  sh -lc 'find /ai_pool/obsidian-vault/execlaw -maxdepth 2 -type f -name "*.md" -print'
```

Interpretation:

- staged manifest wrong: plugin ZIP/install problem;
- staged manifest correct, `Mounts: []`: host hydration/registration problem;
- mount present, source absent: host path/permissions problem;
- source present, health fails: sidecar image/startup problem;
- source and health pass, publish fails: CouchDB URL/auth/network or
  LiveSync document compatibility problem.

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
