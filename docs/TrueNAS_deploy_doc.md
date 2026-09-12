# TrueNAS SCALE Deployment and Capability Enablement Guide

This guide describes how to deploy execlaw on TrueNAS SCALE and then enable
its optional capabilities without losing the isolation, policy, and persistence
properties of the project.

Use this document as the capability checklist. The lower-level Docker and
network troubleshooting reference remains
[`truenas-docker.md`](truenas-docker.md). TrueNAS CORE is FreeBSD-based; run a
Linux VM on CORE and apply this guide inside that VM.

## 1. Deployment model

A capability-complete deployment is not one large container. It consists of:

- One long-running execlaw control-plane container.
- One runner image from which execlaw creates isolated conversation runners.
- One local inference service, normally Ollama, vLLM, or another
  OpenAI-compatible endpoint.
- Optional supervised sidecars declared by installed plugin manifests.
- Optional external services such as Graphiti or HTTP MCP servers.
- One private ZFS dataset containing the database, encryption key, plugins,
  skills, logs, attachments, and sidecar state.

The control plane uses the host Docker socket to create runners and plugin
sidecars. Access to `/var/run/docker.sock` is effectively root-equivalent on
the Docker host. Keep the UI on a trusted LAN or VPN and never expose port
3031 directly to the public Internet.

The implementation authorities for this deployment are:

- [`../Dockerfile.control-plane`](../Dockerfile.control-plane)
- [`../Dockerfile.runner`](../Dockerfile.runner)
- [`truenas-docker.md`](truenas-docker.md)
- [`plugins.md`](plugins.md)
- [`../plugins/README.md`](../plugins/README.md)
- [`testing.md`](testing.md)

## 2. Capability readiness

| Capability | Readiness | Additional requirement |
|---|---|---|
| Web chat, event log, policy, memory | Ready | Control plane, database, local inference |
| Isolated conversation runners | Ready | Runner image, Docker socket, shared Docker network |
| Host built-in tools | Ready | Tool access enabled for the intended trust classes |
| Script plugins without sidecars | Ready | Packaged plugin ZIP and any API/OAuth credentials |
| Signal and WhatsApp | Ready | Public sidecar images, host-gateway routing, device pairing |
| Slack, Discord, SMS | Ready | Provider application/token or reachable Android gateway |
| Google Apps and Places | Ready | Google OAuth client or API key and browser-reachable callback |
| Python Sandbox | Image-dependent | `execlaw/python-sandbox-fast:0.1.0` available locally or in a registry |
| Web Scraper | Image-dependent | `execlaw/web-scraper:0.1.0` available locally or in a registry |
| Filesystem and plugin skills | Ready | Persistent `skills/**/SKILL.md` tree or skill-bearing plugin ZIP |
| MCP tools | Ready | Reviewed stdio command or reachable Streamable HTTP server |
| Always-on agents and automations | Ready | Inference, required tools/plugins, explicit budgets and trust policy |
| Graphify | Optional | Python, Graphify CLI, repository mount, local model endpoint |
| Graphiti | Optional external service | Reachable Graphiti-compatible HTTP service |
| Voice | Partial | STT/TTS endpoints; current follow-ups still apply |

Do not enable every capability at once. Complete and verify each phase below
before adding the next one.

### Host prerequisites

Before deployment, confirm:

- TrueNAS SCALE, or a Linux VM when the storage host is TrueNAS CORE.
- Docker Engine with the Compose plugin.
- Git, Node.js 20 or newer, npm, `zip`, and `sha256sum` in the source-build
  environment.
- A static LAN address or stable DNS name for TrueNAS.
- Working DNS and outbound HTTPS from Docker containers.
- A private reverse proxy or VPN if clients connect outside the trusted LAN.
- Sufficient ZFS capacity for state, source/build cache, sidecar images, and
  local model weights.
- NVIDIA Container Toolkit when Ollama or another backend uses an NVIDIA GPU.

GPU validation:

```bash
nvidia-smi
sudo docker run --rm --gpus all \
  nvidia/cuda:12.4.1-base-ubuntu22.04 nvidia-smi
```

