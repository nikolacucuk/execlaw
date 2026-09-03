# Running execlaw on TrueNAS SCALE with Docker

This guide deploys execlaw's control plane on TrueNAS SCALE and retains all
durable state in one ZFS-backed directory. It assumes Ollama is already
running in Docker on the TrueNAS server.

TrueNAS SCALE is Linux-based and can run Docker containers. TrueNAS CORE is
FreeBSD-based; use a Linux VM on CORE, then follow this guide inside that VM.

## What this deployment runs

The `execlaw` container is the control plane: the HTTP UI/API, encrypted
SQLite event log, plugin host, scheduler, and policy engine. It talks to the
host Docker daemon to create short-lived per-conversation runner containers
and any plugin sidecars.

This is deliberately different from putting all functionality in one
container. The Docker socket mount is required for runners and sidecars. A
process with access to it has root-equivalent control of Docker, so expose the
UI only to trusted LAN users and keep the dataset private.

The live local configuration at the time this guide was written uses:

- Endpoint: `http://192.168.1.76:30068/v1`
- Model: `fredrezones55/Qwen3.6-35B-A3B-APEX:I-Mini`
- Backend mode: external (execlaw does not manage the Ollama container)
- Reasoning: disabled

The `/v1` suffix is required for the runner's OpenAI-compatible client. It
works with Ollama's OpenAI compatibility endpoint. Use the TrueNAS LAN IP and
published Ollama port rather than `localhost`; runner containers must be able
to reach it too.

## 1. Create persistent storage

In the TrueNAS UI, create a dataset such as `tank/apps/execlaw`. Its mounted
path is commonly `/mnt/AI_Pool/execlaw`; substitute your real pool and
dataset name in every command below. Create the directory and give the image's
non-root user ownership:

```bash
sudo mkdir -p /mnt/AI_Pool/execlaw/backups
sudo chown -R 1000:1000 /mnt/AI_Pool/execlaw
sudo chmod 700 /mnt/AI_Pool/execlaw
```

The following persistent files/directories will appear there after first boot:

```text
execlaw.db             Encrypted SQLite state and event log
.execlaw/master.key    The durable SQLCipher, JWT, and event-log key
plugins/               Installed plugin staging area
.execlaw/logs/         Server logs
backups/               Recommended destination for database backups
```

Do not delete or rotate `.execlaw/master.key` independently of
`execlaw.db`. The database cannot be opened and the event log cannot be
verified without the matching key.

## 2. Get the source and prepare Compose

Clone a pinned execlaw revision in a separate directory, for example
`/mnt/AI_Pool/execlaw-source`. Do not place source code in the data dataset:

```bash
git clone https://github.com/nikolacucuk/execlaw.git /mnt/AI_Pool/execlaw-source
cd /mnt/AI_Pool/execlaw-source
```

Create `.env` beside the compose file. `DOCKER_GID` must be the numeric group
that owns `/var/run/docker.sock` on the TrueNAS Docker host. First, run this
in the shell and note the number it prints:

```bash
sudo stat -c '%g' /var/run/docker.sock
```

Next, open `.env` in an editor and make its **entire content** exactly the
following three lines. Replace `568` with the number from the preceding
command:

```text
DOCKER_GID=568
EXECLAW_DATA_DIR=/mnt/AI_Pool/execlaw
OLLAMA_OPENAI_URL=http://192.168.1.76:30068/v1
```

For example:

```bash
vim .env
cat .env
```

The `cat` output must contain only these three `KEY=value` lines. Do not put
`sudo stat`, `printf`, `$SOCKET_GID`, `$(...)`, quotes, or shell redirects in
the `.env` file.

Create `compose.yaml` beside `.env`:

```yaml
services:
  execlaw:
    image: execlaw/control-plane:truenas
    build:
      context: .
      dockerfile: Dockerfile.control-plane
    restart: unless-stopped
    init: true
    ports:
      - "3031:3031"
    environment:
      # Child runner containers use these values, not the Docker host alias.
      EXECLAW_RUNNER_IMAGE: execlaw/runner:truenas
      EXECLAW_RUNNER_NETWORK: execlaw-net
      EXECLAW_RPC_URL: ws://execlaw:3031
      # Boot-time fallback; the backend record below becomes the per-turn source.
      EXECLAW_INFERENCE_URL: ${OLLAMA_OPENAI_URL}
      RUST_LOG: info
    group_add:
      - "${DOCKER_GID}"
    volumes:
      - ${EXECLAW_DATA_DIR}:/var/lib/execlaw
      - /var/run/docker.sock:/var/run/docker.sock
    networks:
      - execlaw-net

  # This profile is build-only. It does not run during `docker compose up`.
  runner-image:
    image: execlaw/runner:truenas
    build:
      context: .
      dockerfile: Dockerfile.runner
    profiles: [build]

networks:
  execlaw-net:
    name: execlaw-net
    driver: bridge
```

Build both images, then start only the control plane. On a default TrueNAS
installation, Docker's socket is root-owned, so use `sudo` consistently unless
you have explicitly granted your account access to that socket:

```bash
sudo docker compose build execlaw runner-image
sudo docker compose up -d execlaw
sudo docker compose logs -f execlaw
```

The runner image is intentionally built separately: execlaw's control plane
uses the host Docker daemon to start it on demand, one isolated container per
active conversation. The runner joins `execlaw-net`, resolves `execlaw` to the
control-plane container, and calls Ollama through the reachable LAN endpoint.
The initial setup wizard detects the mounted Docker socket directly, so it
should report Docker as available even though the minimal control-plane image
does not include the Docker CLI.

