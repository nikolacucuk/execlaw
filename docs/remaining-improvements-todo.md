# Immediate implementation queue

Status: 2026-09-30. The accepted scope is **all H001-H130**, tracked in
[implementation-plan.md](implementation-plan.md). This file selects the
next bounded work, not just three older improvements and not a second full
status ledger. Requirements, acceptance criteria, and F01-F19 findings live in
[llm-harness-roadmap.md](llm-harness-roadmap.md).

This queue continues to track the accepted H001-H130 scope. The completed
implementation slices below supersede the original tracking-only status where
they overlap; unrelated open work remains represented in the delivery ledger.

## Agent event and schedule workstream (2026-09-30)

The eight operator-requested enhancements are implemented as one local code
slice and remain **Partial / verification pending**. `0061_agent_workflow_contracts`
and `0062_agent_schedules` add durable identity, version, draft, ownership, and
calendar-fire state. The local core library suite passed 725/725 and the server
library suite passed 1,180 with five ignored before the final typed source-event
API refactor. After it, the core library suite passed 726/726 and the server
library suite passed 1,182 with five ignored. Mailbox
identity, definition-version, calendar-fire, specialist-routing, and child-join
focused tests passed, and `cargo check --workspace` passed. The relevant `approval_flow` and
`plugin_lifecycle` integration targets passed 11/11 each, the WhatsApp Rhai
decoder passed 9/9, and the standalone full
SPA suite passed 537/537 with TypeScript lint clean.
The WhatsApp 0.2.18 ZIP passed entry-layout, archive-integrity, version, and
SHA-256 checks. The first `cargo test --workspace` attempt stopped before
executing `execlaw-plugin-sdk --test stage_python_sandbox_dist_zip`; excluding
that crate exposed the same block at `execlaw-core --test
bench_artifact_streaming`, and `webhook_auth` also compiled but did not
execute. Windows Code Integrity event 3077 identified policy
`{0283ac0f-fff1-49ae-ada1-8a933130cad6}` and its Enterprise signing-level
requirement for all three executables. A fresh build and execution of
`stage_python_sandbox_dist_zip` subsequently passed 1/1, and a fresh
`cargo test --workspace --quiet` run exited 0, including the previously blocked
integration targets. WSL attempts did not finish with a terminal result; they
are not test evidence. There is no installed TrueNAS evidence.

### Implementation and test checklist

- [x] Implement typed agent outcomes and atomic mailbox/run completion.
- [x] Implement stable source event identity, definition snapshots, and draft
  recovery.
- [x] Implement source-linked, revisioned drafts with stale-state checks and
  Controller review/send.
- [x] Implement trigger filters, versioning, and effect-free event preview.
- [x] Implement bounded, origin-scoped archive read/search for specialists.
- [x] Implement time-zone schedules, quiet hours, overlap/catch-up policies,
  and durable fire receipts.
- [x] Implement recipient ownership, takeover, hand-back, and outbox fencing.
- [x] Implement bounded no-effect local-model evaluation for agent triggers.
- [x] Run the core and server library suites (725/725 and 1,180 passed, five
  ignored) before the final source-event API refactor; rerun core (726/726),
  server (1,182 passed, five ignored), affected focused tests, and
  `cargo check --workspace` afterward.
- [x] Run `approval_flow` and `plugin_lifecycle` (11/11 each), WhatsApp
  decoder (9/9), SPA tests (537/537), and SPA lint.
- [x] Build and verify the WhatsApp 0.2.18 plugin ZIP and SHA-256.
- [x] Rebuild and execute the previously blocked
  `stage_python_sandbox_dist_zip` test (1/1).
- [x] Pass a fresh `cargo test --workspace --quiet` run after the final
  refactor, including `bench_artifact_streaming` and `webhook_auth` (exit 0).
- [ ] Pass strict workspace Clippy; existing warnings in untouched code still
  fail `-D warnings`.
- [ ] Qualify crash/recovery, burst, audience-change, DST, concurrent review,
  and held-out local-model fixtures listed below.
- [ ] Deploy the current build and verify the authenticated TrueNAS
  agent-draft-to-WhatsApp-review/send journey.