Resolve GPU/runtime errors before starting execlaw. The control plane cannot
repair a missing host driver or container runtime.

## 3. Prepare TrueNAS storage

Create separate source and state datasets. Substitute your pool name:

```bash
sudo mkdir -p /mnt/AI_Pool/execlaw
sudo mkdir -p /mnt/AI_Pool/execlaw/backups
sudo mkdir -p /mnt/AI_Pool/execlaw/skills
sudo mkdir -p /mnt/AI_Pool/execlaw-source
sudo chown -R 1000:1000 /mnt/AI_Pool/execlaw
sudo chmod 700 /mnt/AI_Pool/execlaw
```

Keep source code outside the state dataset:

```bash
git clone https://github.com/nikolacucuk/execlaw.git \
  /mnt/AI_Pool/execlaw-source
cd /mnt/AI_Pool/execlaw-source
git checkout <reviewed-tag-or-commit>
```

Pin a reviewed tag or commit in production. Do not deploy an unreviewed moving
branch.

The state dataset will contain at least:

```text
execlaw.db                 SQLite state and event log; SQLCipher in hardened builds
.execlaw/master.key        Database, JWT, and event-log key material
.execlaw/logs/             Structured logs
plugins/                   Installed plugin staging
skills/                    Operator-managed filesystem skills
sidecars/                  Plugin sidecar state
blobs/                     Conversation attachments and artifacts
backups/                   Operator-created database backups
```

Never restore `execlaw.db` without its matching `.execlaw/master.key`. Keep
both private and include both in the same snapshot/replication policy.

## 4. Base Compose deployment

Get the numeric group that owns the Docker socket:

```bash
sudo stat -c '%g' /var/run/docker.sock
```

Create `/mnt/AI_Pool/execlaw-source/.env` with only these values, replacing the
examples:

```text
DOCKER_GID=568
EXECLAW_DATA_DIR=/mnt/AI_Pool/execlaw
OLLAMA_OPENAI_URL=http://192.168.1.76:30068/v1
```

The inference URL must be reachable from child runner containers. Do not use
`localhost` or `127.0.0.1` unless inference runs in the same container, which
is not the supported topology.

Create `compose.yaml` in the source directory:

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
      EXECLAW_RUNNER_IMAGE: execlaw/runner:truenas
      EXECLAW_RUNNER_NETWORK: execlaw-net
      EXECLAW_RPC_URL: ws://execlaw:3031
      EXECLAW_SIDECAR_BIND_HOST: 0.0.0.0
      EXECLAW_SIDECAR_CONNECT_HOST: host.docker.internal
      EXECLAW_INFERENCE_URL: ${OLLAMA_OPENAI_URL}
      RUST_LOG: info
    group_add:
      - "${DOCKER_GID}"
    extra_hosts:
      - "host.docker.internal:host-gateway"
    volumes:
      - ${EXECLAW_DATA_DIR}:/var/lib/execlaw
      - /var/run/docker.sock:/var/run/docker.sock
    networks:
      - execlaw-net

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

Build both images and start the control plane:

```bash
cd /mnt/AI_Pool/execlaw-source
sudo docker compose config
sudo docker compose build execlaw runner-image
sudo docker compose up -d execlaw
sudo docker compose logs -f execlaw
```

### Production database encryption

The `execlaw-core` crate defaults to bundled plaintext SQLite for development.
Production SQLCipher builds require the `execlaw-core/sqlcipher` feature. The
checked-in `Dockerfile.control-plane` currently runs the default Cargo build,
so a production image must change its builder command to:

```dockerfile
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --locked --release -p execlaw \
      --no-default-features -F execlaw-core/sqlcipher \
    && cp target/release/execlaw /tmp/execlaw
```

Rebuild the image and verify the compiled feature set:

```bash
sudo docker compose build --no-cache execlaw
sudo docker compose run --rm execlaw doctor
```

