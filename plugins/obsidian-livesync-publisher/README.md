# Obsidian LiveSync Publisher

This plugin publishes markdown files from the execlaw export directory into
an existing Obsidian LiveSync CouchDB database. It must never read or modify
CouchDB's private filesystem data. CouchDB is accessed only through its HTTP
API.

## Storage topology

These paths are different and must not be conflated:

```text
/mnt/AI_Pool/execlaw-source
    Git checkout and Docker build context

/mnt/AI_Pool/obsidian-vault/execlaw
    Human-readable markdown source files

/mnt/AI_Pool/obsidian_notes
    CouchDB private files; never mount or edit directly
```

The intended sidecar mount is:

```text
host:           /mnt/AI_Pool
container:      /ai_pool
source_subdir:  obsidian-vault/execlaw
effective path: /ai_pool/obsidian-vault/execlaw
```

The sidecar reads markdown from the bind mount and sends LiveSync-compatible
metadata/chunk documents to CouchDB over HTTP. It does not delete remote
documents.

## Configuration

The settings page is the supported configuration surface:

```text
/settings/plugins/obsidian-livesync-publisher
```

Use:

```text
CouchDB URL:     http://couchdb-obsidian-livesync:5984
Database:        djenka_db
Username:        ncucuk
Source folder:   obsidian-vault/execlaw
```

An absolute path entered in the source-folder field is normalized by the UI:

```text
/mnt/AI_Pool/obsidian-vault/execlaw
    -> obsidian-vault/execlaw
```

The password is stored in execlaw's encrypted per-plugin vault. The API
returns `<redacted>` by design. A green save message means only that settings
were persisted; it does not prove the sidecar mount or CouchDB publish path.

Use **Check source** before **Publish now**. A successful publish reports
`files_seen`, `metadata_written`, `files_unchanged`, and `chunks_written`.

`couchdb-obsidian-livesync` is a Docker DNS name, not a TrueNAS host name. It
works only when the publisher sidecar joins the same Docker network as CouchDB.
Set the control-plane environment variable `EXECLAW_SIDECAR_NETWORK` to the
network shown by:

```bash
sudo docker inspect couchdb-obsidian-livesync \
  --format '{{range $name, $_ := .NetworkSettings.Networks}}{{$name}}{{"\n"}}{{end}}'
```

Recreate `execlaw`, remove the publisher container, and let the supervisor
create it again. If CouchDB is intentionally exposed on the TrueNAS LAN rather
than a shared Docker network, use its reachable host IP and published port in
the CouchDB URL instead.

## Confirmed investigation findings

The observed errors included:

```text
publisher source directory is missing: /vault/execlaw
publisher source directory is missing: /ai_pool/obsidian-vault/execlaw
```

The decisive TrueNAS evidence was:

```text
sudo docker inspect "$PUBLISHER" --format '{{json .Mounts}}'
[]
```

and:

```text
find: /ai_pool/obsidian-vault/execlaw: No such file or directory
```

Therefore the running publisher container had no bind mount at all. This was
not a CouchDB path problem and not merely a typo in `source_subdir`.

The staged manifest was separately verified inside the running execlaw
container and contained:

```toml
[[services.mounts]]
source = "/mnt/AI_Pool"
target = "/ai_pool"
read_only = true
```

The important discrepancy was therefore:

```text
staged plugin.toml: correct mount
running Docker container: Mounts = []
```

The CouchDB logs are healthy and show authenticated `200 ok` requests to
`djenka_db/` and continuous `_changes` traffic from LiveSync. The publisher
fails before CouchDB because its source directory is unavailable.

## Root causes found during the session

1. Early Windows `Compress-Archive` packages used backslash ZIP entry names
   such as `ui\\panel.js`. Linux staging treated those as literal names and
   reported missing UI files. Current package creation uses explicit POSIX
   entry names.
2. The sidecar image changed from `0.1.0` to `0.1.1`, but rebuilding execlaw
   alone does not build the sidecar image. The sidecar image must be built
   separately.
3. The plugin manifest evolved from `/vault/execlaw` to `/vault`, then to the
   stable parent mount `/ai_pool`. The manifest, image code, and settings must
   agree.
