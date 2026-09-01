# execlaw on TrueNAS with Docker + Ollama (NVIDIA GPU)

This guide gives you a full, repeatable setup for running:

- execlaw control plane in Docker
- Ollama in Docker on the same TrueNAS host
- NVIDIA GPU acceleration for Ollama
- Docker-backed execlaw runners and plugin sidecars

It is written to match this repository's current runtime model:

- The repository includes [Dockerfile.runner](../Dockerfile.runner) for runner containers.
- The control-plane Docker image is not shipped as a ready-made production image, so you build one locally.

## 1. Target architecture

You will run two long-lived containers:

1. ollama
2. execlaw control plane

The execlaw container talks to the host Docker daemon through /var/run/docker.sock so it can spawn:

- per-conversation runner containers
- plugin sidecars

## 2. Prerequisites

Before starting, confirm on TrueNAS SCALE host:

1. Docker is available and working.
2. NVIDIA GPU is visible to containers.
3. NVIDIA Container Toolkit support is enabled.
4. You have a clone of this repository on the TrueNAS host.

Quick checks:

~~~bash
docker version
docker compose version
nvidia-smi
docker run --rm --gpus all nvidia/cuda:12.4.1-base-ubuntu22.04 nvidia-smi
~~~

If the CUDA test container cannot see the GPU, fix that first.

## 3. Create persistent datasets

Create datasets/directories (adjust pool name):

~~~bash
mkdir -p /mnt/tank/apps/execlaw
mkdir -p /mnt/tank/apps/ollama
mkdir -p /mnt/tank/apps/execlaw-stack
~~~

Suggested ownership (adjust UID/GID to your Docker runtime user model):

~~~bash
chown -R 1000:1000 /mnt/tank/apps/execlaw /mnt/tank/apps/ollama /mnt/tank/apps/execlaw-stack
~~~

## 4. Build execlaw images

From repo root on TrueNAS host:

~~~bash
cd /path/to/execlaw
~~~

### 4.1 Build runner image (required)

~~~bash
docker build -f Dockerfile.runner -t execlaw/runner:dev .
~~~

### 4.2 Create control-plane Dockerfile

Create Dockerfile.control-plane in repo root with this content:

~~~dockerfile
# syntax=docker/dockerfile:1.7

FROM rust:1.90-bookworm AS builder
WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY plugins ./plugins
COPY vendor ./vendor
COPY web ./web
COPY scripts ./scripts
COPY templates ./templates
COPY spec ./spec

# Build CLI binary that runs the control plane.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release -p execlaw \
    && cp target/release/execlaw /tmp/execlaw

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -u 1000 -s /usr/sbin/nologin execlaw
RUN mkdir -p /var/lib/execlaw && chown -R execlaw:execlaw /var/lib/execlaw

COPY --from=builder /tmp/execlaw /usr/local/bin/execlaw

USER execlaw
WORKDIR /var/lib/execlaw

EXPOSE 3031

ENTRYPOINT ["/usr/local/bin/execlaw"]
CMD ["serve", "--db", "/var/lib/execlaw/execlaw.db", "--bind", "0.0.0.0:3031"]
~~~

Build it:

~~~bash
docker build -f Dockerfile.control-plane -t execlaw/control-plane:local .
~~~

## 5. Create Docker Compose stack

Create /mnt/tank/apps/execlaw-stack/compose.yaml:

~~~yaml
services:
  ollama:
    image: ollama/ollama:latest
    container_name: ollama
    restart: unless-stopped
    ports:
      - "11434:11434"
    environment:
      - OLLAMA_HOST=0.0.0.0:11434
    volumes:
      - /mnt/tank/apps/ollama:/root/.ollama
    # Preferred for modern compose with NVIDIA toolkit:
    gpus: all
    # If your compose build ignores gpus, use runtime: nvidia instead.
    # runtime: nvidia

  execlaw:
    image: execlaw/control-plane:local
    container_name: execlaw
    restart: unless-stopped
    depends_on:
      - ollama
    ports:
      - "3031:3031"
    environment:
      - RUST_LOG=info
      - EXECLAW_RUNNER_IMAGE=execlaw/runner:dev
      - EXECLAW_RPC_URL=ws://host.docker.internal:3031
      - EXECLAW_INFERENCE_URL=http://host.docker.internal:11434/v1
      - EXECLAW_INFERENCE_ENGINE=ollama
    volumes:
      - /mnt/tank/apps/execlaw:/var/lib/execlaw
      - /var/run/docker.sock:/var/run/docker.sock
    extra_hosts:
      - "host.docker.internal:host-gateway"
    command: ["serve", "--db", "/var/lib/execlaw/execlaw.db", "--bind", "0.0.0.0:3031"]