Do not switch an existing plaintext database to a SQLCipher binary without a
tested export/import or rekey procedure and a current backup. Record whether
the deployed image is plaintext or SQLCipher in the operations inventory.

Verify the base layer before enabling plugins:

```bash
curl --fail http://127.0.0.1:3031/api/health
sudo docker image inspect execlaw/runner:truenas
sudo docker compose exec execlaw sh -c \
  'id; ls -ln /var/run/docker.sock'
```

Open `http://TRUENAS_IP:3031`, create the controller account, and configure
**Settings -> Backends -> Standard** in External mode. Enter the reachable
OpenAI-compatible endpoint and model, save, and run the inference probe.

### Optional same-host Ollama service

If Ollama is not already deployed by TrueNAS Apps or another Compose project,
add it to the same Compose file:

```yaml
services:
  ollama:
    image: ollama/ollama:latest
    restart: unless-stopped
    gpus: all
    environment:
      OLLAMA_HOST: 0.0.0.0:11434
    volumes:
      - /mnt/AI_Pool/ollama:/root/.ollama
    networks:
      - execlaw-net
```

Pin the image to a reviewed version or digest after validation. Create and
protect `/mnt/AI_Pool/ollama`, start the service, and pull a model:

```bash
sudo mkdir -p /mnt/AI_Pool/ollama
sudo docker compose up -d ollama
sudo docker compose exec ollama ollama pull <model-tag>
sudo docker compose exec ollama ollama list
```

Use `http://ollama:11434/v1` as the backend endpoint when both services share
`execlaw-net`. If Ollama is managed elsewhere, use its LAN address and
published port. Confirm both the Ollama-native and OpenAI-compatible surfaces:

```bash
curl --fail http://TRUENAS_IP:OLLAMA_PORT/api/tags
curl --fail http://TRUENAS_IP:OLLAMA_PORT/v1/models
```

## 5. Enable core capabilities first

Before installing integrations, verify:

1. A new web chat produces an answer from the configured local model.
2. A tool-free turn survives a control-plane restart.
3. A runner container starts on demand and joins `execlaw-net`.
4. **Settings -> Tools** shows built-in tools.
5. Tool access is limited to the intended trust classes.
6. Approval-required tools produce an approval instead of executing directly.

Useful checks:

```bash
sudo docker ps --filter label=execlaw.kind=runner-workspace
sudo docker compose logs --tail=200 execlaw
```

The database is the configuration authority after setup. Environment values
such as `EXECLAW_INFERENCE_URL` are boot-time fallbacks; the saved backend row
is the per-turn authority.

## 6. Build and install plugin ZIPs

Plugin source directories are not installable. Package them from the pinned
source revision:

```bash
cd /mnt/AI_Pool/execlaw-source
npm ci --no-audit --no-fund
./scripts/package-plugins.sh
```

This builds UI panels and creates:

```text
dist/<plugin-id>-<version>.zip
dist/<plugin-id>-<version>.zip.sha256
```

Verify checksums before upload:

```bash
for checksum in dist/*.sha256; do
  (cd dist && sha256sum -c "$(basename "$checksum")")
done
```

Install one ZIP at a time through **Settings -> Plugins**, inspect its declared
tools/services/routes, enable it, and run its smallest read-only validation.
Re-enable a plugin after an upgrade when it owns a sidecar, webhook, or
long-running WebSocket connection.

### Plugin capability matrix