### Runner image inspection failed

If the control-plane log says `runner image not found locally` immediately
after a successful `runner-image` build, first verify the image and the socket
group ID. The number in `.env` must be the number printed by the first command
on this TrueNAS server; `568` in this guide is only an example.

```bash
sudo docker image inspect execlaw/runner:truenas --format '{{.RepoTags}}'
sudo stat -c '%g' /var/run/docker.sock
grep '^DOCKER_GID=' .env
```

The final two values must match. If they differ, edit `.env` so it contains
the socket's actual numeric group ID, then recreate the control plane:

```bash
sudo docker compose up -d --force-recreate execlaw
sudo docker compose logs --tail=100 execlaw
```

To test the socket from the same container user, run:

```bash
sudo docker compose exec execlaw sh -c 'id; ls -ln /var/run/docker.sock; curl --silent --show-error --fail --unix-socket /var/run/docker.sock http://localhost/images/execlaw/runner:truenas/json >/dev/null && echo runner-image-inspect=ok'
```

`runner-image-inspect=ok` confirms execlaw can discover and start runners.
If the command reports a permission error, correct `DOCKER_GID` as above. If
it reports a missing image, rebuild `runner-image` and rerun the check.

### SPA bundle not found

If `http://TRUENAS_IP:3031/` displays `execlaw SPA bundle not found`, the
control-plane image was built from a Dockerfile that compiled Rust before
building `web/dist/`. Update `Dockerfile.control-plane` from the current
repository revision, then rebuild both images and recreate the control plane:

```bash
cd /mnt/AI_Pool/execlaw-source
sudo docker compose build --no-cache execlaw runner-image
sudo docker compose up -d --force-recreate execlaw
sudo docker compose logs -f execlaw
```

Verify the image serves the SPA from the TrueNAS shell:

```bash
curl -I http://127.0.0.1:3031/
```

The response must be `HTTP/1.1 200 OK`. A successful rebuild also creates the
`execlaw/runner:truenas` image; the log warning that the runner image is
missing then disappears on the next control-plane restart.

### Runner build: missing `reasoning_content`

If the runner build fails with `missing field reasoning_content in initializer
of ChatMessage`, your checkout predates the runner compatibility fix. Update
`crates/runner-binary/src/turn_loop.rs` from the current repository revision,
then rebuild both images:

```bash
cd /mnt/AI_Pool/execlaw-source
sudo docker compose build --no-cache runner-image
sudo docker compose up -d --force-recreate execlaw
```

The fixed runner initializers explicitly set the optional field to `None`.

### Build failure: Cargo registry cache

If the initial build fails with messages such as `failed to unpack package`,
`File exists (os error 17)`, or a missing `Cargo.toml` below
`/usr/local/cargo/registry`, BuildKit's shared Cargo cache was corrupted by
the two parallel image builds. The Dockerfiles in the current repository lock
that cache correctly. Update your checkout, clear only build cache data, and
retry:

```bash
cd /mnt/AI_Pool/execlaw-source
git pull --ff-only
sudo docker builder prune -af
sudo docker compose build execlaw runner-image
```

`docker builder prune -af` removes cached build layers only. It does not
remove running containers, images currently in use, named volumes, or the
`/mnt/AI_Pool/execlaw` data directory. Do not use `docker system prune --volumes`
for this recovery.

## 3. Configure the backend in execlaw

Open `http://TRUENAS_IP:3031`, create the first operator account, then open
**Settings -> Backends -> Standard**. Set:

- Mode: `External`
- Endpoint: `http://192.168.1.76:30068/v1` (or your replacement from `.env`)
- Model: `fredrezones55/Qwen3.6-35B-A3B-APEX:I-Mini`
- Reasoning: leave disabled to match the current working configuration

Save, then use the inference probe from the Backends screen before sending a
chat. If it fails, test both API surfaces from the TrueNAS shell:

```bash
curl http://192.168.1.76:30068/api/tags
curl http://192.168.1.76:30068/v1/models
```

If your Ollama container is not published on the TrueNAS LAN IP, attach it to
the same `execlaw-net` network and use its Compose service name instead, for
example `http://ollama:11434/v1`. Do not use `127.0.0.1` or `localhost` in a
backend endpoint: those addresses identify the calling container, not Ollama.

## Operations and backup

Check service and child runner state:

```bash
docker compose ps
docker ps --filter label=execlaw.kind=runner-workspace
docker compose logs --tail=200 execlaw
```

Back up the database through execlaw so the snapshot is internally consistent:

```bash
docker compose exec execlaw execlaw backup \
  --db /var/lib/execlaw/execlaw.db \
  --to /var/lib/execlaw/backups/execlaw-$(date +%F).db
```

Back up both the `backups/` directory and `.execlaw/master.key` using a private
TrueNAS snapshot/replication target. Stop the control plane before restoring a
snapshot. Runner scratch volumes are Docker-managed volumes named
`execlaw-runner-<group-id>`; they persist under the Docker application's own
storage dataset and are not the source of conversation state. The event log
and all durable agent state are in `EXECLAW_DATA_DIR`.

## Updating

From the source directory, fetch the desired revision and rebuild both images:

```bash
git pull --ff-only
docker compose build execlaw runner-image
docker compose up -d --force-recreate execlaw
```

Do not run `docker compose down -v`: the `-v` option can remove Docker-managed
runner workspace volumes. The mounted ZFS data dataset remains intact, but
there is no reason to discard the runner volumes during a routine update.