| Improvement | Local implementation | Remaining acceptance |
|---|---|---|
| Typed outcomes and completion | `agent_contract::AgentOutcome` validates JSON or legacy Markdown; mailbox acknowledgment and run outcome commit together. | Live model output qualification and automatic artifact/delivery evidence under H022. |
| Stable event identity and recovery | Stable upstream IDs deduplicate specialist admission; definition snapshots and mailbox versions bind work to its trigger; an interrupted published draft replays from the event log. | Webhook duplicates, host kill at every commit boundary, and full H023/H103 burst coalescing. |
| Fresh draft review | `state_reply_drafts` carries source, audience, revision, owner, and stale state; only a fresh Controller-approved revision can queue a send. | Group membership changes, in-flight delivery reconciliation, retention, and concurrent live reviews (H100/H127). |
| Trigger preview and versioning | Typed group/channel/keyword filters, definition revisions, and an effect-free event preview explain each match. | Captured TrueNAS fixtures, definition edit/restart replay, and false-positive qualification. |
| Scoped capabilities | Imported `read`/`search` declarations run through a bounded local archive tool loop restricted to the originating recipient. | Model-family tool qualification, adversarial cross-scope fixtures, and broader plugin-declared read capabilities (H038). |
| Calendar schedules | IANA-zone cron, quiet hours, overlap and catch-up policies, plus durable queued/skipped fire receipts. | DST/clock-step, outage, alert digest, and installation qualification (H128). |
| Ownership and takeover | Per-recipient lead/observer selection, Controller takeover, generation-checked hand-back, and owner-fenced automatic outbox admission/dispatch. | In-flight external sink race, reconnect, and intervening-action replay (H105). |
| No-effect replay evaluation | Synthetic/captured event preview with expected matches; explicit bounded local-model mode measures typed outcomes, missing drafts, irrelevant results, and latency without tools or effects. | Published/candidate comparison, disposable automation graph execution, and held-out real-model evaluation (H126). |

Do not mark these enhancements qualified until the remaining acceptance
fixtures, installed-binary recovery checks, and authenticated TrueNAS
review/send journey pass. The TrueNAS deployment previously returned chat
history without authentication even though the checkout requires Controller;
verify that boundary after deploying the current build.

## First delivery slices