~~~

Start the stack:

~~~bash
cd /mnt/tank/apps/execlaw-stack
docker compose up -d
~~~

## 6. First boot verification

### 6.1 Check containers

~~~bash
docker ps --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"
~~~

You should see ollama and execlaw running.

### 6.2 Check execlaw health

~~~bash
curl -s http://127.0.0.1:3031/api/health
~~~

Expected:

~~~json
{"status":"ok"}
~~~

### 6.3 Pull your model in Ollama

Use the same model line you validated locally:

~~~bash
docker exec -it ollama ollama pull qwen2.5:14b-instruct
~~~

Replace with your preferred tag.

### 6.4 Open UI

Open:

- http://TRUENAS_IP:3031

Complete login/setup and confirm chat completes through Ollama.

## 7. Reuse your already-tested local configuration

If you want the same setup and state you tested locally, migrate your execlaw data directory.

From your existing machine, copy the full execlaw state folder (not only the DB):

- Windows source is typically under your user profile .execlaw directory.

Copy contents into:

- /mnt/tank/apps/execlaw

Then restart execlaw:

~~~bash
cd /mnt/tank/apps/execlaw-stack
docker compose restart execlaw
~~~

Why copy the full state directory:

1. It preserves DB rows.
2. It preserves key material and local runtime artifacts.
3. It keeps behavior closest to your validated local environment.

## 8. TrueNAS-specific operational notes

1. Keep /var/run/docker.sock mounted into execlaw, otherwise runner and sidecar spawning will fail.
2. Keep extra_hosts host.docker.internal mapping so containers can call host services reliably.
3. Ensure port 3031 is reachable from your LAN (or reverse proxy).
4. Restrict WAN exposure unless you add TLS and auth hardening.

## 9. Update workflow

When you update execlaw source:

~~~bash
cd /path/to/execlaw
docker build -f Dockerfile.runner -t execlaw/runner:dev .
docker build -f Dockerfile.control-plane -t execlaw/control-plane:local .

cd /mnt/tank/apps/execlaw-stack
docker compose up -d
~~~

## 10. Troubleshooting

### execlaw cannot talk to Docker

Symptoms:

- Runner/sidecar spawn failures
- Errors referencing Docker daemon connectivity

Checks:

~~~bash
docker exec -it execlaw ls -l /var/run/docker.sock
docker logs execlaw --tail=200
~~~

### Ollama runs but model calls fail

Check:

~~~bash
docker exec -it ollama ollama list
docker logs ollama --tail=200
~~~

If model missing, run ollama pull again.

### NVIDIA is not used by Ollama

Check host and container:

~~~bash
nvidia-smi
docker exec -it ollama nvidia-smi
~~~

If container cannot see GPU, review TrueNAS NVIDIA runtime/toolkit setup.

### host.docker.internal resolution issues

This guide sets:

- extra_hosts: host.docker.internal:host-gateway

Keep that line in compose for Linux/TrueNAS environments.

## 11. Optional hardening for production

1. Put execlaw behind a reverse proxy (HTTPS).
2. Limit inbound network exposure (LAN/VPN only).
3. Back up /mnt/tank/apps/execlaw regularly.
4. Pin image tags once stable.

## 12. Command quick reference

~~~bash
# start/restart
cd /mnt/tank/apps/execlaw-stack
docker compose up -d
docker compose restart execlaw

# logs
docker logs -f execlaw
docker logs -f ollama

# health
curl -s http://127.0.0.1:3031/api/health

# model management
docker exec -it ollama ollama list
docker exec -it ollama ollama pull qwen2.5:14b-instruct
~~~
