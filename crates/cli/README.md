# execlaw (CLI)

Operator-facing binary. Production deployment is **always bare-metal** —
the control plane is a native binary registered as a host service via
the `service-manager` crate (systemd / launchd / Windows SCM). There is
no `docker compose` deployment for the control plane.

## Subcommands

`execlaw memory export-assertion <ID> --to <FILE> --redaction-map <FILE> --consent`
exports one assertion and its verified source evidence locally. Redaction
runs on the host, requires a reviewed replacement map and explicit consent,
and never overwrites an existing destination.

Flagged eval ranges can be exported with `execlaw eval export-flagged` after
providing a local redaction map and the explicit `--consent` flag. The exporter
never sends transcript data to a service; see [testing](../../docs/testing.md)
for the offline `eval-harness replay-fixture` validator. CI can replay a
whole catalog with `eval-harness replay-fixtures --directory evals/fixtures`;
the report records each fixture's SHA-256 and validation result.
Optional `--incident-ref INC-42` and `--release-ref v2026.09.29` links are
validated as bounded identifiers and are retained in the exported fixture and
offline validation report.

- `execlaw install` — first-run install: migrate DB → register service → start it.
- `execlaw service install` / `start` / `restart` / `stop` / `uninstall` / `status` — service lifecycle.
- `execlaw doctor` — preflight checks (DB, vault, OS keyring, optional Docker for sidecars/runner).
- `execlaw db migrate` — apply pending migrations directly (run by `install` automatically).
- `execlaw db status` — print applied migration count.
- `execlaw hw rescan` — sysfs / WMI / IOKit hardware scan (JSON).
- `execlaw serve` — run the Axum server in the foreground (dev / debug; the host-service path uses this internally).
- `execlaw replay <conversation_id> --at <seq>` — reconstruct the exact prompt history, capability set, policy decision, and committed events for one turn.
- `execlaw eval flag` / `eval list` — tag regression-target event ranges for the LLM-judge harness.
- `execlaw backup` / `restore` — snapshot the SQLCipher DB and atomically swap a snapshot back in.
- `execlaw backfill-events` / `resign-events` — Phase-7 hardening: HMAC-tag historical event rows under the current key.
- `execlaw client login` / `send` / `messages` / `stop` / `control` / `approvals` / `download-artifact` / `workspace-read` / `workspace-search` / `workspace-checkpoint` / `workspace-diff` / `workspace-apply` / `workspace-restore` — versioned headless Controller API client following [`client-contract-v1.schema.json`](../../spec/client-contract-v1.schema.json). `execlaw client editor-adapter` provides an allowlisted LSP `workspace/executeCommand` bridge to the same API. Refresh tokens stay in the OS keyring; chat/control/apply/restore retries accept a caller-supplied idempotency key. Accounts requiring WebAuthn must authenticate in the SPA before importing a refresh token with the hidden-prompt `client import-refresh-token` command.

`cargo bootstrap`, `cargo start`, `cargo stop`, `cargo restart`,
`cargo svc-status`, and `cargo doctor` are convenience aliases that
forward to the equivalent `execlaw …` invocations (see
`.cargo/config.toml`).

`execlaw client qualify --context-tokens 4096` runs the configured Standard
backend's H034 qualification matrix using the keyring-backed Controller session
and prints per-check results without session credentials.
`execlaw qualify-model --context-tokens 4096` runs the same checks directly
against the local database and configured backend when an older server lacks
the endpoint. Use `--no-encrypt` only for a plaintext development database.

`execlaw client send` can attach a durable completion contract with repeatable
`--acceptance-criterion ID=DESCRIPTION`,
`--optional-acceptance-criterion ID=DESCRIPTION`, and
`--required-artifact ID=DESCRIPTION` options. A failed optional criterion is
reported as a partial result. Add `--delivery-required` when the outcome depends
on confirmed external delivery; it requires at least one criterion or artifact.
Reuse the same `--request-id` when retrying so the contract body remains identical.
The editor adapter's `execlaw.chat.send` command accepts the same contract as
its optional third argument.
To resume an eligible durable run, use `--resume-run-id RUN_ID` without
replacement `--text` or contract flags; keep a caller-supplied `--request-id`
when retrying an uncertain resume request.

The default bind address is `127.0.0.1:3031`. Override per-run with
`--bind` or persistently via Settings → General in the SPA (which writes
to `config_general.bind_address`).

## Implementation plan

All 130 enhancements are committed implementation scope under the
[implementation plan](../../docs/implementation-plan.md). This commitment is not a
claim that they have shipped: the plan owns implementation status, and the
[roadmap](../../docs/llm-harness-roadmap.md) owns acceptance criteria.

Relevant work for this component:

- [H028: release qualification](../../docs/llm-harness-roadmap.md#enhancement-028).
- [H044: terminal and headless clients](../../docs/llm-harness-roadmap.md#enhancement-044).
- [H050: operator diagnostics](../../docs/llm-harness-roadmap.md#enhancement-050).
- [H075: safe shutdown](../../docs/llm-harness-roadmap.md#enhancement-075).
- [H116: air-gapped installation](../../docs/llm-harness-roadmap.md#enhancement-116).
- [H118: machine-loss recovery](../../docs/llm-harness-roadmap.md#enhancement-118).
