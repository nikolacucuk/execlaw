# Testing execlaw

This document defines the repository test tiers and the external prerequisites
needed to exercise capability surfaces that cannot run in an offline unit-test
process.

The entry point is:

```powershell
pwsh -File scripts/test-all.ps1
```

The default run checks documentation links, plugin manifest identity, Rust
formatting/tests/clippy, and SPA tests/typecheck/build. Optional switches add
expensive or environment-dependent tiers:

```powershell
pwsh -File scripts/test-all.ps1 \
  -IncludeSqlCipher \
  -IncludePackaging \
  -IncludeDocker \
  -LiveBaseUrl http://192.168.1.76:3031
```

Use `-SkipRust` or `-SkipWeb` only to isolate a failure. A release candidate
must not omit either default tier.

## Coverage matrix

| Surface | Default automated coverage | Additional acceptance test |
|---|---|---|
| Event log, HMAC, replay, migrations | Core unit/integration tests | SQLCipher tier and backup/restore drill |
| Conversation FSM and memory | Core, session, skills tests | Restart a staged deployment and verify replay |
| Trust ladder and Rule of Two | Policy and server tests | Unknown-contact transport test with sideband approval |
| In-process turn executor | Runner-local adversarial tests | Local inference turn with real tool calls |
| Container runner protocol | Server WebSocket integration tests | `-IncludeDocker` runner image lifecycle test |
| Model adapters and inference clients | Adapter/API unit tests with mock servers | Probe each deployed local backend/model |
| Built-in tools | Core/server tests | Read-only smoke calls, then approval-gated effects |
| Plugin manifest and ZIP lifecycle | Plugin SDK/host/server tests | `-IncludePackaging`, upload, enable, disable, upgrade |
| Rhai plugin behavior | Script tests against shipped source | Provider sandbox or paired-device test |
| Webhook authentication | Server adversarial tests | Signed provider webhook through reverse proxy |
| Signal and WhatsApp | Manifest/script tests | QR pair, inbound, outbound, attachment, restart |
| Slack and Discord | Script tests | OAuth/token pairing and gateway reconnect |
| SMS | Script tests | Android gateway reconnect and cursor rehydrate |
| Google Apps and Places | Script/server tests | OAuth refresh and restricted-key read-only calls |
| Weather and finance | Manifest/script tests | Live upstream read-only calls |
| Python Sandbox | Host wiring tests | Approved sidecar image, kernel persistence, artifact output |
| Web Scraper | Install/admin integration test | Approved sidecar image and bounded dynamic-page fetch |
| Research and tool chains | Core/server integration tests | Bounded research job and approval halt/resume |
| Agents, routines, automations | Core/server/SPA tests | Scheduled and event-only runs across restart |
| MCP | Client/server tests | Reviewed stdio and HTTP server discovery/call |
| Graphify and Graphiti | Server/API and SPA tests | Local graph build and Graphiti health/test-call |
| Durable runs and steps | Core transition/reopen tests | Process-kill matrix through the production turn driver after it is wired |
| Tool contracts and failures | Core, plugin-host, MCP, runner-local tests | Container-runner typed-failure parity after protocol migration |
| Local endpoint policy | Policy crate adversarial tests and server adapter tests | Exercise each configured LAN/VPN endpoint and inspect persisted resolution |
| Memory assertions/jobs | Core evidence/trust/reopen tests and skills capture tests | Production memory-extraction worker after it is wired |
| Artifact provenance | Core/host/container tests plus packaging checks | Verify detached release bundles through bundled install on every platform |
| Voice | Pipeline/server/SPA tests | Real STT -> agent -> TTS test when deferred wiring lands |
| SPA and accessibility contracts | Vitest and TypeScript | Browser smoke at desktop/mobile widths |
| Deployment | Documentation checks | Live health, OpenAPI, SPA, backup/restore |

## Test layers

### Offline deterministic

These tests must run without Docker, a GPU, provider credentials, or Internet
access:

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace -- -D warnings
npm --prefix web test
npm --prefix web run lint
npm --prefix web run build
```

The turn-executor tests include malformed tool arguments, tool failure pairing,
round-limit cancellation, zero-tool-budget text turns, history hydration,
spotlighting, and context summarization.

Focused Rust tests are implemented beside the P0 stores and policies, including
expired-lease recovery, idempotent outbox completion, schema registration and
result rejection, retry/circuit behavior, DNS rebinding resistance, memory
evidence/supersession/trust filtering, v2 chain tampering/key rotation, and
artifact allowlist/digest/override checks.

On this Windows host, Application Control may block execution of locally built
Rust test binaries. It can also block the PowerShell harness itself; a
`PSSecurityException` is an environment limitation, not a passing test. Run the
same focused tests on an allowed Windows developer shell or CI runner and record
the result. The existence of the tests does not substitute for executing them.

### SQLCipher

Run when storage, vault, migrations, release packaging, or backup behavior
changes:

```bash
cargo test --workspace --no-default-features -F execlaw-core/sqlcipher
```

On Windows this requires the production OpenSSL build prerequisites. The test
harness does not silently fall back to plaintext SQLite.

### Plugin artifacts

Run when any plugin manifest, script, schema, or UI panel changes:

```powershell
pwsh -File scripts/test-all.ps1 -SkipRust -SkipWeb -IncludePackaging
```

The packaging stage requires Node.js, npm, and the archive tools used by the
platform packaging script. Verify generated checksums before installation.

### Docker runner

Build the runner image, then run the ignored lifecycle test:

```bash
docker build -f Dockerfile.runner -t execlaw/runner:dev .
```

```powershell
pwsh -File scripts/test-all.ps1 -SkipWeb -IncludeDocker
```

This launches a real runner, verifies authenticated WebSocket registration,
and checks container/volume cleanup. It does not require a live LLM because it
stops after runner registration.

### Live deployment

The harness deliberately performs only non-mutating unauthenticated checks:
health, OpenAPI, and SPA root. Example:

```powershell
pwsh -File scripts/test-all.ps1 -SkipRust -SkipWeb \
  -LiveBaseUrl http://192.168.1.76:3031
```

Authenticated feature acceptance remains manual or provider-specific because
credentials must never pass through a generic test script. Use a dedicated
test account, workspace, phone number, or provider sandbox.

## Release acceptance

Before a release:

1. Run the default suite on a clean checkout.
2. Run SQLCipher tests on a production-capable builder.
3. Package every plugin and verify all checksums.
4. Run Docker runner E2E against the release image.
5. Deploy to a staging data directory and run live smoke checks.
6. Exercise configured transports in both directions.
7. Exercise approval denial, approval acceptance, and cancellation.
8. Restart control plane and sidecars; verify replay and pairing state.
9. Create and restore an application-consistent backup.
10. Record skipped provider tests and the reason in release notes.

A skipped external test is not a pass. Release notes must distinguish passing
automated coverage from capabilities that were not exercised because hardware,
credentials, or third-party services were unavailable.
