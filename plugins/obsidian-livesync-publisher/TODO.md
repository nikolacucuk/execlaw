# Obsidian LiveSync Publisher TODO

## Current blocker

- [x] Fix the stale TrueNAS sidecar image problem by changing the manifest
  image from `0.1.0` to `0.1.1`.
- [x] Mount `/mnt/AI_Pool` at `/ai_pool` and resolve the source
  folder from the relative `source_subdir` setting.
- [x] Persist `source_subdir` when saving plugin settings.
- [x] Preserve the stored password when the UI receives `<redacted>`.
- [x] Add **Check source** before **Publish now**.
- [ ] On TrueNAS, build and verify `execlaw/obsidian-livesync-publisher:0.1.1`.
- [ ] If the settings page reports `sidecar is not healthy`, inspect the
  sidecar container state and logs before retrying the UI.
- [ ] On TrueNAS, install the matching `0.1.12` ZIP and verify the staged
  manifest has `source = "/mnt/AI_Pool"` and `target = "/ai_pool"`.
- [ ] Run **Check source** and confirm `/vault/execlaw` resolves to the
  configured folder and reports at least one markdown file.
- [ ] Run **Publish now** and capture the result counters.
- [ ] Confirm the resulting `execlaw/` note appears in Obsidian LiveSync.

## Local verification

- [x] Validate Rhai, Python, and panel JavaScript diagnostics in VS Code.
- [x] Add source-directory and traversal tests.
- [x] Add a fake-CouchDB HTTP integration test for metadata and chunk writes.
- [ ] Run `python -m unittest discover -s sidecar -p 'test_*.py'` in the
  sidecar image or an environment with Python installed.
- [ ] Run the sidecar image and exercise `/healthz`, `/v1/check`, and
  `/v1/publish` against a disposable CouchDB database.

## Compatibility and security follow-up

- [ ] Test against the exact LiveSync settings used by `djenka_db`, including
  E2EE, path obfuscation, chunk size, and hash compatibility.
- [ ] Replace the admin CouchDB account with a database-scoped publisher user.
- [ ] Add explicit CouchDB response/authentication diagnostics without logging
  credentials.
- [ ] Add remote deletion/pruning only after a separate approval design; the
  current publisher is intentionally additive and non-destructive.