| Plugin | Capability | TrueNAS requirement |
|---|---|---|
| `open-meteo` | Weather and environmental data | Outbound HTTPS; no key |
| `finance-yahoo` | Market data | Outbound HTTPS; no key |
| `google-places` | Place search | Restricted Google Places API key |
| `google-apps` | Gmail, Calendar, Contacts, Tasks, Drive, identity | Google OAuth client and callback routing |
| `pushover` | Push notifications | Pushover application/user credentials |
| `slack` | Slack transport | Bot token, app token, Socket Mode, outbound WSS |
| `discord` | Discord transport | Bot token, gateway intents, outbound WSS |
| `sms-socket` | SMS/MMS transport | Android gateway reachable by LAN IP |
| `signal` | Signal transport | Signal sidecar and QR pairing |
| `whatsapp` | WhatsApp transport | WuzAPI sidecar, QR pairing, working webhook callback |
| `python-sandbox` | Persistent Python kernels | Private/local sidecar image and storage capacity |
| `web-scraper` | JavaScript-rendered scraping | Private/local sidecar image, memory, outbound HTTPS |
| `autoresearch` | Research orchestration | Search/fetch tools and sufficient model budget |
| `tool-chain` | Deterministic approved tool plans | Tools being chained and approval policy |
| `humanizer-skills` | Writing skills | Install ZIP and enable imported skills |
| `obsidian-skills` | Obsidian workflows | Install ZIP; mount a vault only if the workflow requires it |
| `identity-local-address-book` | Local identity resolution | Maintained contact JSON in persistent storage |
| `plugin-hello` | Subprocess example | Testing/development only |

## 7. Sidecar-backed plugins

The sidecar supervisor reads image, mount, environment, port, and health-check
requirements from each plugin manifest. The control plane creates sidecars
through the mounted Docker socket.

Current sidecar-backed plugins are:

| Plugin | Image | Persistent state |
|---|---|---|
| Signal | `bbernhard/signal-cli-rest-api:latest` | Linked-device identity |
| WhatsApp | `asternic/wuzapi:latest` | WuzAPI database and session keys |
| Python Sandbox | `execlaw/python-sandbox-fast:0.1.0` | Per-conversation workspaces |
| Web Scraper | `execlaw/web-scraper:0.1.0` | Browser/cache state |

Before enabling one, confirm its image exists or can be pulled:

```bash
sudo docker image inspect <image>
# or, only for an approved public/private registry image:
sudo docker pull <image>
```

This repository does not currently include Dockerfiles for the
`execlaw/python-sandbox-fast:0.1.0` or `execlaw/web-scraper:0.1.0` images.
Treat those capabilities as blocked until an approved image artifact or build
source is available. Do not substitute an unrelated image under the expected
tag.

For every created sidecar, check health and the host-gateway mapping:

```bash
sudo docker ps --filter name=execlaw-sidecar
sudo docker inspect <sidecar-name> \
  --format '{{json .HostConfig.ExtraHosts}}'
sudo docker logs --tail=200 <sidecar-name>
```

`ExtraHosts` must include `host.docker.internal:host-gateway` when the sidecar
calls a host-published control-plane or webhook address.

After pairing Signal or WhatsApp, take a ZFS snapshot. Pairing credentials live
in sidecar state and should survive image recreation.

For SMS, configure the Android phone's LAN address, such as
`ws://192.168.1.50:8787/`; `127.0.0.1` would point back into the control-plane
container.

Detailed pairing procedures are in
[`setup-walkthroughs.md`](setup-walkthroughs.md).

## 8. Credentials, OAuth, and external APIs

Enter plugin credentials through the plugin's Settings panel so secrets are
stored through the vault. Do not place API keys, refresh tokens, or provider
secrets in source files, plugin manifests, Compose YAML, or Git.

For OAuth integrations:

1. Create the provider application and enable only required scopes.
2. Register the exact callback URI shown by execlaw.
3. Ensure the callback hostname is reachable by the operator's browser.
4. Complete authorization while signed in as the controller.
5. Run a read-only tool call before enabling write/send tools.

A browser on another workstation cannot use `localhost` to reach TrueNAS.
Use the LAN/VPN hostname or reverse-proxy origin configured for the deployment,
and register that exact callback at the provider.

Restrict outbound traffic where practical, but allow DNS, HTTPS, and WSS to the
specific providers used by enabled plugins.

## 9. Skills

Execlaw supports two skill sources.

### Plugin-provided skills

Install and enable a skill-bearing plugin ZIP such as `humanizer-skills` or
`obsidian-skills`. The plugin host imports the declared skill resources into
the versioned skill store.

### Filesystem skills

Create files named exactly `SKILL.md` below the persistent skills root:

```text
/mnt/AI_Pool/execlaw/skills/research/gather/SKILL.md
```

The relative directory becomes the skill name, in this example
`research/gather`. The importer does not scan arbitrary Markdown files or the
repository checkout.

After provisioning or changing a filesystem skill:

```bash
sudo chown -R 1000:1000 /mnt/AI_Pool/execlaw/skills
sudo docker compose restart execlaw
sudo docker compose logs --tail=200 execlaw | grep -i 'filesystem skill'
```

Review imported instructions as executable policy. A skill can influence tool
selection and arguments even though it is Markdown.

## 10. MCP servers and external GitHub repositories

Use a plugin for first-party integrations that need OAuth, inbound events,
sidecars, UI panels, or host trust gating. Use MCP for reviewed third-party
request/response tool servers.

For a third-party GitHub repository:

1. Pin a release tag or full commit SHA.
2. Review its license, dependencies, install scripts, network behavior, and
   secret handling.
3. Build it in an isolated builder or dedicated image.
4. Run it as a non-root user with a read-only root filesystem where possible.
5. Mount only required directories; never mount the execlaw data root or
   Docker socket into an MCP server.
6. Place it on a dedicated Docker network if using Streamable HTTP.
7. Store bearer tokens or process environment through execlaw's MCP settings,
   not in the cloned repository.
8. Register the server in **Settings -> MCP** and inspect the discovered tool
   schemas before granting access.
9. Test a read-only call, then enable mutating tools individually.

Stdio MCP commands execute inside the control-plane container. Their binaries
and runtime dependencies must therefore be present in the control-plane image.
A host-installed binary is not visible inside the container. Prefer a separate
HTTP MCP container when a tool has a large dependency tree or needs stronger
isolation.

Treat MCP output and fetched repository content as untrusted input. MCP servers
do not receive execlaw's caller trust class and must not be treated as policy
enforcement points.

## 11. Always-on agents, routines, and automations

Enable these only after their dependencies are healthy:

1. Create the agent with the narrowest required tool set.
2. Select a configured local backend and model.
3. Set token, runtime, concurrency, and cadence limits.
4. Keep external-effect tools approval-gated during initial validation.
5. For event-only agents, verify the matching transport webhook creates a
   mailbox event before enabling unattended operation.
6. Test pause/resume and inspect run history.
7. Add routines only after one manual run succeeds.
8. Enable automations with read-only nodes first, then add external effects.

The supervisor, mailbox, checkpoints, and run history are durable in SQLite.
Container recreation should not erase agent state.

Research and `autoresearch` can consume substantial inference time and outbound
bandwidth. Configure search/fetch tools first, use bounded budgets, and verify
retention settings before scheduling recurring research.

## 12. Graphify

Graphify is a repository-analysis tool, not a prerequisite for chat. To enable
it inside the control plane, extend the runtime image with Python and Graphify,
and mount the reviewed source checkout:

```dockerfile
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       ca-certificates curl libfontconfig1 python3 python3-venv \
    && python3 -m venv /opt/graphify \
    && /opt/graphify/bin/pip install --no-cache-dir graphifyy openai \
    && ln -s /opt/graphify/bin/graphify /usr/local/bin/graphify \
    && rm -rf /var/lib/apt/lists/*
```

Add to the `execlaw` Compose service:

```yaml
working_dir: /workspace/execlaw-source
environment:
  EXECLAW_GRAPHIFY_BIN: /usr/local/bin/graphify
  EXECLAW_GRAPHIFY_GRAPH_JSON: /workspace/execlaw-source/graphify-out/graph.json
  OLLAMA_HOST: http://host.docker.internal:30068
  OLLAMA_API_KEY: local
  OLLAMA_MODEL: qwen3.5:9b
volumes:
  - /mnt/AI_Pool/execlaw-source:/workspace/execlaw-source
```

Prepare the output directory and rebuild:

```bash
sudo mkdir -p /mnt/AI_Pool/execlaw-source/graphify-out
sudo chown -R 1000:1000 /mnt/AI_Pool/execlaw-source/graphify-out
sudo docker compose build --no-cache execlaw
sudo docker compose up -d --force-recreate execlaw
```

Validate the AST-only path before semantic extraction:

```bash
sudo docker compose exec execlaw \
  graphify update /workspace/execlaw-source --force
sudo docker compose exec execlaw \
  graphify cluster-only /workspace/execlaw-source --no-label
```

Then test the built-in Graphify tool from **Settings -> Tools** or an
authenticated model turn. Keep Graphify pointed at reviewed source paths; do
not grant it the private state dataset.

## 13. Graphiti

Graphiti is a separate temporal-memory HTTP service. Deploy and secure that
service independently, then make it reachable from the control plane.

Configure the endpoint after the control plane is running:

```json
PUT /api/admin/graphiti/config
{
  "base_url": "http://graphiti:8000",
  "api_key_vault_ref": "graphiti-api-key",
  "api_key": "<write-only secret value>"
}
```

The endpoint and vault reference are stored in SQLite. The API key value is
stored only in the encrypted core vault and is never returned by the API.

Validate with authenticated admin endpoints:

```text
GET  /api/admin/graphiti/health
POST /api/admin/graphiti/test-call
```

Conversation scope, trust scope, evidence IDs, and source-event references are
derived by the host; do not configure a separate model-controlled `group_id`.

## 14. Voice

Voice is not a fully turnkey TrueNAS capability in the current tree. The
pipeline and browser PCM16 capture exist, but real chat-path reply integration,
continuous endpointing, and production audio-service packaging still have
follow-up work documented in [`voice-followups.md`](voice-followups.md).

Before enabling voice for operator use:

- Configure reachable `VoiceStt` and `VoiceTts` backend records.
- Verify Whisper-compatible STT and Kokoro/Piper-compatible TTS independently.
- Use HTTPS at the browser origin when microphone permission requires a secure
  context.
- Use headphones because server-side AEC3 remains deferred.
- Treat push-to-talk and transcript echo as validation behavior, not complete
  conversational voice.

Do not advertise voice as production-ready until an end-to-end test confirms
mic input, STT, a real agent turn, TTS playback, interruption, and event-log
replay on the deployed revision.

## 15. Network and security controls

Minimum controls for a capability-rich deployment:

- Bind port 3031 only to a trusted interface, reverse proxy, or VPN.
- Terminate TLS at the reverse proxy and preserve WebSocket upgrades.
- Restrict the ZFS dataset to the execlaw runtime UID and backup operator.
- Restrict access to the Docker socket and verify `DOCKER_GID` after TrueNAS
  upgrades.
- Do not publish dynamic sidecar ports to untrusted interfaces. If
  `EXECLAW_SIDECAR_BIND_HOST=0.0.0.0` is required for host-gateway routing,
  enforce TrueNAS firewall rules around those ports.
- Use host-enforced query-token or HMAC authentication for plugin webhooks.
- Keep provider tokens in the vault and rotate them after suspected exposure.
- Review tool trust floors and approval policy after every plugin upgrade.
- Pin external images by digest where practical.
- Scan third-party plugin ZIPs, images, skills, and MCP repositories before
  installation.
- Never give an integration container both the Docker socket and untrusted
  Internet input unless it is the reviewed control plane itself.

## 16. Backup and restore

Create an application-consistent database backup:

```bash
sudo docker compose exec execlaw execlaw backup \
  --db /var/lib/execlaw/execlaw.db \
  --to /var/lib/execlaw/backups/execlaw-$(date +%F).db
```

Then snapshot or replicate the full `/mnt/AI_Pool/execlaw` dataset. Include:

- Database backup and live database.
- `.execlaw/master.key`.
- Plugin staging and sidecar state.
- Filesystem skills.
- Attachments and artifacts.

Before restore, stop the control plane. Restore the database and matching key
as one unit, restore sidecar state if paired transports must remain linked,
then start execlaw and inspect event-log verification before accepting traffic.

