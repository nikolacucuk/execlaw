# Workspace coding jobs

The `workspace-coding` plugin binds model tools to a durable run's isolated
checkout. It exposes bounded UTF-8 read/search and SHA-256 checked patch tools,
plus terminal and language-server jobs. The registered source root changes
only through the existing reviewed diff/apply route.

## Configure a toolchain

The toolchain is a Controller-owned SQLite setting. It selects one OCI image
and a map from language IDs to LSP argv vectors. Floating tags are rejected.
The image must already be present in the local Docker daemon when a job starts.

Build the pinned development image with:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/build-workspace-toolchain.ps1
```

The script prints the resulting local image digest. A Controller can configure
that exact local digest through `PUT /api/admin/workspace-execution` and set
`approve_image_digest: true`; execlaw records a per-image Controller approval
in artifact-provenance history. For a published image, the repository workflow publishes a
multi-platform image with build provenance when a `workspace-toolchain-vX.Y.Z`
tag is dispatched. Production configuration uses that published
`name@sha256:<digest>` reference and does not require a local override.

Example request:

```json
{
  "image_reference": "ghcr.io/owner/execlaw-workspace-toolchain@sha256:<64-hex-digest>",
  "language_servers": {
    "rust": ["rust-analyzer"],
    "typescript": ["typescript-language-server", "--stdio"]
  },
  "approve_image_digest": false
}
```

`approve_image_digest` is a per-image decision that explicitly accepts the
exact digest without remote attestation. It never changes the global
artifact-verification policy. Without verified provenance or an explicit
Controller approval for the exact digest, configuration and execution fail
closed.

## Job boundaries

`workspace.run` accepts an argv array. It does not build a host shell command.
`workspace.diagnostics` accepts a workspace-relative file path and language ID;
the language-server command comes from the Controller configuration. Both
require Controller trust, `workspace.process`, and a durable run binding.

The host copies the isolated checkout into a disposable snapshot, excluding
secret paths, `.git`, build outputs, links, junctions, and multiply linked files.
The container mounts that snapshot read-only at `/workspace`; only `/tmp` is
writable. Docker enforces `network=none`, a read-only root filesystem, no Linux
capabilities, `no-new-privileges`, 2 GiB memory, 2 CPUs, 128 processes, a
180-second hard timeout, and bounded output. The process receives no execlaw
credentials or host environment. A command cannot modify the run checkout or
registered source root; edits use `workspace.apply_patch` and then the reviewed
diff flow.

`workspace.diagnostics` speaks LSP 3.17 over container stdio. It supports pull
and push diagnostics, answers bounded client-configuration requests, and waits
briefly after an empty pull report for the corresponding push notification.
The diagnostic text is checked against the mounted file so stale request text
cannot produce an authoritative result.

Both operations use durable `(run_id, job_id)` receipts. A retry with the same
request hash returns the stored result; conflicting content under the same
identity is rejected. Expired leases can be reclaimed. The toolchain's own
`timeout` process bounds runtime even if the control process is interrupted.

## Qualification

Run the Docker-backed smoke tests against a local digest printed by the build
script:

```powershell
cargo run -p execlaw-container-manager --example workspace_toolchain_smoke -- sha256:<local-image-id>
node scripts/qualification/workspace-lsp-smoke.mjs sha256:<local-image-id>
```

The first command executes a multi-file Rust workspace test and requests a Rust
Analyzer diagnostic. The second independently checks that external networking
is unavailable, the workspace mount is read-only, and the image's Rust LSP
returns a diagnostic. These local-image checks do not replace Controller
approval or an attested release image. The held-out agent repair benchmark and
H023-H029 write-authority qualification remain required before H040 is complete.
