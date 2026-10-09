# Testing execlaw

This document defines the repository test tiers and the external prerequisites
needed to exercise capability surfaces that cannot run in an offline unit-test
process.

## Implementation evidence contract

The [implementation plan](implementation-plan.md) tracks all committed
H001-H154 items. Tests listed here describe available coverage or required
acceptance work, not evidence that they passed for a particular release.
The [roadmap findings F01-F19](llm-harness-roadmap.md#review-findings-and-unresolved-verification)
remain open until their remediation and specified verification are recorded.
In particular, a store reopen test does not qualify process recovery (F06-F08),
a process-kill test does not qualify power-loss durability (F17), and the
current automation Test run can make live effects (F18/F19).

The release evidence work is [H021 real-task evaluation](llm-harness-roadmap.md#enhancement-021),
[H023 executor recovery](llm-harness-roadmap.md#enhancement-023),
[H028 packaged-artifact qualification](llm-harness-roadmap.md#enhancement-028),
[H049 performance gates](llm-harness-roadmap.md#enhancement-049),
[H058 storage-fault qualification](llm-harness-roadmap.md#enhancement-058),
[H064 schema evolution](llm-harness-roadmap.md#enhancement-064), and
[H096 attributable test evidence](llm-harness-roadmap.md#enhancement-096).
Extend enforcement evidence with [H107 model checking](llm-harness-roadmap.md#enhancement-107),
[H108 protocol fuzzing](llm-harness-roadmap.md#enhancement-108),
[H109 mutation tests](llm-harness-roadmap.md#enhancement-109), and
[H110 denied-network integration](llm-harness-roadmap.md#enhancement-110).
[H120 evidence-backed support documentation](llm-harness-roadmap.md#enhancement-120)
will tie claims to the exact artifact, platform, execution path, command,
prerequisites, and recorded result. Skipped, blocked, and failed checks must
remain visible; this plan update claims no new test executions.

H022's durable contract store is exposed to the Controller at
`GET /api/admin/runs/{run_id}/completion`, `PUT .../completion-contract`,
`POST .../completion-verifications`, `POST .../completion-artifacts`, and
`POST .../delivery-confirmation`. Run execution status is separate from
`verified_complete`: required checks, artifacts, and any required delivery
evidence must all pass. The Controller run inspector lets an operator create
an immutable contract, record criterion/artifact evidence, confirm required
delivery, and review derived status and unfinished reasons. These are
operator-entered records; production task creation does not yet derive
contracts from requests or run automatic deterministic verifiers.

For H024, `POST /api/chats/{conversation_id}/messages` accepts an optional
`Idempotency-Key` header. The key is scoped to the authenticated principal and
conversation and bound to a hash of the normalized request body. Matching
completed retries return the saved response; a changed body receives 409;
an active request returns 202; and an interrupted/expired request returns
409 with `unknown_outcome` and requires an explicit retry decision. Incognito
turns reject this header because they do not persist a replayable request.

When a request returns `unknown_outcome`, the chat surfaces a warning that an
external action may already have happened. The operator can inspect delivery
state, then explicitly retry as a new request; this creates a new idempotency
key and can repeat effects. Sink status lookup and reconciliation remain open.

The real-task scorer has small release and larger periodic suites under
[`evals/benchmark/`](../evals/benchmark/README.md). Its offline fixture mode
validates task scoring without a model and must not be reported as model
performance. Live runs use the configured local OpenAI-compatible endpoint;
benchmark records preserve failures, identities, budgets, per-task outcomes,
and uncertainty intervals.

H036 flagged event ranges can be exported locally with
`execlaw eval export-flagged <ID> --to <fixture.json> --redaction-map <map.json> --consent`.
The map is read on the host and is not included in the fixture. Run
`eval-harness replay-fixture --fixture <fixture.json> --report <validation.json>`
to check event ordering, expected transitions, tool-call pairing, and mock
responses. This also replays supported user/tool/model turns through the
production `TurnExecutor` using loopback scripted completions and fixture-only
tool dispatch; it never enables external effects. Export may attach bounded
`--incident-ref` and `--release-ref` identifiers. The CI catalog command
replays every regular JSON fixture and requires both references for each
catalog entry. Catalog/policy-only trajectories, attached media, unsupported
event kinds, and raw inference-stream framing are not yet replayed by the
production executor path, so H036 remains partial.

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
| Chat/admin session boundary | Outer middleware authenticates both namespaces, requires Controller role, and chat writes bind sender identity; browser stream uses the revocable HttpOnly session cookie | Generated route matrix and logout-all/revoked-WebSocket qualification (H026) |
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
| MCP (H045/F13) | Pinned stdio and Streamable HTTP fixtures; bounded responses, wrong IDs, session expiry without tool replay, matching open-SSE response, and stdio handshake/tool-call correlation | Installed reference servers and cross-platform process/session qualification |
| Graphify and Graphiti | Server/API and SPA tests | Local graph build and Graphiti health/test-call |
| Durable runs and steps | Core transition/reopen coverage; SPA and headless Controller client can explicitly resume a safe non-transport run from saved input/checkpoints | Automatic runner dispatch recovery, routine/child parity, and process-kill matrix (H023; F06-F08) |
| Streaming protocol | SSE/NDJSON byte framing; multilingual SSE/tool payload every-split fixture; typed incomplete-stream failure carries visible partial output; no model checkpoint/tool call before terminal marker | Process-kill framing, provider-specific/tool-delta split fixtures, and MCP response-before-EOF qualification (H029) |
| Inference admission | Shared global/per-model permits, foreground reserve, bounded queue, background aging, workload labels, and a two-child-per-parent scope; durable parent/child token/time/retry/effect quotas and resource-capped runner admission are implemented | Run the restart/cancellation budget suite and measure queue delay on each supported hardware tier (H030) |
| Context budget (H031) | Every in-process, container-runner, and direct-streaming model call sizes the serialized request, tools, and output reserve against the qualified context ceiling; both tool paths have scoped artifact offload | Held-out Unicode/JSON/vision/tool-round suite against exact model/template identities; token-estimate calibration and artifact lifecycle/replay (H031/F12) |
| Task completion contracts | Core acceptance/artifact/delivery verifier tests; SPA, CLI, and editor adapter submit required/optional criteria, artifacts, and delivery requirements | Other task producers, restart binding, broad real-task coverage, and deterministic production verifier wiring (H022) |
| Skill evaluation and rollback | Disposable workspace/mock tool execution, exact expected files and call order, forbidden output/actions, per-case output/token limits, identical parent/candidate cases, and current-suite promotion hash checks | Complete live-model held-out qualification and record governed promotion evidence (H039) |
| Compaction provenance (H032) | Receipt fingerprint/reopen coverage and Controller source-event endpoint | HMAC-verified paged original evidence, stale receipt invalidation, trust-retention attacks, and multi-compaction acceptance-criteria quality suite |
| Chat request idempotency and effects | Core reopen/replay/stale-outcome/conflicting-body tests; dispatcher safety declarations; unknown outcomes park until a Controller resolves them with evidence | Production transport sink integration, broad sink reconciliation, and process-kill qualification across real adapters (H024) |
| Workspace checkpoints and forks (H041) | Core checkpoint/apply receipt reopen tests; server isolated-run, conflict preview, owned-change restore, hardlink/path-denial, and interrupted-replace recovery tests | Installed process-kill matrix; run symlink-escape test with Windows symlink privilege enabled |
| Child-agent run tree (H042) | Child join replay, killed inference recovery, nested-parent cancellation, fresh fork lineage, and a Criterion durable child-reservation benchmark | Stable runner/release timing and supported-hardware delegation qualification |
| Durable turn controls (H043) | SQLite transition/idempotency tests; queued-event recovery without duplicate append; authenticated stop under 500 ms while local inference is held open | Direct in-process steer/pause/resume and installed runner delivered-but-unacknowledged control reconnect |
| Terminal/editor contract (H044) | Contract schema compatibility, older-client minimum-version handling, editor command allowlist mapping, and same-key chat replay/body conflict | Installed SPA-to-client handoff and WebAuthn-assisted client login qualification |
| Tool contracts and failures | Core, plugin-host, MCP, runner-local tests | Container-runner typed-failure parity after protocol migration |
| Local endpoint policy | Policy crate adversarial tests and server adapter tests | Exercise each configured LAN/VPN endpoint and inspect persisted resolution |
| Public and plugin egress (H025) | Existing URL guards and local endpoint policy coverage | Prove mixed DNS answers, IPv4-mapped IPv6, redirect-to-private, proxy bypass, approved private integrations, and registered-sidecar pinning issue zero prohibited requests |
| Memory assertions/jobs | Core evidence/trust/reopen tests and skills capture tests | Exercise the startup-wired extraction worker through durable completion and restart |
| Memory evidence review | Controller can inspect assertion revisions, validity, status, hash-verified source spans, and review history; CLI export requires consent/redaction; chat and agent traces show metadata-only governed loadout receipts; unreceipted legacy HOT prompt injection is disabled | Agent-run restart and runtime ancestor/projection invalidation qualification (H037) |
| Trust-first memory retrieval | Chat, in-process, runner, agent, and research prompts apply scope/trust/lifecycle/time/mode eligibility before ranking, exclude TOOL_ONLY assets, deduplicate source hashes, and record query/source/rank receipts; local embeddings/rebuild are versioned; `eval-harness qualify-memory-retrieval` records held-out lexical/hybrid recall, answer accuracy, leakage, index-build time, and retrieval p50/p95 | Live local-backend held-out pass and fixed-budget supported-hardware qualification; current report records `http_connect` because no inference service is listening (H038) |
| Workspace coding (H040) | Controller-registered roots and isolated run checkouts; manifest host tools for bounded read/search/SHA-checked patch/terminal/LSP jobs; durable execution receipts; pinned toolchain image; direct Docker smoke passed offline Cargo test, denied-network and read-only mount checks, and Rust Analyzer diagnostics | Execute the Rust-side LSP late-push regression and server/route tests; run the held-out multi-file model-repair benchmark; complete H023-H029 write-authority prerequisites |
| Artifact provenance | Core/host/container tests plus packaging checks | Verify detached release bundles through bundled install on every platform |
| Plugin upgrade transaction and panel authority (H027) | Manifest/path and ZIP bounds, rollback and provenance checks; browser test verifies panel credential isolation and denied unrelated RPC routes | Platform release qualification and lifecycle/process-kill rollback matrix |
| Voice | Pipeline/server/SPA tests | Runtime locking and cancellation are per-session, but the production path remains push-to-talk with non-streaming STT/TTS and an echo callback; H047 endpointing, queue limits, sentence streaming, real agent integration, and hardware qualification remain |
| SPA and accessibility contracts | Vitest and TypeScript | Browser smoke at desktop/mobile widths |
| Deployment and release artifacts | Linux/macOS/Windows build scripts select SQLCipher; installed `doctor` performs encrypted header/wrong-key/migration/reopen plus disposable backup/restore/rekey checks; tagged releases attach Sigstore-signed offline update bundles with schema-safe rollback instructions | Execute each native workflow and review its logs; record install/recovery and actual offline upgrade/rollback results per OS (H028/F04/F16) |

For H034, authenticate the terminal client with `execlaw client login
--username <controller>` (password is entered at the local prompt), then run
`execlaw client qualify --context-tokens 4096`. The command prints the
model/template/backend identity and per-check result matrix without printing
session credentials. Repeat after selecting each deployed Standard backend;
record failed checks as failures. A passing qualification persists the profile
for the exact identity used by routing. If the running server is older than
the checked-out CLI, `execlaw qualify-model --context-tokens 4096` uses the
local database and the same qualification checks without restarting it.
Pass `--no-encrypt` only for a plaintext development database.
Plain inference success and `/v1/models` reachability do not qualify tool
calls. The Standard backend's `model_spec_json` must include the exact
`quantization`, `chat_template`, and `backend_version`; an Ollama backend also
needs `binary_hint: "ollama"` to use its native tool and thinking protocol.
Choose a context budget the deployed Ollama instance actually accepts and
require the context check to pass. See
[`truenas-docker.md`](truenas-docker.md#qualify-ollama-before-using-chat-tools)
for the deployed-model procedure and the `model_identity_incomplete` alert.
Passing text and tool checks alone does not authorize tool turns when context
calibration fails; inspect the full matrix before calling a model qualified.

For a local memory export, run
`execlaw memory export-assertion <ID> --to <FILE> --redaction-map <map.json> --consent`.
The CLI verifies each referenced event and quote hash, applies the reviewed
replacement map plus built-in pattern redaction, and refuses to overwrite an
existing file. The JSON remains a private local artifact; the command does not
send it to a service.

## H049 stable-hardware performance gate

Criterion regression comparisons run in `.github/workflows/performance-bench.yml`
on a dedicated Linux x64 self-hosted runner labeled `execlaw-bench`. The runner
must remain the same physical/virtual host across comparisons and have no
deployment or signing credentials. Fork pull requests are skipped because the
workflow executes repository code on this runner.

The gate records current Criterion estimates as a workflow artifact and caches
the last passing default-branch baseline by runner name. It compares replay,
trust-filtered memory search, catalog assembly, runner framing, contended
inference queue wait, and 4 MiB artifact publishing. A case fails only when its
current 95% confidence interval clears the baseline interval plus the
case-specific noise tolerance. A new case is recorded as unbaselined in its
first change and joins the gate after the default-branch run stores it.

If no baseline exists, the initial PR measurement is uploaded as an explicit
bootstrap report but is not called a regression comparison; the default-branch
run then saves the persistent baseline, and following PRs are gated. After a
hardware or Rust toolchain change, review and
dispatch the workflow with `replace_baseline` enabled on the default branch.
Benchmark results from different runner/compiler fingerprints are rejected.
`python3 -m unittest discover -s scripts/tests -p 'test_performance_gate.py'`
exercises a deliberate regression failure, an in-tolerance pass, and rejection
of a different CPU identity. These policy tests do not replace a measured
baseline from the stable runner.
Prefix reuse, model residency, batching, and speculative decoding remain
separate trials; an optimization must preserve task-quality and policy results
before adoption.

## Test layers

### Fast local feedback

Use the smallest check that covers the current edit, then run the full required
suite before opening a pull request:

```bash
# Compile one Rust crate without linking or running its tests.
cargo check -p <crate>

# Run one matching test in a crate. Add --lib for unit tests only.
cargo test -p <crate> <test_filter>
cargo test -p <crate> --lib <test_filter>

# SPA edit loop.
npm --prefix web test -- <test_filter>
```

Keep the target directory, profile, and feature selection consistent across
repeated runs. Cargo keeps separate artifacts for different targets, profiles,
and feature sets; switching between a package-only test, the workspace suite,
and SQLCipher can trigger another compile. A first focused run can therefore be
much slower than a later run of the same command. A warm no-op `cargo build`
measures cache startup, not the cost of recompiling a changed crate.

On Windows, the PowerShell dev-server, full-test, and migration-test scripts use
a workspace-specific Cargo target under `%LOCALAPPDATA%` by default. This keeps
frequently rewritten compiler artifacts outside a OneDrive-synced checkout and
seeds the local cache from the existing `target/` directory on first use. The
initial copy may take several minutes for a large target tree. An explicit
`CARGO_TARGET_DIR` is respected; set `EXECLAW_REPO_CARGO_TARGET=1` to keep
artifacts in the repository. For direct Cargo commands, initialize the same
target in the current PowerShell session:

```powershell
. .\scripts\cargo-target-local.ps1
Use-ExeclawLocalCargoTarget -RepoRoot (Get-Location).Path
cargo test -p execlaw-server --lib <test_filter>
```

To locate build time, run `cargo build --workspace --timings`; Cargo saves an
HTML report under `target/cargo-timings/`. For comparisons, use the same source
revision and command, keep other Cargo jobs idle, and record cold compilation
separately from warm reruns and test execution. Do not remove the target
directory between paired runs.

The full workspace suite remains the regression gate. Focused checks shorten
edit feedback but do not replace it:

```bash
cargo test --workspace --no-fail-fast
```

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

For migration-only validation, use the repository helper:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\test-migrations.ps1
```

PowerShell 7 can use `pwsh -File scripts/test-migrations.ps1` instead. If
PowerShell reports that `powershell.exe` or `pwsh` is not recognized, the
launcher is missing from the current process `PATH`; the test script has not
run yet. On Windows, verify the inbox launcher with
`Get-Command powershell.exe`, or invoke
`$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe` directly.

It invokes `cargo test -p execlaw-core --lib migrations`. The `--lib` flag is
intentional: without it, Cargo also runs helper binaries and integration-test
targets in the package. Those targets may correctly report `0 tests` (or
`1 filtered out`) even when all migration tests pass, which can make the
focused result look misleading.

On Windows, Application Control may block execution of locally built Rust test
binaries. Cargo reports OS error 4551 when this happens; the affected test
executable never started, so this is a blocked check rather than a test
assertion failure or a pass. Application Control can also block the PowerShell
harness itself; a `PSSecurityException` is an environment limitation, not a
passing test. Run the same focused tests on an allowed Windows developer shell
or CI runner and record the result. The existence of the tests does not
substitute for executing them.

### SQLCipher

Run when storage, vault, migrations, release packaging, or backup behavior
changes:

```bash
cargo test --workspace --no-default-features -F execlaw/sqlcipher
```

On Windows this requires the production OpenSSL build prerequisites. The test
harness does not silently fall back to plaintext SQLite.
The CLI feature is selected explicitly so `doctor`, backup, restore, and
rotation compile and run under the same feature set as the packaged binary.
From Windows PowerShell, run:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/test-sqlcipher-windows.ps1
```

This imports the installed Visual Studio C++ build environment, runs the
suite with a repo-local Cargo target, then executes the native SQLCipher CLI's
`doctor` preflight.
Install a complete Windows Perl distribution first, or pass `-PerlExe` with a
portable Strawberry Perl path. Git for Windows' Perl lacks an OpenSSL build
module and is insufficient. WSL is not required for this suite.

The desktop build scripts select the CLI `sqlcipher` feature explicitly, and
the installed `execlaw doctor` preflight checks encrypted-header, wrong-key,
migration/reopen, disposable backup/restore, rekey, and post-rotation recovery.
Tagged bundle workflows emit detached Sigstore provenance and offline
update/rollback archives. H028 still requires running every packaged artifact
and recording the backup/restore, rotation, signing, and rollback results.
A successful plaintext workspace suite cannot substitute for this tier. F16
records the remaining CI/evidence gaps.

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