Test restore procedures on a separate dataset and Compose project. A backup
that has never been restored is not yet a verified backup.

## 17. Updates and rollback

Before an update:

1. Record the deployed commit, plugin versions, and image digests.
2. Run a database backup and ZFS snapshot.
3. Review migrations and plugin manifest changes.
4. Package fresh plugin ZIPs from the same source revision as the control plane.

Update:

```bash
cd /mnt/AI_Pool/execlaw-source
git fetch --tags
git checkout <reviewed-tag-or-commit>
sudo docker compose build execlaw runner-image
sudo docker compose up -d --force-recreate execlaw
```

Do not use `docker compose down -v` during a normal update. It can remove
Docker-managed runner workspaces.

After updating, upgrade affected plugin ZIPs and re-enable sidecar-backed or
connection-owning plugins. If validation fails, stop the new control plane and
restore the matching application backup/ZFS snapshot. Do not run an older
binary against a database after irreversible migrations unless that rollback
path was explicitly tested.

## 18. Capability acceptance checklist

### Base platform

- [ ] `/api/health` returns `{"status":"ok"}`.
- [ ] The SPA loads from the TrueNAS LAN/VPN origin.
- [ ] Inference probe succeeds from the saved Standard backend.
- [ ] A runner starts, connects to `ws://execlaw:3031`, and completes a turn.
- [ ] Restarting the control plane preserves chats, users, and settings.

### Plugins and tools

- [ ] Every installed ZIP checksum matches the packaged artifact.
- [ ] Plugin version in the SPA matches its manifest.
- [ ] Sidecar image source and digest are approved.
- [ ] Sidecars are healthy and persistent state survives recreation.
- [ ] Read-only tool validation succeeds before mutating tools are enabled.
- [ ] Trust floors and approval requirements match operator intent.

### Transports

- [ ] Pairing survives sidecar and control-plane restart.
- [ ] A new inbound message creates or resumes the correct conversation.
- [ ] Outbound replies reach the originating transport exactly once.
- [ ] Webhook or WebSocket authentication is enabled.
- [ ] Unknown contacts follow the expected trust/approval path.

### Skills, MCP, and agents

- [ ] Imported skills are visible and source-attributed.
- [ ] MCP tool schemas were reviewed before access was granted.
- [ ] External repositories are pinned and isolated.
- [ ] Agent budgets, concurrency, tool access, and pause/resume are tested.
- [ ] Routine and automation external effects remain approval-gated initially.

### Operations

- [ ] Logs contain no unexpected secrets or message bodies.
- [ ] Database plus matching master key are backed up privately.
- [ ] A restore test has succeeded.
- [ ] TrueNAS firewall rules protect port 3031 and dynamic sidecar ports.
- [ ] The deployed commit, image digests, and plugin versions are recorded.

## 19. Troubleshooting order

Diagnose capability failures from the bottom of the stack upward:

1. **Storage:** ownership, free space, database/key availability.
2. **Docker:** socket group, image presence, network membership.
3. **Control plane:** health endpoint and logs.
4. **Runner:** image discovery, RPC URL, inference reachability.
5. **Inference:** `/api/tags`, `/v1/models`, then execlaw probe.
6. **Plugin:** installed version, enable state, manifest registration.
7. **Sidecar:** container health, mounts, host-gateway mapping, logs.
8. **Credentials:** provider scopes, callback URI, token expiry.
9. **Policy:** trust floor, tool access, approval state.
10. **Agent/automation:** mailbox event, budget, checkpoint, run history.

Commands used most often:

```bash
sudo docker compose ps -a
sudo docker compose logs --tail=300 execlaw
sudo docker ps --filter name=execlaw-sidecar
sudo docker ps --filter label=execlaw.kind=runner-workspace
sudo docker network inspect execlaw-net
curl --fail http://127.0.0.1:3031/api/health
```

For detailed runner, sidecar, SPA bundle, Cargo cache, WhatsApp webhook, and
Ollama troubleshooting, continue with
[`truenas-docker.md`](truenas-docker.md).