4. The installed plugin row in SQLite stores `manifest_toml`. Boot hydration
   originally parsed that cached value and ignored the staged `plugin.toml`.
   This allowed a staged manifest to show the correct mount while the live
   registry still had stale sidecar declarations.
5. A host fix was added to `crates/plugin-host/src/host.rs` to reread the
   staged `plugin.toml` during hydration and refresh stale cached manifest
   metadata. Confirm that this fix is present in the Docker image actually
   running on TrueNAS; a plugin-only `git pull` does not prove that.
6. The sidecar supervisor already compares `mounts` in drift detection. Once
   hydration supplies the correct registered mounts, a changed mount should
   force a stop/respawn. If `Mounts` remains empty, the failure is before
   Docker creation, in hydration/registration or in the image being run.
7. `djenka_db` is reachable independently. The CouchDB logs show successful
   authenticated `200 ok` traffic, so source discovery must be repaired before
   debugging CouchDB writes.

## Handoff diagnostic sequence

Run these checks in order. Do not infer a successful mount from a successful
plugin save or a successful image build.

### 1. Confirm the staged manifest

```bash
sudo docker compose exec execlaw \
  sh -lc 'grep -A5 -B2 "services.mounts" /var/lib/execlaw/plugins/obsidian-livesync-publisher-*/plugin.toml'
```

Expected:

```text
source = "/mnt/AI_Pool"
target = "/ai_pool"
```

### 2. Confirm the host source

```bash
sudo mkdir -p /mnt/AI_Pool/obsidian-vault/execlaw
printf '# LiveSync publisher test\n' \
  | sudo tee /mnt/AI_Pool/obsidian-vault/execlaw/livesync-test.md
```

### 3. Confirm the running sidecar image and mount

```bash
sudo docker ps -a \
  --format '{{.Names}}\t{{.Image}}\t{{.Status}}' \
  | grep -E 'obsidian|publisher'

PUBLISHER=execlaw-sidecar-obsidian-livesync-publisher-publisher
sudo docker inspect "$PUBLISHER" --format '{{json .Mounts}}'
```

The image must be:

```text
execlaw/obsidian-livesync-publisher:0.1.1
```

The mount must contain `/mnt/AI_Pool` and `/ai_pool`. An empty `[]` is a
failure; do not continue to the UI when it is empty.

### 4. Confirm the sidecar filesystem

```bash
sudo docker exec "$PUBLISHER" \
  sh -lc 'find /ai_pool/obsidian-vault/execlaw -maxdepth 2 -type f -name "*.md" -print'
```

Expected:

```text
/ai_pool/obsidian-vault/execlaw/livesync-test.md
```

### 5. Confirm health

```bash
sudo docker exec "$PUBLISHER" \
  python -c "import urllib.request; print(urllib.request.urlopen('http://127.0.0.1:8080/healthz').read().decode())"
```

Expected:

```json
{"ok":true,"source":"/ai_pool"}
```

Only after the mount and health checks pass should **Check source** and
**Publish now** be used.

## Rebuild requirements

The control plane and sidecar are separate images:

```bash
cd /mnt/AI_Pool/execlaw-source
sudo docker build --no-cache \
  -t execlaw/obsidian-livesync-publisher:0.1.1 \
  plugins/obsidian-livesync-publisher/sidecar
sudo docker compose build --no-cache execlaw
sudo docker compose up -d --force-recreate execlaw
```

If the staged manifest is correct but the sidecar still has `Mounts: []`,
remove the old container and restart the control plane:

```bash
sudo docker rm -f \
  execlaw-sidecar-obsidian-livesync-publisher-publisher 2>/dev/null || true
sudo docker compose restart execlaw
```

Then repeat the inspect command. If it remains empty, inspect the running
control-plane image and logs for hydration/registration errors; rebuilding the
plugin ZIP alone cannot fix a host hydration defect.

## LiveSync compatibility

The publisher currently writes unencrypted, non-obfuscated ordinary-file
metadata and plain leaf chunks. It is not yet proven compatible with the
existing database's E2EE, path obfuscation, custom chunk size, or alternate
hash settings. Do not publish important data until those settings are matched
or a disposable database has passed a two-way Obsidian test.

The publisher is additive and non-destructive: it does not delete remote files
or prune old chunks.