| Order | Slice | IDs and acceptance focus | Findings |
|---:|---|---|---|
| 1 | Close unauthorized administration and execution | [H026](llm-harness-roadmap.md#enhancement-026), [H052](llm-harness-roadmap.md#enhancement-052): complete-router authorization, session revocation, dispatch-time authority | F01, F05, F19 |
| 2 | Make installation and test execution safe | [H027](llm-harness-roadmap.md#enhancement-027), [H126](llm-harness-roadmap.md#enhancement-126): validate before replacement, contain paths/ZIP expansion, effect-disabled simulation | F02, F14, F18, F19 |
| 3 | Qualify production encryption and recovery tooling | [H020](llm-harness-roadmap.md#enhancement-020), [H028](llm-harness-roadmap.md#enhancement-028): SQLCipher artifacts, encrypted-header/wrong-key/restore/rotation checks | F04, F16 |
| 4 | Close network and log disclosure paths | [H018](llm-harness-roadmap.md#enhancement-018), [H025](llm-harness-roadmap.md#enhancement-025), [H035](llm-harness-roadmap.md#enhancement-035): connection-time egress and metadata-only logs | F03, F09 |
| 5 | Repair streaming and cancellation behavior | [H005](llm-harness-roadmap.md#enhancement-005), [H029](llm-harness-roadmap.md#enhancement-029), [H043](llm-harness-roadmap.md#enhancement-043), [H045](llm-harness-roadmap.md#enhancement-045): framing, terminal states, retries, waking cancellation | F10, F11, F13 |
| 6 | Prove production recovery and reconciliation | [H009](llm-harness-roadmap.md#enhancement-009), [H011](llm-harness-roadmap.md#enhancement-011), [H023](llm-harness-roadmap.md#enhancement-023), [H024](llm-harness-roadmap.md#enhancement-024), [H058](llm-harness-roadmap.md#enhancement-058): host/runner/relay recovery and separate power-loss contract | F06, F07, F08, F17 |
| 7 | Enforce per-round context and audience safety | [H031](llm-harness-roadmap.md#enhancement-031), [H032](llm-harness-roadmap.md#enhancement-032), [H100](llm-harness-roadmap.md#enhancement-100): complete request budgets and current recipients | F12; audience fixtures |
| 8 | Establish qualification evidence | [H021](llm-harness-roadmap.md#enhancement-021), [H107](llm-harness-roadmap.md#enhancement-107)-[H111](llm-harness-roadmap.md#enhancement-111), [H120](llm-harness-roadmap.md#enhancement-120): real tasks, invariant/fuzz/mutation checks, local-only tests, support records | F16 and per-fix regressions |

Regression tests accompany each fix; order 8 does not defer verification until
the end. Track voice lock remediation under H047/F15 and the remaining
findings in the [closure ledger](implementation-plan.md#finding-closure-ledger).
Advance complete user journeys in the six workstreams after their blocking
dependencies pass. Lab work remains in scope as qualified trials.

## Unresolved validation inherited from the earlier pass

## Delivered implementation slice: H038 trust-first retrieval primitive (2026-09-28)

`MemoryAssetStore::search_eligible` computes the caller's eligible asset set
before lexical or vector ranking. It applies agent binding, trust class, owner
scope, visibility, active lifecycle, creation time, and expiry checks; oversized
sets return a typed error instead of silently trimming recall. Vector ranking
also rejects embeddings whose source hash no longer matches the asset.

Chat, in-process, and runner prompt assembly now uses the trust-first path for
HOT and DISCOVERABLE assets. TOOL_ONLY assets are excluded before ranking;
retrieval results are deduplicated by source hash, injected as read-only context,
and recorded in the run's loadout receipt. Versioned local embedding/reranking
and rebuild, research/agent consumers, held-out quality, and hardware latency
measurements remain open under [H038](llm-harness-roadmap.md#enhancement-038).

## Delivered implementation slice: H039 safe skill rollback (2026-09-28)

Controller rollback copies a selected immutable skill version into a new,
monotonically numbered version, retains parent lineage and bundled resources,
and returns the skill to `trial`. Promotion therefore requires a fresh pass for
the current held-out suite. The version-history panel exposes the operation
with a confirmation and explains the re-evaluation requirement.

H039 remains partial: the evaluator still checks generated prose with required
substring matches and does not execute skills in isolated workspaces or mock
integrations. Behavioral assertions, forbidden-action checks, resource budgets,
and comparative candidate/parent qualification remain open.

## Delivered implementation slice: H041 owned-change restore (2026-09-28)

Restore is limited to the run's latest successfully applied diff. It compares
each current file hash with that receipt's proposed hash before creating a
reverse per-file receipt, restores only those recorded paths from the immutable
base checkpoint, and refuses to overwrite edits made afterward. Restore uses
the same idempotent lease and per-file journal as apply; retries reuse the
durable plan, and the checkpoint materializes into a fresh managed temporary
checkout.

H041 remains partial pending process-kill recovery and concurrent external-edit
qualification.

## Delivered implementation slice: H031-H035 (2026-09-28)

The H031-H035 implementation rows are now marked implemented in
[`implementation-plan.md`](implementation-plan.md), and their roadmap items are
checked. The per-round request compiler and run-scoped result artifacts cover
all executor paths; compaction receipts now bind validated untrusted summaries
to source ranges and mandatory run state; both tool loops support authorized
progressive discovery; model profiles are generated by backend probes and gate
context, tools, structured output, and vision; the Controller run inspector
reconnects by durable cursor and exports metadata only.

Verification performed for this implementation slice: `cargo fmt --all --check`,
`cargo check --tests` for affected Rust crates (test targets compiled, not
executed), and `npm run lint` passed.
The local model qualification flow must be run for each deployed backend before
its profile-dependent capabilities are enabled.

## Completed implementation slice: H005, H009, H011, H018 (2026-09-28)

- H005: bounded transient retries stop at the deadline, do not retry invalid
  requests, and never replay visible stream chunks. Cancellation interrupts
  both request opening and retry backoff for chat, incognito chat, and both
  runner paths. API regressions cover cancellation before headers and while
  backing off.
- H009: automation leases and run definitions survive redelivery; completed
  graph nodes and runner model/tool checkpoints replay after restart. Paired
  commits are recovered without another model turn. A tool call without a
  durable idempotency fence remains unknown and is not dispatched twice.
  Process-kill coverage exercises automation lease reclaim, runner tool
  checkpoint recovery, host recovery after paired commit, and relay restart
  after sink acceptance with stable receipt reuse.
- H011: transport review and outbox events project onto the source chat message;
  the SPA shows delivery transition, attempt, timestamp, and receipt. The
  file-backed relay process-kill test covers remote acceptance before local
  acknowledgment and proves reuse of the idempotency key and receipt.
- H018: the source review covers inference decode errors, runner frames and
  failures, MCP request/notification bodies, WebSocket close reasons, automation,
  tool failures, and skill-capture messages. Logs retain bounded metadata and
  omit content-bearing fields. Phase samples and context estimates remain
  bounded and numeric.

Focused verification (2026-09-28):

- `cargo test -p execlaw-inference-api --lib`: 32 passed, including both
  streaming and non-streaming open/backoff cancellation and deadline tests.
- `cargo test -p execlaw-runner-local --lib process_kill_after_tool_checkpoint_reopens_and_resumes_next_model`:
  passed.
- `cargo test -p execlaw-core automation_bus::tests::process_kill_after_automation_claim_recovers_after_database_reopen`:
  passed.
- `cargo test -p execlaw-core outbox::tests::conversation_projection_joins_only_transport_effect_transitions`:
  passed.
- `cargo test -p execlaw-outbox`: 14 passed, including process kill after
  sink acceptance and stable receipt reuse.
- `npm.cmd test -- src/__tests__/message-stream.test.tsx`: 49 passed.
- `rustfmt --edition 2024 --check crates/server/src/chats.rs` and
  `git diff --check`: passed.
- `cargo check -p execlaw-server --tests`: passed before the concurrent
  turn-controls work appeared. The later server test link is currently blocked
  by that work's missing `ToSchema` implementation for `TurnControlKind` in
  the untracked `crates/server/src/turn_controls_admin.rs`.
- `npm.cmd run lint` is currently blocked by separate in-progress
  `RunInspectorPage.tsx` errors (`Form` import and inferred event types).
  The runner-binary log sentinel rerun was blocked by Windows Application
  Control rejecting its generated build script. The prior focused sentinel
  run passed.

These current checkout blockers do not leave H005, H009, H011, or H018 code
items partial. Release and supported-platform qualification remain separate
from these four completed implementations.

- Re-run production SQLCipher feature tests with a complete native build
  toolchain, then test packaged binaries. The 2026-09-29 build attempt found
  Git for Windows Perl but it lacks `Locale::Maketext::Simple`. H020 remains
  verification-blocked.
- Full workspace, SQLCipher release, and supported-platform qualification remain
  tracked in the implementation ledger; they are outside this four-item
  completion slice and must not be inferred from focused regressions.

## Historical slice: metrics, resource admission, and key recovery

The previous checklist recorded implementation of phase/context metrics,
resource admission, backup/rotation tooling, documentation, focused tests, SPA
checks, and Graphify refresh. Those records remain in H018-H020's requirement
text. They do not establish current full-workspace, production SQLCipher,
packaged-artifact, or model-quality qualification. Use the implementation
ledger to attach fresh results and close the remaining gates.

## Current implementation slice: H022 completion evidence UI

The Controller run inspector creates an immutable acceptance contract and
records criterion, artifact, and required-delivery evidence through the
existing run API. It renders derived completion status and unfinished reasons.
The headless `execlaw client send` and allowlisted editor adapter can also
submit required/optional criteria, required artifacts, and delivery
requirements. Durable in-process and runner executors persist contracts before
inference; non-durable and incognito paths reject tracked tasks. Automatic
contract generation for other task producers and broad real-task
coverage remain open. Core (709), server (1,161), and focused evidence-review
UI tests passed in this verification pass. The full SPA test run was interrupted
after unrelated timeout failures under concurrent Cargo load; it has not been
requalified in an idle run.

Outbox dispatchers now declare idempotency or status-lookup guarantees and
return typed definite-failure versus ambiguous-outcome errors. Ambiguous
non-idempotent effects park in an `unknown` status rather than retrying
automatically. Controller APIs list unknown effects without payloads and allow
an audited retry authorization or delivery confirmation with evidence; the run
inspector shows the uncertainty and resolution actor. Tests cover status
reconciliation, process-kill idempotent receipt reuse, Controller-only
resolution, and retry audit. The production relay handles plugin text and attachment sends plus wakeups. Model tool attachments stage until the matching event pair commits; research PDF bridges enqueue under stable task keys. Outbox (17), attachment relay (4), core pair-release, and server (1,161) tests pass, including unknown outcomes and process-kill receipt reuse. Full production process-kill qualification remains.

H037's Memory Assets page now lists the newest assertions with status,
scope/trust, observed/valid windows, supersession, extraction run, and up to 20
event/path/hash evidence references each. Controller retraction writes a
signed conversation event and append-only review row in the same SQLite
transaction, removes projection rows for the assertion and its superseded
ancestors, and prevents reprojecting the retracted lineage. Corrective
replacement appends a new approved assertion with operator-correction evidence
and updates the projection atomically. The Controller can reveal a source span
only after the server verifies the event chain and quote hash; display is
length-bounded and identifies source text as untrusted. `execlaw memory export-assertion`
now requires consent and a local redaction map, verifies each
source event/quote, and writes a non-overwriting local file. Chat prompt
assembly persists metadata-only per-turn HOT-loadout receipts; the run inspector
shows trust/scope, binding, source version/hash, injected length, and selection
reasons. A retraction regression verifies a later turn omits the deleted asset.
Agent-run history and broader runtime invalidation qualification remain open.
Focused core/server tests and the full SPA suite passed.

H023 startup recovery inventory and the Controller run trace now report the
checkpoint-derived next transition (claim/reclaim/wait/advance/finalize) with
bounded step metadata. The inventory does not claim leases, replay outputs,
or dispatch effects. The inspector and headless Controller client can resume an
eligible non-transport run with its saved user event, selected skills, and
timezone; the chat route gates recovery on the same run id, unchanged latest
input, and checkpoint action. Startup dispatches eligible recovered chat runs,
requeues interrupted agent mailbox input, and resumes pending routine fires
under their original run id. Core and server suites passed (709 and 1,161);
the host process-kill-after-paired-commit case passed. Completed child joins now replay stored results without a second inference request (focused regression passes). Recovery during active child inference, cross-path parity, and the complete kill matrix remain open.

2026-09-29 follow-up for H022-H025: routine definitions now accept completion
contracts and freeze them per fire; optional host-checkpoint and scheduled-agent
structured-output verifiers derive criterion results without accepting manual
pass overrides. Core (713),
routine API, routine editor, and focused verifier tests pass. Startup retries
only inherited chat runs after inference leases expire; a killed child process
proved lease fencing and interrupted-attempt recovery. A second relay kill
fixture proved that a no-guarantee send parks as unknown without redispatch.
Endpoint policy tests (11) and plugin-HTTP/OAuth adversarial fixtures pass; the
plugin fixture exposed and closed approved-DNS loopback access. Automatic
artifact/delivery proof, cross-path parity, installed-binary process-kill
qualification, and full supported-endpoint adversarial qualification remain.
The latest affected-crate runs passed: core 713, server 1,165 (five ignored),
runner-local 25, outbox 19, inference-api 36, endpoint policy 11, and SPA 532;
`cargo check --workspace`, `cargo test --workspace`, TypeScript lint, and
touched-file formatting passed. `scripts/test-adversarial.ps1` also passed its
endpoint, OAuth, plugin, guard, and runner fixtures. The first workspace test attempt was stopped
when free disk fell to about 3 GB; a later full retry completed successfully.
The SQLCipher migration regression began compiling in WSL but the service
terminated with `Wsl/Service/E_UNEXPECTED` before test execution; native
Windows has no Perl in this shell. The
plaintext workspace pass does not qualify the encrypted production path.
The strict Clippy attempt stopped in `execlaw-core` on existing warnings
(including `too_many_arguments` and `type_complexity`); the touched endpoint
policy crate passed `clippy -D warnings`. No strict workspace Clippy pass is
recorded.

2026-09-30 H022-H025 qualification follow-up: Docker's two transport sidecars
are healthy, but the configured local model endpoint on `127.0.0.1:30068` is
not listening. An isolated Windows `execlaw.exe` on port 3032 with a local
OpenAI-compatible fixture proved an active inference process kill resumes the
original durable run on attempt 2 with exactly one user event and one model
reply. A fresh kill also proved a retry with the same caller idempotency key
returns the original run and event sequence, without a second turn. The
in-process tool path now binds its request key to the durable run before
dispatch, and unknown requests are reconciled again when the caller retries
after lease recovery. This is debug-binary and fixture evidence; the full
cross-path/installed-production kill matrix remains open. A Controller API
probe showed that `attachment:nonexistent-report` can still be manually marked
present and produce `verified_complete`; H022 needs scoped artifact proof and
real-task acceptance before it can close. The adversarial script passed its
11 endpoint-policy cases and guarded HTTP/OAuth/plugin fixtures; H025 still
needs live supported-endpoint and cross-adapter DNS/proxy qualification. The
logged-in SPA and real local-model gates were unavailable in this session.
The Windows server library retest passed 1,171 tests with five ignored after
the transport turn-context prose was shortened to its pinned size budget.
Strict server Clippy remains blocked by existing warnings in dependency crates
(`manual_contains` in `execlaw-plugin-sdk` and argument-count/complexity warnings
in `execlaw-core`) before it reaches the server crate. Non-strict server Clippy
completed with warnings.

Later 2026-09-30 H022-H025 qualification: the earlier model outage above was
the laptop using `host.docker.internal` (which resolves to the laptop) instead
of the TrueNAS host at `192.168.1.76:30068`. With a scoped LocalInference
approval and native Ollama requests, the exact Q3_K_M model passed text,
streaming, tool, three structured-output, and 16K effective-context probes;
the native backend allocated 32K context to prove that prompt window. A real
chat contract moved through Incomplete, Partial, and VerifiedComplete under
Controller attestations; missing artifact and unconfirmed delivery remained
Blocked and Incomplete. The old fabricated `attachment:nonexistent-report`
result now reads Blocked, and resubmission returns HTTP 400. Core checks
produced-artifact scope/hash at write and report time and resolves delivery to
an outbox receipt or auditable Controller confirmation. Agent output and
manual evidence are also scoped. Other artifact producer links and real tasks
from headless/editor/routine/agent entry points remain H022 gates.

For H023/H024, the prior held-inference host kill and same-key replay remain
valid. A copied installed Windows debug executable was killed after a
disposable subprocess transport persisted one send but before ack. Its outbox
became Unknown after the 120-second lease; attempt count and sink effect count
stayed at one. Lease reclaim now has its own timeline transition. Release
artifact, runner/container, parent tool-pair, and full cross-path kill gates
remain. An isolated runner-enabled server cannot share the live Docker daemon:
its boot orphan sweep can remove another installation's runner volumes.

A focused real child-process kill during active inference now passes: recovery
marks and reclaims the interrupted model attempt once, settles one child
budget, and publishes one artifact. This is one H023 cut, not cross-path or
release qualification. Use the [H022-H025 qualification runbook](h022-h025-qualification.md)
for the remaining matrix and environment recovery steps.
The isolated server also repeatedly retried startup recovery for a run after
inference timed out and its wall-clock budget expired; resolve that loop before
closing H023.

For H025, revoking the TrueNAS LocalInference CIDR returned 503, and a
PrivateIntegration-only grant did not authorize inference. A sidecar redirect
fixture reached the origin and zero target-listener requests. Public,
research, automation, plugin, MCP, OAuth, proxy, DNS-rebinding, and supported
sidecar live matrices remain. Focused core evidence tests, inference API (38),
runner-local (25), runner protocol (10), outbox (19), and SPA (537) passed;
TypeScript lint passed. Full affected-crate/workspace and release tests still
need their final run on the settled source tree. H022-H025 remain Partial.

## Current implementation slices: H027-H030 and H036 (2026-09-29)

H028-H030 implementation code, release workflow definitions, and documentation
were updated in the implementation pass. That pass ran no Cargo commands,
tests, verification commands, or runtime flows as requested. The later
verification pass and its remaining platform/model gates are recorded below.

H027 stages bundled candidates in unique directories and verifies provenance
before lifecycle replacement. Plugin panels run in opaque-origin frames with
manifest-scoped RPC; same-version provenance rejection and browser credential/
route-isolation checks pass. Cross-platform lifecycle qualification remains.

H028's Linux, macOS, and Windows bundle workflows install their generated
package and run the installed SQLCipher binary's `doctor` check. The doctor
now exercises encrypted backup restore, disposable rekey, and pre/post-key
recovery snapshots. Each tagged platform workflow attaches a keyless Sigstore
signature/provenance bundle and packages an offline update archive; when an
earlier published installer exists it is included as the rollback binary. The
archive carries its own verifier and requires restoring the pre-update database
before reinstalling an older binary. Native runner execution and packaged
upgrade/rollback verification are deferred as requested.

H029's SSE fixture checks every byte split and byte-at-a-time delivery for
multilingual text and tool payloads. A truncated response preserves emitted
content, returns typed `IncompleteStream`, and is not replayed. Runner error
frames carry the failure class and visible partial output. The host retains
streamed deltas, commits an incomplete `model_turn`, and terminalizes the
durable run as failed after a runner disconnect. No tool dispatch or checkpoint
occurs without `[DONE]`. The focused host paired-commit process-kill recovery
test passed; the full stream process-kill matrix remains open.

H030 bounds child inference to two concurrent requests per durable parent
scope. A research/background burst leaves foreground slots available, cancelled
waiters release their reservations, and durable child token reservations
survive database reopen. Child prompt-plus-completion use is charged, including
over-budget use. New durable quotas reserve and settle parent/child time, retry,
and effect capacity; in-process and runner-stream retries debit the same
durable counter, and the host cancels a runner when a retry cannot be charged.
Delegated calls share the parent's admission scope, have a 120-second deadline,
and cannot dispatch effects. Each Docker runner receives a 1-GiB default memory
cap, and launch admission serializes starts against live RAM and reservations
read from running runner labels. Queue-delay measurement per supported hardware
tier and the requested regressions are deferred to the later verification pass.

H036 flagged exports now accept bounded incident and release identifiers; the
offline validation report preserves those references. The first local attempt
to run `eval-harness replay-fixture` was blocked by Windows Application Control
(OS error 4551). In the 2026-09-29 verification pass, the checked-in synthetic
fixture passed both the core validator tests and the offline eval-harness CLI.
Production executor replay and fix-to-release gating remain open.

## Verification pass: outstanding local checks (2026-09-29)

This pass covers explicit verification deferrals in the implementation slices
above, plus H033-H040 from the active harness roadmap request. Passing unit
tests do not close live-model, hardware, packaged-release, or missing-feature
gates. Items remain Partial where those acceptance requirements are not
implemented or cannot be qualified on this Windows host.

| Items | Verification performed | Result | Still open |
|---|---|---|---|
| H027 | `cargo test -p execlaw-server --test plugin_lifecycle`; panel sandbox/frame SPA tests | 11 Rust and 5 SPA tests passed | Cross-platform plugin lifecycle and packaged qualification |
| H028 | CLI doctor backup/restore round-trip; SQLCipher feature build | Backup/restore test passed (1); production SQLCipher build stopped before application compilation because `perl` is not installed | SQLCipher test suite, installed package doctor, backup/rekey recovery, Linux/macOS runners |
| H029 | Inference API, runner binary, runner-local, and protocol tests; server host process-kill recovery | 36 + 13 + 23 + 10 tests passed; focused host recovery passed (1) | Full process-kill stream matrix |
| H030 | Full core/server library suites and runner-local suite | 709 core, 1,155 server, and 23 runner-local tests passed; child reservation/reopen, queue fairness, and queued-child cancellation covered | Hardware-tier queue-delay benchmark; durable time/retry/effect restart matrix |
| H033 | Server `build_runner_tool_catalog` tests | 4 tests passed | No dedicated test exercised progressive search plus exact-schema activation end to end |
| H034 | Mocked inference probe, Controller authorization, and exact-identity requalification tests | 8 focused tests passed; a failed recheck now invalidates the prior profile | Run the held-out model/template/backend matrix against each deployed local backend; the active Standard endpoint is currently unavailable |
| H035 | Metadata-only reconnectable run-trace asset receipt test | 1 test passed; follow-up full SPA suite passed 531 tests | Live restart/reconnect qualification |
| H036 | `cargo test -p execlaw-core --lib`; offline `eval-harness replay-fixture` | 709 tests passed; fixture report valid, effects disabled, incident/release refs retained | Production executor replay and release-linked regression gate |
| H037-H038 | Core suite; server run-trace, governed-asset injection, and memory-admin tests; Memory Assets/Skills SPA tests | Core 709 passed; server 3 passed; SPA 7 passed | Agent-run history/invalidation; embedding rebuild, research/agent consumers, held-out recall/answer quality, hardware latency |
| H039 | `cargo test -p execlaw-skills --lib`; server `skills_admin::tests` | 157 and 9 tests passed; Skills UI was part of the 7 passing Memory Assets/Skills tests above | Evaluation still uses substring scoring; isolated task execution, forbidden-action/resource checks, comparative promotion |
| H040 | Source and test inventory reviewed | No workspace coding handler/plugin tests exist | Plugin model tools, safe patching, confined terminal/LSP dispatch, durable dispatch, and H023-H029 write-authority prerequisites |
| H041 | Source/test inventory reviewed | No workspace checkpoint/restore tests exist | Process-kill and concurrent external-edit recovery qualification |
| H043 | Existing stop-turn route tests | 2 tests passed | Durable steer/queue/pause/resume idempotency tests, delivered-but-unacknowledged recovery, and latency measurements; the new control store has no unit tests |
| H046 | Full server library suite | Source-fetch and research unit tests passed as part of the server suite | Semantic citation support, stale/contradictory evidence, browser actions, and held-out research scoring are implementation/quality gates |
| H047 | Voice-pipeline library tests; full server library suite | 41 pipeline tests and server voice unit tests passed | Continuous endpointing, production agent wiring, supported hardware, and end-to-end streaming remain incomplete |
| H048 | Full core and server library suites | Unit suites passed; deletion tombstone paths compile and existing projections run | Descendant deletion, backup restore reapplication, and full seeded projection qualification remain open |
| H049 | No stable-runner Criterion workflow or benchmark execution was available in this local verification | No baseline or optimization result produced | Self-hosted baseline, repeated hardware measurements, and quality/policy equivalence |
| H050 | Full SPA suite and focused Memory Assets/Skills/panel tests | Follow-up full SPA run passed 531 tests; TypeScript lint passed | Real-browser onboarding/recovery, keyboard/screen-reader, per-OS, and accessibility regression gates |

Additional workspace checks:

- `cargo fmt --all -- --check` failed on formatting differences across the
  already-dirty workspace. `rustfmt --edition 2024 --check
  crates/core/src/eval.rs` passed after formatting the changed validator.
- `git diff --check` reports pre-existing whitespace issues in
  `plugins/google-apps/plugin.toml` and `plugins/signal/ui/panel.js`; the
  changed `crates/core/src/eval.rs` passes its focused whitespace check.
- `npm.cmd run lint` fails on the unused `CompletionEvidenceRef` import in
  `web/src/routes/Agents.tsx`. The full SPA run had three 5-second timeouts in
  `general-page.test.tsx`, `message-stream.test.tsx`, and
  `settings-shell.test.tsx`; focused Memory Assets/Skills and panel-isolation
  tests passed.
- A targeted runner-local run first had 2 retry-classification failures. A
  later full workspace run passed all 23 runner-local tests; those failures did
  not reproduce.
- The full `cargo test --workspace --no-fail-fast` run passed the core (709),
  server library (1,155 passed, 5 ignored), inference API (36), runner-local
  (23), skills (157), and other reported unit targets. It was not green:
  `approval_flow` (11), `google_apps_e2e` (4), `script_plugin_e2e` (7),
  `skills_plugin_zip_e2e` (1), `voice_ws_round_trip` (2), `web_scraper_e2e`
  (1), and `webhook_auth` (9) received HTTP 401 due to missing Authorization
  headers in their setup requests. Four script integration test binaries were
  blocked by Windows Application Control (OS error 4551). In a separate run,
  `signal_plugin` executed and 7 of 16 tests failed with missing/null decoded
  Rhai fields; a later attempt to rerun that binary was blocked by the same
  Application Control policy. These failures require test-fixture/auth and
  plugin decoding follow-up before the workspace suite is green.
- The workspace compile initially exposed a missing `rusqlite::OptionalExtension`
  import in `crates/core/src/routines.rs`; that import was added, after which
  the full workspace compile proceeded and all 709 core tests passed.
- `python -m py_compile` could not run because Python is not installed; the
  Windows `python` command is only the Microsoft Store execution alias. The
  offline update scripts therefore still need syntax and archive-flow checks.

### Follow-up verification repair (2026-09-29)

- The seven integration binaries that previously failed at the session
  boundary now pass all 37 tests. Their fixtures mint real Controller sessions
  for private routes or pass the token returned by `/api/setup`. The
  `approval_flow` fixture uses the inbound cold-contact path, preserving the
  Controller chat route's sender-identity guard. The script ZIP fixture now
  opts into unsigned local development, and the web-scraper ZIP includes its
  declared `clip_page` schema.
- Signal frame decoding now uses only frame data; the inbound callback applies
  vault-backed settings. `cargo test -p execlaw-script --test signal_plugin`
  passed all 17 tests, including an own-account regression.
- The H034 Controller route passed a new exact-identity qualification test that
  persists a passing profile and invalidates it after a failed recheck. The
  terminal client now offers `client qualify`; `qualify-model` runs the same
  checks against a local database when an older server lacks the endpoint.
  The live database has migrations through 27 while this checkout requires 59;
  a backup was migrated through 59 without modifying the live database. The
  copied configuration's Standard endpoint is denied because
  `host.docker.internal` lacks local endpoint approval, and no inference
  container is running. No live result matrix or qualified profile was
  produced. H034 remains open for deployed-model qualification and held-out
  task success/latency comparison.
- `npm.cmd run lint` passed. The full SPA run passed 531 tests in 59 files;
  the earlier three timeouts did not recur. Real-browser gates remain open.
- `cargo fmt --all -- --check` still reports differences in other dirty-tree
  Rust files; touched Rust files were formatted individually.

H043's Controller chat panel now offers steer-at-boundary, queue-next-turn,
pause, resume, and runner cancel, while the existing stop path also creates a
durable cancel control. Runner acknowledgements update the SQLite control
lifecycle. Queued messages are appended as user events and marked applied in
that same event-log transaction; startup verifies old event chains and
reconciles accepted queue controls idempotently. Conversation-scoped request
keys are reused from session storage after a lost response and reject changed
intent. Reconciliation of delivered-but-unacknowledged runner controls and
stop/steer latency qualification remain open; duplicate-key/body-conflict
behavior needs focused verification.

## Delivered implementation slice: H046 fetched-source evidence (2026-09-28)

Successful fetch notes now retain a bounded text snapshot, retrieval time,
stable ID from the normalized final URL, response-body SHA-256, and truncation
state. Research cards and the job inspector expose that evidence as an
untrusted excerpt. Report Markdown links must match a successfully fetched URL;
unmatched links are rendered as unverified text and listed in a warning section.

H046 remains partial: semantic claim-to-source support, stale-page and
contradiction detection, browser-action qualification, and held-out research
scoring remain open.

## Delivered implementation slice: H048 research retention retry (2026-09-28)

Controller deletion and retention now create one idempotent research-job
tombstone in `state_privacy_deletion_jobs`. The job row's query, plan, notes,
and error are scrubbed, and research list/get/claim paths hide the tombstoned
resource immediately. Job creation refuses an ID present in the tombstone
table. A retry worker removes the report attachment, workspace,
and Obsidian/Graphify snapshot before atomically deleting the source row and
completing the tombstone. The Research page explains that backups may retain
older copies and shows pending retries and their last error. Attachment bytes
are removed only when no attachment or artifact row still references the path.

Governed memory asset deletion uses `state_memory_asset_deletion_tombstones`.
Within one SQLite transaction it records the actor, removes memory-asset FTS,
wiki FTS, bindings, embeddings, wiki pages, and code graph nodes/edges through
the asset cascade. The Memory Assets page requires confirmation and tells the
operator that original source files and backups remain. Replaying a delayed
asset create with the same ID is rejected.

H048 remains partial: memory assertion/event-evidence descendants, skills,
exports and diagnostics, plugin storage, backup restore reapplication, and full
projection qualification remain open.

## Delivered implementation slice: H047 per-session voice cancellation (2026-09-28)

The runtime-wide session map lock now protects only lookup/insert/removal. Each
session owns its client mutex, so STT flush/reset and TTS state no longer block
another session's ingest or interrupt. Barge-in publishes playback interruption
and fires the TTS cancellation epoch before waiting for that session's cleanup.
Agent callbacks now run in a cancellation select and receive a token they can
pass to inference/tool work; audio broadcast checks cancellation per chunk.

H047 remains partial: the current transport is push-to-talk, Whisper is
request/response, Kokoro returns a complete synthesis, and the voice route still
uses an echo callback. Continuous endpointing, bounded streaming queues,
incremental STT, sentence-streamed TTS, production agent wiring, and supported
hardware qualification remain open.

## Delivered implementation slice: H049 stable-runner Criterion gate (2026-09-28)

Added a dedicated self-hosted `execlaw-bench` workflow. It records runner, CPU,
compiler, and lockfile identity with Criterion estimates; compares current
95% confidence intervals with the last passing baseline plus per-case noise
tolerances; stores reports as run artifacts; and advances the baseline only
from a passing default-branch run. Required cases cover event replay,
trust-filtered memory search, tool-catalog assembly, runner frame decode,
contended inference admission, and 4 MiB artifact publishing.

H049 remains partial until the stable runner supplies its first baseline and
optimization trials demonstrate unchanged task quality and policy behavior.

## Delivered implementation slice: H049 stable-runner Criterion gate (2026-09-28)

Added a dedicated self-hosted `execlaw-bench` workflow. It records runner, CPU,
compiler, and lockfile identity with Criterion estimates; compares current
95% confidence intervals with the last passing baseline plus per-case noise
tolerances; stores reports as run artifacts; and advances the baseline only
from a passing default-branch run. Required cases cover event replay,
trust-filtered memory search, tool-catalog assembly, runner frame decode,
contended inference admission, and 4 MiB artifact publishing.

H049 remains partial until the stable runner supplies its first baseline and
optimization trials demonstrate unchanged task quality and policy behavior.
