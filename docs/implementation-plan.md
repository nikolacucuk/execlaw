# Implementation plan: all 130 enhancements

Accepted scope: **H001-H130**, 2026-09-27. The operator intends to implement
all 130 items. This is the delivery ledger, not a request to implement every
item in a single change. No item may be silently dropped, marked complete
for lack of time, or described as shipped solely because it appears here.

The [Nexus visual extension](#nexus-visual-extension) separately tracks NX01-NX30 and NXF01-NXF11. These additional chat-appearance proposals do not renumber the core H001-H130 baseline or alter its status totals.

## Sources of truth

- [Harness roadmap](llm-harness-roadmap.md): requirements, acceptance criteria,
  priority/effort estimates, detailed dependency intent, and F01-F19 findings.
- This plan: delivery status, ownership, sequencing, and evidence for every H ID.
- [Immediate queue](remaining-improvements-todo.md): the next bounded slices
  and outstanding validation, linked back to this ledger.
- [Architecture](architecture.md), [agent model](agent-model.md), and subsystem
  docs: design contracts and implementation detail. Their historical phase
  checklists do not override this plan's current status.
- [Documentation index](README.md): navigation and document-maintenance rules.

Stable IDs never change when an item moves between releases. Use H IDs in
implementation descriptions and test reports; link the corresponding
`enhancement-NNN` anchor. Use F IDs for fixes and link their independent
closure evidence. One change may advance several items without completing
them all. Do not duplicate live status tables in component READMEs.

## Baseline and status rules

This baseline reconciles the source review; it is not a fresh execution of
the Rust, SPA, production SQLCipher, crash, or model-quality suites. Existing
implementation reports are retained as evidence pointers, not newly certified
results. No item is release-qualified by this documentation change.

| Status | Items | Meaning |
|---|---:|---|
| Planned | 74 | Accepted items without delivery evidence attached yet |
| Implemented; revalidate | 30 | Implementation is recorded; current acceptance and supported-platform evidence remains to be executed |
| Partial | 25 | H022-H025, H036-H050, H100, H103, H105, and H126-H128 have implemented slices with open acceptance gates; see the individual rows below |
| Reopened | 0 | No reopened enhancement items |
| Verification blocked | 1 | H020 requires production SQLCipher qualification and reconciliation of F04/F16 |
| Qualified | 0 | All applicable acceptance, security, and supported-platform evidence passes |
| **Total** | **130** | Every accepted requirement is represented below |

Set an owner before changing Planned to In progress. Use Partial when a slice
lands without the entire acceptance contract. Use Verification blocked when
an identified external/toolchain gate prevents qualification; document the
blocker and next action. Implemented means code exists, not that an unrun
check passed. Qualified requires all gates below. Reopen an item when evidence
invalidates its guarantee; preserve the old evidence and explain the change.

**2026-09-29 verification update:** the focused results and remaining gates
for H027-H030, H033-H043 are recorded in
[`remaining-improvements-todo.md`](remaining-improvements-todo.md#verification-pass-outstanding-local-checks-2026-09-29).
The full core library suite passed (709 tests), the server library passed
(1,155 passed, 5 ignored), and runner-local passed (23/23 on the workspace
rerun). The full workspace run was not green: seven server integration targets
failed with HTTP 401 from missing auth in test setup, four script integration
targets were blocked by Windows Application Control, and a separate Signal
plugin run had seven decoder failures. The full SPA suite had three timeouts;
lint and workspace-wide formatting checks also remain blocked by dirty-tree
issues. Live-model, hardware, packaged-release, and unfinished-feature gates
remain open; no item is marked Qualified by this pass.

**2026-09-30 agent workstream update:** the typed outcome, event identity,
draft review, trigger preview, scoped read/search, calendar schedule,
ownership/takeover, and no-effect agent evaluation slices are implemented.
After Windows Code Integrity event 3077 initially blocked three generated test
executables, a fresh `cargo test --workspace --quiet` rerun exited 0; core
passed 726/726 and server passed 1,182 with five ignored. The standalone SPA
suite passed 537/537 and lint passed. The implementation/test checklist in
[`remaining-improvements-todo.md`](remaining-improvements-todo.md#implementation-and-test-checklist)
tracks these checks separately from open installed TrueNAS, recovery,
audience-change, concurrency, DST, and held-out local-model acceptance gates.
H022/H023/H038/H100/H103/H105/H126-H128 remain Partial under their broader
contracts.

Roadmap checkboxes are the implementation shorthand: checked for Implemented
or Qualified, unchecked for Planned, In progress, Partial, Reopened, or
Verification blocked. Update them with the ledger rather than treating them
as an independent status system. H005, H009, H011, and H018 have completed
their requested implementation and focused acceptance checks. H020 remains
unchecked and verification-blocked.

Lab items H084/H085 remain accepted work. Implement the specified trial and
evaluate its acceptance gate before production activation. A failed trial
requires redesign or an explicit recorded operator decision to change scope;
it is not an automatic exemption from the 130-item plan.

## Workstreams and delivery order

Workstreams assign coordination scope, not a strict serial schedule. The
roadmap's explicit prerequisites still apply; parent initiatives and related
items are not automatically hard blockers. Assign one owner per item and
record any additional dependencies before implementation begins.

| Stream | Scope | Items | Primary code and documentation owners |
|---|---|---:|---|
| S1 | Safety and recovery | 34 | Policy, server dispatch/auth, outbox, release/testing docs |
| S2 | Runtime and operations | 28 | Core storage, supervisors, container manager, CLI and platform docs |
| S3 | Inference and context | 13 | Inference API, model adapters, context window, Ollama/agent docs |
| S4 | Coding and orchestration | 19 | Runner, workspace plugins, run APIs, SPA and client contracts |
| S5 | Knowledge and deliverables | 18 | Memory/skills, attachments, research/Python, memory and evaluation docs |
| S6 | Plugins, communication, and workflows | 18 | Plugin SDK/host, transport, automation/voice, plugin/operator docs |

Start with the [immediate queue](remaining-improvements-todo.md): prevent
unauthorized effects and unsafe installation/test execution, qualify shipped
encryption, then close logging and recovery gaps. Establish H021's benchmark
and acceptance evidence in parallel with those fixes. H107-H111 verification
work accompanies implementation rather than waiting for an end-stage audit.

Next deliver inference/context, coding, knowledge, and automation as complete
user journeys. Bring operational and cross-platform checks into each release.
Keep platform expansion, external adapters, and experimental runtimes behind
their stated dependencies. All workstreams remain in scope; sequence does
not imply deferral or removal. No dates or individual assignees are invented.

## Item ledger

Each title links to the exact requirement and acceptance criteria. Unassigned
means no implementation owner has been designated. Pending means no current
qualification artifact has been attached; follow the linked roadmap for
existing source/test references. Update this table rather than adding a
parallel list with its own status.

| ID | Requirement / acceptance | Stream | Status | Owner | Evidence / next gate |
|---|---|---|---|---|---|
| H001 | [Select native Ollama for an operator-managed endpoint](llm-harness-roadmap.md#enhancement-001) | S3 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H002 | [Add backend protocol conformance probes](llm-harness-roadmap.md#enhancement-002) | S3 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H003 | [Reject an unavailable model explicitly in production turns](llm-harness-roadmap.md#enhancement-003) | S3 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H004 | [Capture backend readiness and version transitions](llm-harness-roadmap.md#enhancement-004) | S2 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H005 | [Bound inference retries by error class and deadline](llm-harness-roadmap.md#enhancement-005) | S1 | Implemented; revalidate | Unassigned | Deadline/error-class retry policy and cancellation now cover non-streaming runner calls plus chat and incognito streaming open/backoff; cancellation and retry regressions cover the API paths |
| H006 | [Prove tool-call schemas against each supported model adapter](llm-harness-roadmap.md#enhancement-006) | S3 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H007 | [Make tool catalog budgets observable](llm-harness-roadmap.md#enhancement-007) | S3 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H008 | [Complete the two-pass untrusted-content boundary](llm-harness-roadmap.md#enhancement-008) | S3 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H009 | [Run a process-kill recovery matrix for every tool boundary](llm-harness-roadmap.md#enhancement-009) | S1 | Implemented; revalidate | Unassigned | Automation lease reclaim, graph checkpoint resume, runner model/tool checkpoint recovery, paired event commit replay, safe refusal to redispatch unfenced in-flight effects, and idempotent sink receipt reuse are covered by process-restart/kill tests |
| H010 | [Bind approvals to a specific pending effect and replay state](llm-harness-roadmap.md#enhancement-010) | S1 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H011 | [Provide an auditable transport delivery timeline](llm-harness-roadmap.md#enhancement-011) | S1 | Implemented; revalidate | Unassigned | Review and outbox events project onto the originating chat message; SPA displays each state; a killed relay after sink acceptance reuses the same idempotency key and receipt after restart |
| H012 | [Index conversation search without weakening trust scope](llm-harness-roadmap.md#enhancement-012) | S5 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H013 | [Finish governed memory-asset loadouts](llm-harness-roadmap.md#enhancement-013) | S5 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H014 | [Close the memory lifecycle review loop](llm-harness-roadmap.md#enhancement-014) | S5 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H015 | [Evaluate skill capture against held-out local tasks](llm-harness-roadmap.md#enhancement-015) | S5 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H016 | [Version prompts, model settings, and tool catalogs per turn](llm-harness-roadmap.md#enhancement-016) | S5 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H017 | [Add offline adversarial evaluation suites](llm-harness-roadmap.md#enhancement-017) | S1 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H018 | [Attribute latency and context cost by phase](llm-harness-roadmap.md#enhancement-018) | S1 | Implemented; revalidate | Unassigned | Broader inference, runner, MCP/WS transport, automation, and skill-capture log review removed content-bearing fields; numeric phase metrics and context estimates remain bounded |
| H019 | [Validate resource-aware model routing](llm-harness-roadmap.md#enhancement-019) | S2 | Implemented; revalidate | Unassigned | Historical implementation in linked requirement; revalidate |
| H020 | [Drill backup, key rotation, and secret incident recovery](llm-harness-roadmap.md#enhancement-020) | S1 | Verification blocked | Unassigned | [F04](llm-harness-roadmap.md#finding-f04), [F16](llm-harness-roadmap.md#finding-f16); vault/OAuth tests, WSL SQLCipher core drill, CLI doctor and disposable recovery flow passed; full SQLCipher workspace run interrupted by Wsl/Service/E_UNEXPECTED; native installed-package runner results pending |
| H021 | [Build a reproducible real-task harness benchmark](llm-harness-roadmap.md#enhancement-021) | S1 | Implemented; revalidate | Unassigned | `crates/eval-harness/src/benchmark.rs`; offline release/periodic suites; live local-model and hardware-tier qualification pending |
| H022 | [Make task completion an explicit, verifiable contract](llm-harness-roadmap.md#enhancement-022) | S4 | Partial | Unassigned | Chat, headless, editor, scheduled-agent, and routine producers persist contracts. Core now checks produced artifact scope, expiry, and file hash at write and report time; manual passes and delivery confirmations require a matching Controller audit attestation or a scoped delivered outbox event. The earlier fabricated attachment now reads Blocked and HTTP resubmission returns 400. A real TrueNAS-model chat demonstrated Incomplete, Partial, VerifiedComplete, missing-artifact Blocked, and unconfirmed-delivery Incomplete. Plugin/research artifact producer linkage and real-task acceptance across headless/editor/routine/agent paths remain |
| H023 | [Unify production executor semantics and recovery](llm-harness-roadmap.md#enhancement-023) | S1 | Partial | Unassigned | A killed isolated Windows executable resumed held inference on the original run with one user event and one model reply; completed child joins replay output and interrupted agents/routines retain durable identities. A focused real child-process kill during inference reclaimed the model attempt once and published one artifact. Native Ollama carries explicit context and thinking controls across host/runner protocol; the 16K effective TrueNAS profile passed. An isolated exhausted run repeatedly retried startup recovery after inference timeout and wall-clock budget expiry. Fix that loop, then prove streaming/routine/runner parity, parent tool-pair process behavior, and the full kill matrix. Runner-enabled isolation needs a separate Docker daemon or installation-scoped orphan sweep |
| H024 | [Add client request idempotency and explicit effect reconciliation](llm-harness-roadmap.md#enhancement-024) | S1 | Partial | Unassigned | Scoped request reservations, conflict detection, durable response replay, audited unknown-outcome decisions, and a leased text/attachment relay are implemented. Same-key retry after an isolated inference kill returned the original run. A copied installed Windows debug executable was killed after a disposable subprocess transport accepted a send but before ack; after restart the outbox was Unknown, attempts stayed 1, and the sink still had one effect. Lease reclaim now records `lease_reclaimed` instead of a second `send_requested`. Release-installed relay/transport and idempotent real-sink process-kill qualification remain |
| H025 | [Enforce capability-specific network egress at connection time](llm-harness-roadmap.md#enhancement-025) | S1 | Partial | Unassigned | Policy clients pin approved addresses and restrict redirects/proxies. The live TrueNAS inference endpoint passed exact-model text, streaming, tool, structured-output, and context qualification through a scoped `192.168.1.76/32` approval; revoking it returned 503, and a private-integration-only grant did not authorize inference. A sidecar HTTP redirect regression proves the destination listener receives zero requests. Existing endpoint-policy and OAuth/plugin fixtures pass; full public/research/automation/MCP/OAuth/sidecar cross-adapter DNS, proxy, redirect, and supported-endpoint matrix remains |
| H026 | [Make authorization and session revocation systematic](llm-harness-roadmap.md#enhancement-026) | S1 | Implemented; revalidate | Unassigned | OpenAPI-derived route/role matrix maps protected operations and explicit public auth/meta exceptions; central middleware gates admin, chat, and WebSocket paths for active Controller sessions; anonymous, Viewer, and Operator denial covers plugin lifecycle/settings/media/WebSocket surfaces; plugin state remains unchanged on denied lifecycle calls. Password changes and logout-all revoke persisted sessions, access-token checks reject immediately, active WebSockets close after revocation, and approval cards show principal, scope, reason, requested effect steps, and editable Limited topic scope. Focused server/core and SPA tests pass; full release-matrix qualification remains |
| H027 | [Make plugin upgrades transactional and panel authority explicit](llm-harness-roadmap.md#enhancement-027) | S1 | Implemented; revalidate | Unassigned | Plugin versions and panel entries are constrained to safe paths; ZIP staging limits entries and expanded size; canonical stage containment, isolated candidate directories, rollback after candidate failure, and provenance-before-replacement are enforced. UI panels run in opaque-origin frames with manifest-scoped RPC and no operator token in the frame; focused browser and same-version rejected-upgrade regressions verify credential isolation, denied unrelated routes, and preservation of installed bytes. Cross-platform release qualification remains separate |
| H028 | [Qualify the actual production release artifacts](llm-harness-roadmap.md#enhancement-028) | S1 | Implemented; revalidate | Unassigned | The standard CLI doctor backup/restore round-trip test passed (1); the production SQLCipher feature build stopped before application compilation because Git for Windows Perl lacks `Locale::Maketext::Simple`. Linux/macOS release runners and installed-package doctor/rekey flows remain |
| H029 | [Treat streaming as a tested protocol state machine](llm-harness-roadmap.md#enhancement-029) | S1 | Implemented; revalidate | Unassigned | Inference API (36), runner binary (13), runner-local (23), and runner protocol (10) tests passed; focused host process-kill recovery passed (1). Full process-kill stream matrix remains |
| H030 | [Schedule local inference with fair hierarchical budgets](llm-harness-roadmap.md#enhancement-030) | S2 | Implemented; revalidate | Unassigned | Core tests (709), runner-local tests (23), and inference admission/resolver tests (18) passed, including child token reservation/reopen and cancellation cleanup. Hardware-tier queue-delay and time/retry/effect restart qualification remain |
| H031 | [Compile context against the full budget on every model round](llm-harness-roadmap.md#enhancement-031) | S3 | Implemented; revalidate | Unassigned | All executor paths serialize and budget every request, reserve output, calibrate bytes/token from qualified local usage (3,000 milli fallback), preserve whole turns, and offload large tool results to bounded run-scoped artifacts |
| H032 | [Give compaction a provenance and quality contract](llm-harness-roadmap.md#enhancement-032) | S3 | Implemented; revalidate | Unassigned | All history paths persist source-range/fingerprint receipts; summaries use an untrusted role, strict schema, two corrections, waiting approvals, completion criteria, artifacts, and delivery state; stale sources invalidate; Controller evidence paging verifies HMAC when configured |
| H033 | [Add on-demand tool discovery and progressive schema loading](llm-harness-roadmap.md#enhancement-033) | S3 | Implemented; revalidate | Unassigned | Both executors search only the run-pinned authorized catalog, load exact schemas on selection, hash the complete catalog in the run manifest, and recheck dispatch policy |
| H034 | [Qualify model-specific capability profiles and structured outputs](llm-harness-roadmap.md#enhancement-034) | S3 | Implemented; revalidate | Unassigned | Controller qualification probes text, streaming, tools, three JSON schemas, usage-measured context, and red/blue vision; exact model/quantization/template/backend/parser identities gate routing and native JSON schema |
| H035 | [Provide a durable execution inspector with reconnectable traces](llm-harness-roadmap.md#enhancement-035) | S2 | Implemented; revalidate | Unassigned | Controller run API and Settings inspector expose cursor-reconnectable events, snapshots, steps, retries, approvals, children, queue wait, run-bounded delivery, stale-cursor reload, and local metadata-only export |
| H036 | [Turn failures into consented, replayable regression fixtures](llm-harness-roadmap.md#enhancement-036) | S2 | Partial | Unassigned | Full core library tests passed (709); offline `eval-harness replay-fixture` accepted the synthetic fixture with effects disabled and incident/release references intact. Production executor replay and fix-to-release regression gating remain |
| H037 | [Make memory evidence inspectable and correctable](llm-harness-roadmap.md#enhancement-037) | S5 | Partial | Unassigned | Core and Controller API expose assertion status, scope/trust, observed/valid windows, supersession lineage, review history, and hash-verified source spans; Controller can atomically retract or replace assertions. `execlaw memory export-assertion` produces a consented local redacted export. Chat prompt assembly persists metadata-only per-turn HOT-loadout receipts, and the run inspector shows scope, trust, binding, source version/hash, injected length, and selection reasons; a retraction regression verifies a later turn omits the deleted asset. Agent-run history and broader runtime invalidation qualification remain |
| H038 | [Qualify trust-first hybrid and temporal retrieval end to end](llm-harness-roadmap.md#enhancement-038) | S5 | Partial | Unassigned | `MemoryAssetStore::search_eligible` now filters scope, trust, lifecycle, owner visibility, time, and allowed injection modes before lexical/vector ranking; bounded eligibility fails closed and stale vector rows are ignored. Chat, in-process, and runner prompts consume only HOT/DISCOVERABLE eligible assets, deduplicate by source hash, and record query fingerprints, ranks, and injected source metadata in the run trace; TOOL_ONLY assets stay out of prompt context. Versioned local embedding/reranking and rebuild, research/agent consumers, held-out recall/answer-quality qualification, and hardware latency gates remain |
| H039 | [Evaluate learned skills by execution and support safe rollback](llm-harness-roadmap.md#enhancement-039) | S5 | Partial | Unassigned | Controller rollback creates a new monotonic trial version from selected immutable content and copies its resources; stable skills return to trial and must pass the current suite again. Evaluation still invokes a tools-disabled model completion with substring checks; isolated workspaces/mock integrations, behavioral and forbidden-action assertions, resource budgets, and comparative held-out promotion qualification remain |
| H040 | [Ship a complete workspace coding plugin](llm-harness-roadmap.md#enhancement-040) | S4 | Partial | Unassigned | Controller-registered roots now support bounded read/search, secret/link/hardlink rejection, and isolated checkpoint creation. Manifest-declared model tools, safe patching, confined terminal jobs/LSP diagnostics, durable dispatch, adversarial qualification, and H023-H029 write-authority prerequisites remain |
| H041 | [Add workspace checkpoints, run forks, and safe diff application](llm-harness-roadmap.md#enhancement-041) | S4 | Partial | Unassigned | Content-addressed snapshots, isolated per-run copies, conflict-aware previews, idempotent per-file apply and owned-change restore receipts with rechecked hashes, resumable per-file restore after restart, and fresh linked forks without copied approvals/steps/effect keys are implemented; no checkpoint/restore tests were found or run, and process-kill/external-edit concurrency qualification remains |
| H042 | [Make child-agent work a durable, inspectable run tree](llm-harness-roadmap.md#enhancement-042) | S4 | Partial | Unassigned | Persistent runner delegates now store typed task/trust contracts, parent-linked spawn/join checkpoints, a shared 8192-token aggregate reservation ledger, completed-sibling dependency validation, cancellation, scoped result artifacts with authenticated download links, and inspector/Agents projections; restart/cancel fixtures and delegation-overhead qualification remain |
| H043 | [Support durable steering, queued messages, and useful stop controls](llm-harness-roadmap.md#enhancement-043) | S4 | Partial | Unassigned | Existing stop-turn tests passed (2); the turn-control store/API has no focused tests for idempotency conflicts, queue application, or delivery recovery. Non-runner steering/pause, process-kill/duplicate-delivery fixtures, delivered-but-unacknowledged reconciliation, and latency qualification remain |
| H044 | [Expose a stable headless, terminal, and editor client contract](llm-harness-roadmap.md#enhancement-044) | S4 | Partial | Unassigned | A version-1 `/api/client-contract`, refreshable terminal client, and allowlisted LSP execute-command adapter cover idempotent chat, cursors, artifacts, approvals, controls, workspace checkpoints/diffs, and cancellation; WebAuthn login plus older-client/SPA compatibility qualification remain |
| H045 | [Complete negotiated, bounded MCP interoperability](llm-harness-roadmap.md#enhancement-045) | S6 | Partial | Unassigned | Both transports share pinned 2025-06-18 negotiation; sessions reinitialize after expiry without replaying tool effects; response IDs, JSON-RPC shape, frames/bodies/pages, capability gates, and explicit cancellation are enforced. Pinned restart/session-expiry/oversize/wrong-ID/SSE fixtures and production qualification remain |
| H046 | [Make research and browser results evidence-verifiable](llm-harness-roadmap.md#enhancement-046) | S6 | Partial | Unassigned | Fetched source notes now retain bounded text snapshots, fetch time, normalized-URL source IDs, body hashes, and truncation state; generated Markdown links to URLs not fetched in this run are unlinked and labeled unverified. Claim-level entailment, stale/contradictory evidence handling, browser-action integration, and held-out qualification remain |
| H047 | [Deliver cancellable, low-latency local voice sessions](llm-harness-roadmap.md#enhancement-047) | S6 | Partial | Unassigned | Runtime map locking is now isolated per session; agent callbacks receive a cancellation token, interruption fires the TTS epoch and playback event before client cleanup, and synthesis checks cancellation per outbound chunk. Push-to-talk, non-streaming Whisper/Kokoro, the current echo callback, endpointing/queue limits, and supported-hardware qualification remain |
| H048 | [Make privacy retention and deletion cover every projection](llm-harness-roadmap.md#enhancement-048) | S5 | Partial | Unassigned | Research jobs use persistent tombstones, payload scrubbing, read/claim hiding, and a retry queue for report/workspace/Graphify projections; governed asset deletion atomically removes bindings, embeddings, wiki/code graph rows, and both FTS projections, with a tombstone that blocks recreation. Skills, exports/diagnostics, plugin storage, memory assertion/event-evidence descendants, backup restore reapplication, and full seeded-projection qualification remain |
| H049 | [Enforce performance budgets and benchmark local optimizations](llm-harness-roadmap.md#enhancement-049) | S2 | Partial | Unassigned | Added a stable `execlaw-bench` self-hosted Criterion gate with stored per-run estimates, same-runner/compiler fingerprint checks, 95% confidence-interval comparison plus per-case noise tolerances, and benchmark coverage for replay, trust-filtered memory search, catalog assembly, runner framing, contended inference queue wait, and 4 MiB artifact publishing. A qualified runner baseline and optimization trials with identical task-quality/policy checks remain |
| H050 | [Make first success and recovery accessible and diagnosable](llm-harness-roadmap.md#enhancement-050) | S2 | Partial | Unassigned | Added a Controller-only scrubbed support-bundle endpoint and Backends download/summary UI covering database encryption mode, schema migration count, hardware capacity estimate, backend readiness/profile counts, plugin/tool-policy authority counts, and recovery queue statuses. The capacity class is a heuristic, not measured model performance; disposable-backend browser journeys, accessibility checks, and per-OS first-task measurements remain pending |
| H051 | [Carry typed sensitivity and provenance through the entire run](llm-harness-roadmap.md#enhancement-051) | S1 | Planned | Unassigned | Pending |
| H052 | [Recheck live authority immediately before dispatch](llm-harness-roadmap.md#enhancement-052) | S1 | Planned | Unassigned | Pending |
| H053 | [Render approvals from canonical typed actions](llm-harness-roadmap.md#enhancement-053) | S1 | Planned | Unassigned | Pending |
| H054 | [Broker secrets without placing credentials in model context](llm-harness-roadmap.md#enhancement-054) | S1 | Planned | Unassigned | Pending |
| H055 | [Check outbound data at the actual delivery boundary](llm-harness-roadmap.md#enhancement-055) | S1 | Planned | Unassigned | Pending |
| H056 | [Offer task-scoped safety profiles with visible enforcement](llm-harness-roadmap.md#enhancement-056) | S1 | Planned | Unassigned | Pending |
| H057 | [Simulate policy changes before enabling them](llm-harness-roadmap.md#enhancement-057) | S1 | Planned | Unassigned | Pending |
| H058 | [Define and test power-loss durability separately from crash recovery](llm-harness-roadmap.md#enhancement-058) | S1 | Planned | Unassigned | Pending |
| H059 | [Move blocking database work behind a bounded execution service](llm-harness-roadmap.md#enhancement-059) | S2 | Planned | Unassigned | Pending |
| H060 | [Manage disk pressure and WAL growth as first-class health states](llm-harness-roadmap.md#enhancement-060) | S2 | Planned | Unassigned | Pending |
| H061 | [Version event payloads and replay transformations explicitly](llm-harness-roadmap.md#enhancement-061) | S2 | Planned | Unassigned | Pending |
| H062 | [Rebuild derived projections safely while the service runs](llm-harness-roadmap.md#enhancement-062) | S2 | Planned | Unassigned | Pending |
| H063 | [Detect rollback of an otherwise valid database snapshot](llm-harness-roadmap.md#enhancement-063) | S2 | Planned | Unassigned | Pending |
| H064 | [Test schema evolution across supported release histories](llm-harness-roadmap.md#enhancement-064) | S2 | Planned | Unassigned | Pending |
| H065 | [Describe tool effects and concurrency semantics in manifests](llm-harness-roadmap.md#enhancement-065) | S1 | Planned | Unassigned | Pending |
| H066 | [Support prepare/preview/execute with resource preconditions](llm-harness-roadmap.md#enhancement-066) | S1 | Planned | Unassigned | Pending |
| H067 | [Model compensating actions for partially completed workflows](llm-harness-roadmap.md#enhancement-067) | S6 | Planned | Unassigned | Pending |
| H068 | [Parallelize only independent tool work](llm-harness-roadmap.md#enhancement-068) | S4 | Planned | Unassigned | Pending |
| H069 | [Specify timer behavior across clock changes and restart](llm-harness-roadmap.md#enhancement-069) | S2 | Planned | Unassigned | Pending |
| H070 | [Give routines explicit missed-run and overlap policies](llm-harness-roadmap.md#enhancement-070) | S6 | Planned | Unassigned | Pending |
| H071 | [Provide safe dead-letter inspection and controlled redrive](llm-harness-roadmap.md#enhancement-071) | S2 | Planned | Unassigned | Pending |
| H072 | [Enforce distinct least-privilege runtime profiles](llm-harness-roadmap.md#enhancement-072) | S2 | Planned | Unassigned | Pending |
| H073 | [Qualify isolation on every supported operating system](llm-harness-roadmap.md#enhancement-073) | S2 | Planned | Unassigned | Pending |
| H074 | [Budget processes, disk, descriptors, and output as well as memory](llm-harness-roadmap.md#enhancement-074) | S2 | Planned | Unassigned | Pending |
| H075 | [Drain safely during shutdown, update, and host suspend](llm-harness-roadmap.md#enhancement-075) | S2 | Planned | Unassigned | Pending |
| H076 | [Reduce secret lifetime and document host-compromise limits](llm-harness-roadmap.md#enhancement-076) | S2 | Planned | Unassigned | Pending |
| H077 | [Give artifacts transactional references and safe garbage collection](llm-harness-roadmap.md#enhancement-077) | S2 | Planned | Unassigned | Pending |
| H078 | [Qualify useful hardware tiers beyond the primary GPU path](llm-harness-roadmap.md#enhancement-078) | S3 | Planned | Unassigned | Pending |
| H079 | [Ship a plugin author conformance kit](llm-harness-roadmap.md#enhancement-079) | S6 | Planned | Unassigned | Pending |
| H080 | [Negotiate plugin API compatibility explicitly](llm-harness-roadmap.md#enhancement-080) | S6 | Planned | Unassigned | Pending |
| H081 | [Pin executable tool versions for in-flight runs](llm-harness-roadmap.md#enhancement-081) | S6 | Planned | Unassigned | Pending |
| H082 | [Support publisher revocation and offline compromise response](llm-harness-roadmap.md#enhancement-082) | S2 | Planned | Unassigned | Pending |
| H083 | [Make plugin hook ordering, failure, and reentrancy predictable](llm-harness-roadmap.md#enhancement-083) | S6 | Planned | Unassigned | Pending |
| H084 | [Trial a restricted WebAssembly plugin tier](llm-harness-roadmap.md#enhancement-084) | S6 | Planned | Unassigned | Pending |
| H085 | [Trial delegation between explicitly paired operator-owned hosts](llm-harness-roadmap.md#enhancement-085) | S4 | Planned | Unassigned | Pending |
| H086 | [Make instruction precedence inspectable and resistant to injection](llm-harness-roadmap.md#enhancement-086) | S3 | Planned | Unassigned | Pending |
| H087 | [Detect dependency cycles and capacity deadlocks](llm-harness-roadmap.md#enhancement-087) | S4 | Planned | Unassigned | Pending |
| H088 | [Cache qualified read results with authorization and freshness checks](llm-harness-roadmap.md#enhancement-088) | S3 | Planned | Unassigned | Pending |
| H089 | [Resolve entities without silently merging identities](llm-harness-roadmap.md#enhancement-089) | S5 | Planned | Unassigned | Pending |
| H090 | [Separate explicit preferences from inferred personalization](llm-harness-roadmap.md#enhancement-090) | S5 | Planned | Unassigned | Pending |
| H091 | [Ingest documents with page, cell, and region evidence](llm-harness-roadmap.md#enhancement-091) | S5 | Planned | Unassigned | Pending |
| H092 | [Maintain a revision-aware local code and documentation index](llm-harness-roadmap.md#enhancement-092) | S4 | Planned | Unassigned | Pending |
| H093 | [Apply patches with explicit file preconditions and transactions](llm-harness-roadmap.md#enhancement-093) | S4 | Planned | Unassigned | Pending |
| H094 | [Use structured command specifications and platform-aware execution](llm-harness-roadmap.md#enhancement-094) | S4 | Planned | Unassigned | Pending |
| H095 | [Manage development servers as owned run resources](llm-harness-roadmap.md#enhancement-095) | S4 | Planned | Unassigned | Pending |
| H096 | [Make test and build evidence independently verifiable](llm-harness-roadmap.md#enhancement-096) | S4 | Planned | Unassigned | Pending |
| H097 | [Treat dependency installation as an explicit execution boundary](llm-harness-roadmap.md#enhancement-097) | S4 | Planned | Unassigned | Pending |
| H098 | [Validate application migrations against disposable local databases](llm-harness-roadmap.md#enhancement-098) | S4 | Planned | Unassigned | Pending |
| H099 | [Verify UI changes in an isolated browser with attributable evidence](llm-harness-roadmap.md#enhancement-099) | S4 | Planned | Unassigned | Pending |
| H100 | [Bind sensitive sends to recipient and audience identities](llm-harness-roadmap.md#enhancement-100) | S1 | Partial | Unassigned | Agent drafts now snapshot channel, recipient, group ID/title, source event, and revision; Controller send uses the stored proposal and exact recipient. Live group-membership change detection and broader sensitive-effect audience qualification remain |
| H101 | [Normalize message edits, deletion, reactions, and reply lineage](llm-harness-roadmap.md#enhancement-101) | S6 | Planned | Unassigned | Pending |
| H102 | [Make cross-channel continuity an explicit identity operation](llm-harness-roadmap.md#enhancement-102) | S6 | Planned | Unassigned | Pending |
| H103 | [Coalesce trigger bursts without losing event meaning](llm-harness-roadmap.md#enhancement-103) | S6 | Partial | Unassigned | Specialist mailbox admission deduplicates stable `(agent, source, event ID)` identities, preserves source time/version, and processes one event per run. Configurable debounce/burst batching, correction supersession, and restart fixtures remain |
| H104 | [Qualify webhook replay resistance and credential rollover](llm-harness-roadmap.md#enhancement-104) | S1 | Planned | Unassigned | Pending |
| H105 | [Support an explicit operator takeover and hand-back state](llm-harness-roadmap.md#enhancement-105) | S6 | Partial | Unassigned | Per-recipient Controller/agent ownership and generation-checked hand-back are durable; automatic outbox admission and dispatch check the generation. In-flight sink races, reconnect UX, and intervening-action reconciliation require qualification |
| H106 | [Preserve multimodal grounding through transformations](llm-harness-roadmap.md#enhancement-106) | S5 | Planned | Unassigned | Pending |
| H107 | [Model-check the critical state transitions](llm-harness-roadmap.md#enhancement-107) | S1 | Planned | Unassigned | Pending |
| H108 | [Fuzz full protocol conversations and untrusted parsers](llm-harness-roadmap.md#enhancement-108) | S1 | Planned | Unassigned | Pending |
| H109 | [Mutation-test the enforcement tests themselves](llm-harness-roadmap.md#enhancement-109) | S1 | Planned | Unassigned | Pending |
| H110 | [Prove local-only operation with denied-network integration tests](llm-harness-roadmap.md#enhancement-110) | S1 | Planned | Unassigned | Pending |
| H111 | [Protect held-out tasks and calibrate local judges](llm-harness-roadmap.md#enhancement-111) | S1 | Planned | Unassigned | Pending |
| H112 | [Measure when to clarify, proceed conservatively, or abstain](llm-harness-roadmap.md#enhancement-112) | S4 | Planned | Unassigned | Pending |
| H113 | [Detect non-progress and bound strategy changes](llm-harness-roadmap.md#enhancement-113) | S4 | Planned | Unassigned | Pending |
| H114 | [Make release builds reproducible and dependency inventories actionable](llm-harness-roadmap.md#enhancement-114) | S2 | Planned | Unassigned | Pending |
| H115 | [Export standard telemetry through a replaceable local adapter](llm-harness-roadmap.md#enhancement-115) | S2 | Planned | Unassigned | Pending |
| H116 | [Provide a complete air-gapped installation and update path](llm-harness-roadmap.md#enhancement-116) | S2 | Planned | Unassigned | Pending |
| H117 | [Separate host-service authority from desktop and tool processes](llm-harness-roadmap.md#enhancement-117) | S1 | Planned | Unassigned | Pending |
| H118 | [Rehearse recovery after loss of the entire machine](llm-harness-roadmap.md#enhancement-118) | S2 | Planned | Unassigned | Pending |
| H119 | [Version configuration changes and preview their operational impact](llm-harness-roadmap.md#enhancement-119) | S2 | Planned | Unassigned | Pending |
| H120 | [Generate capability and support documentation from verified evidence](llm-harness-roadmap.md#enhancement-120) | S2 | Planned | Unassigned | Pending |
| H121 | [Support resumable, quota-governed attachment uploads](llm-harness-roadmap.md#enhancement-121) | S5 | Planned | Unassigned | Pending |
| H122 | [Provide a deliverable library with previews and non-code revisions](llm-harness-roadmap.md#enhancement-122) | S5 | Planned | Unassigned | Pending |
| H123 | [Package reproducible data-analysis runs](llm-harness-roadmap.md#enhancement-123) | S5 | Planned | Unassigned | Pending |
| H124 | [Pin automation runs to immutable published workflow revisions](llm-harness-roadmap.md#enhancement-124) | S6 | Planned | Unassigned | Pending |
| H125 | [Finish durable approval, wait, composition, and join nodes](llm-harness-roadmap.md#enhancement-125) | S6 | Planned | Unassigned | Pending |
| H126 | [Make automation scenarios effect-free and support shadow evaluation](llm-harness-roadmap.md#enhancement-126) | S1 | Partial | Unassigned | Agent trigger preview accepts captured/synthetic events without effects; optional explicitly requested local-model preview has no tools and bounded event/token/runtime budgets. Automation-graph simulation still needs isolated mock/effect-recording backends and shadow comparison |
| H127 | [Give reply drafts a freshness-aware review inbox](llm-harness-roadmap.md#enhancement-127) | S6 | Partial | Unassigned | Source-linked, revisioned drafts now have recipient snapshots, stale-on-new-inbound checks, Controller edit/reject/approval, and a chat/Agents review UI. Group-membership change checks, retention, concurrent browser review, and installed-binary send/recovery proof remain |
| H128 | [Manage operator attention with quiet hours and actionable digests](llm-harness-roadmap.md#enhancement-128) | S6 | Partial | Unassigned | Timed agents accept IANA-zone cron, overlap, catch-up, and quiet-hours policy with durable fire receipts. Alert aggregation, snooze/acknowledgment, escalation, and cross-channel digest behavior remain planned |
| H129 | [Qualify multilingual behavior beyond translated UI strings](llm-harness-roadmap.md#enhancement-129) | S5 | Planned | Unassigned | Pending |
| H130 | [Offer incrementally synchronized local knowledge collections](llm-harness-roadmap.md#enhancement-130) | S5 | Planned | Unassigned | Pending |

## Finding closure ledger

F01-F19 are source-review findings, distinct from enhancement completion.
All remain Open in this baseline. Severity/preconditions and remediation
tests live in the linked finding; no exploit or production test is implied.
A fix may satisfy part of an enhancement while its broader acceptance work
remains open. Fill the owner and closure evidence when work is assigned.

| Finding | Related implementation IDs | Status | Owner | Closure evidence |
|---|---|---|---|---|
| [F01](llm-harness-roadmap.md#finding-f01) | [H026](llm-harness-roadmap.md#enhancement-026), [H027](llm-harness-roadmap.md#enhancement-027) | Closed | Unassigned | H026 middleware blocks anonymous, Viewer, and Operator plugin lifecycle requests through build_router; denied install requests leave plugin DB state unchanged |
| [F02](llm-harness-roadmap.md#finding-f02) | [H027](llm-harness-roadmap.md#enhancement-027), [H079](llm-harness-roadmap.md#enhancement-079) | Open | Unassigned | Pending |
| [F03](llm-harness-roadmap.md#finding-f03) | [H025](llm-harness-roadmap.md#enhancement-025), [H110](llm-harness-roadmap.md#enhancement-110) | Open | Unassigned | Pending |
| [F04](llm-harness-roadmap.md#finding-f04) | [H020](llm-harness-roadmap.md#enhancement-020), [H028](llm-harness-roadmap.md#enhancement-028), [H114](llm-harness-roadmap.md#enhancement-114), [H118](llm-harness-roadmap.md#enhancement-118) | Open | Unassigned | Pending |
| [F05](llm-harness-roadmap.md#finding-f05) | [H026](llm-harness-roadmap.md#enhancement-026), [H052](llm-harness-roadmap.md#enhancement-052) | Closed | Unassigned | Password change invalidates access and refresh credentials, persisted revocation survives SQLite reopen, and live WebSocket authority is removed by the session recheck |
| [F06](llm-harness-roadmap.md#finding-f06) | [H009](llm-harness-roadmap.md#enhancement-009), [H023](llm-harness-roadmap.md#enhancement-023), [H024](llm-harness-roadmap.md#enhancement-024), [H071](llm-harness-roadmap.md#enhancement-071) | Open | Unassigned | Pending |
| [F07](llm-harness-roadmap.md#finding-f07) | [H009](llm-harness-roadmap.md#enhancement-009), [H023](llm-harness-roadmap.md#enhancement-023) | Open | Unassigned | Pending |
| [F08](llm-harness-roadmap.md#finding-f08) | [H009](llm-harness-roadmap.md#enhancement-009), [H023](llm-harness-roadmap.md#enhancement-023), [H024](llm-harness-roadmap.md#enhancement-024) | Open | Unassigned | Pending |
| [F09](llm-harness-roadmap.md#finding-f09) | [H018](llm-harness-roadmap.md#enhancement-018), [H035](llm-harness-roadmap.md#enhancement-035), [H036](llm-harness-roadmap.md#enhancement-036), [H048](llm-harness-roadmap.md#enhancement-048) | Open | Unassigned | Pending |
| [F10](llm-harness-roadmap.md#finding-f10) | [H005](llm-harness-roadmap.md#enhancement-005), [H029](llm-harness-roadmap.md#enhancement-029) | Open | Unassigned | Pending |
| [F11](llm-harness-roadmap.md#finding-f11) | [H023](llm-harness-roadmap.md#enhancement-023), [H043](llm-harness-roadmap.md#enhancement-043) | Open | Unassigned | Pending |
| [F12](llm-harness-roadmap.md#finding-f12) | [H031](llm-harness-roadmap.md#enhancement-031), [H032](llm-harness-roadmap.md#enhancement-032) | Open | Unassigned | Pending |
| [F13](llm-harness-roadmap.md#finding-f13) | [H029](llm-harness-roadmap.md#enhancement-029), [H045](llm-harness-roadmap.md#enhancement-045) | Open | Unassigned | Pending |
| [F14](llm-harness-roadmap.md#finding-f14) | [H027](llm-harness-roadmap.md#enhancement-027), [H054](llm-harness-roadmap.md#enhancement-054), [H072](llm-harness-roadmap.md#enhancement-072) | Open | Unassigned | Pending |
| [F15](llm-harness-roadmap.md#finding-f15) | [H047](llm-harness-roadmap.md#enhancement-047) | Open | Unassigned | Pending |
| [F16](llm-harness-roadmap.md#finding-f16) | [H021](llm-harness-roadmap.md#enhancement-021), [H023](llm-harness-roadmap.md#enhancement-023), [H028](llm-harness-roadmap.md#enhancement-028), [H039](llm-harness-roadmap.md#enhancement-039), [H049](llm-harness-roadmap.md#enhancement-049), [H050](llm-harness-roadmap.md#enhancement-050), [H120](llm-harness-roadmap.md#enhancement-120) | Open | Unassigned | Pending |
| [F17](llm-harness-roadmap.md#finding-f17) | [H009](llm-harness-roadmap.md#enhancement-009), [H058](llm-harness-roadmap.md#enhancement-058) | Open | Unassigned | Pending |
| [F18](llm-harness-roadmap.md#finding-f18) | [H066](llm-harness-roadmap.md#enhancement-066), [H126](llm-harness-roadmap.md#enhancement-126) | Open | Unassigned | Pending |
| [F19](llm-harness-roadmap.md#finding-f19) | [H026](llm-harness-roadmap.md#enhancement-026), [H052](llm-harness-roadmap.md#enhancement-052), [H126](llm-harness-roadmap.md#enhancement-126) | Open | Unassigned | Pending |

## Qualification and evidence required for completion

Before marking an item Qualified, attach a durable evidence record containing:

- H/F IDs, implementation revision or source hashes, owner, and review date.
- The exact acceptance checks, fixture versions, commands, outputs, and
  pass/fail/skip results; explain every required check not run.
- Relevant runtime paths and supported OS/backend/model/hardware identities.
  An in-process fixture does not prove runner-container or packaged behavior.
- Security and failure-path coverage; effectful work includes denied access,
  cancellation, restart, idempotency/reconciliation, and resource limits.
- Performance/quality baselines where required, including failures and
  uncertainties; do not claim a speedup or comparative win without results.
- Migration/upgrade/rollback and operator documentation affected by the change.
- Remaining limitations and related open findings; no contradictory release
  claim may remain hidden behind a checked implementation box.

Store reports alongside the relevant tests or in a committed documentation
artifact, linking sanitized results here. Do not put prompts, credentials,
private operator data, or live approval tokens into evidence files.

Run the relevant tests and required repository checks for implementation
changes, including the workspace suite before an explicitly authorized commit.
Production crypto changes require the SQLCipher path; frontend changes require
SPA checks; performance changes require their benchmark gates. This plan does
not authorize commits, production deployment, live effects, or destructive
operator-state changes by itself.

## Update procedure

1. Select a bounded item/slice, assign an owner, and check its prerequisites
   and open F findings. Record scope before starting.
2. Implement and test against the linked acceptance criteria. Keep unrelated
   work and operator state intact; follow the repository architecture.
3. Attach evidence; update the ledger status and roadmap checkbox together.
   For partial work, state the exact remainder rather than closing the item.
4. Update the subsystem docs and READMEs affected by actual behavior. Keep
   future scope labelled planned and historical evidence dated.
5. Reconcile the immediate queue and status totals. Close an F finding only
   with its own regression evidence. Never renumber IDs to hide unfinished work.

## Change record

- 2026-09-27: accepted all H001-H130, created the full ledger and F01-F19
  closure map, reconciled H009/H018/H020, and linked subsystem documentation.
  Documentation validation only; no implementation or runtime qualification
  was performed in this update.

## Nexus visual extension

Added 2026-09-27. The operator requested a focused Nexus design review and
backlog. The [Nexus roadmap section](llm-harness-roadmap.md#nexus-visual-roadmap)
owns the design, settings/off contract, acceptance criteria, and NXF01-NXF11
findings. These 30 proposals are separately tracked as Planned; this update
does not implement the visual redesign or close a UI/security finding.
The core ledger above still covers exactly H001-H130.

Retain Settings -> General -> Chat appearance -> Classic / Nexus as the
master selector. Enabling a lens, map, or richer preview must be optional;
disabling Nexus must restore the complete Classic view without discarding
messages, annotations, drafts, or delivery state. Fix NX01/NX28 first and keep
NX30 verification alongside every slice. Related H work is shared, not duplicated.

| ID | Requirement | Priority / effort | Status | Owner | Related core work |
|---|---|---|---|---|---|
| NX01 | [Reliable master toggle and complete Classic restoration](llm-harness-roadmap.md#nexus-nx01) | P0 / M | Planned | Unassigned | H050 |
| NX02 | [A cohesive theme and user-controlled visual preferences](llm-harness-roadmap.md#nexus-nx02) | P1 / M | Planned | Unassigned | H050/H119 |
| NX03 | [Distinct author, channel, account, and destination identities](llm-harness-roadmap.md#nexus-nx03) | P1 / M | Planned | Unassigned | H100/H102 |
| NX04 | [Compact adaptive toolbar with progressive disclosure](llm-harness-roadmap.md#nexus-nx04) | P1 / M | Planned | Unassigned | H050 |
| NX05 | [Source lenses with explicit filter versus highlight behavior](llm-harness-roadmap.md#nexus-nx05) | P1 / M | Planned | Unassigned | H012/H129 |
| NX06 | [Readable execlaw response and turn cards](llm-harness-roadmap.md#nexus-nx06) | P1 / M | Planned | Unassigned | H022/H035 |
| NX07 | [Focused relationship connectors within the timeline](llm-harness-roadmap.md#nexus-nx07) | P1 / M | Planned | Unassigned | H035 |
| NX08 | [Optional relationship map with a readable list equivalent](llm-harness-roadmap.md#nexus-nx08) | P2 / L | Planned | Unassigned | H035/H050 |
| NX09 | [Relationship provenance and direction as a visual grammar](llm-harness-roadmap.md#nexus-nx09) | P1 / M | Planned | Unassigned | H037/H051 |
| NX10 | [Stable group and causal IDs independent of display labels](llm-harness-roadmap.md#nexus-nx10) | P1 / L | Planned | Unassigned | H016/H101 |
| NX11 | [Follow relationships into unloaded history and return](llm-harness-roadmap.md#nexus-nx11) | P1 / M | Planned | Unassigned | H012/H026 |
| NX12 | [Consistent full-history search and saved-view scope](llm-harness-roadmap.md#nexus-nx12) | P1 / M | Planned | Unassigned | H012/H038 |
| NX13 | [A source-aware thread sidebar with honest unread state](llm-harness-roadmap.md#nexus-nx13) | P1 / M | Planned | Unassigned | H043/H105 |
| NX14 | [Branch summaries that expose context and new activity](llm-harness-roadmap.md#nexus-nx14) | P1 / M | Planned | Unassigned | H042/H103 |
| NX15 | [Reading-position marker and optional activity overview](llm-harness-roadmap.md#nexus-nx15) | P2 / M | Planned | Unassigned | H035/H103 |
| NX16 | [Event-backed delivery strip with clear next actions](llm-harness-roadmap.md#nexus-nx16) | P1 / M | Planned | Unassigned | H011/H024 |
| NX17 | [Stable streaming and visible work states](llm-harness-roadmap.md#nexus-nx17) | P1 / M | Planned | Unassigned | H035/H043 |
| NX18 | [Compact, inspectable tool activity and result previews](llm-harness-roadmap.md#nexus-nx18) | P1 / M | Planned | Unassigned | H065/H096 |
| NX19 | [Evidence-linked attachment and citation cards](llm-harness-roadmap.md#nexus-nx19) | P1 / M | Planned | Unassigned | H091/H122 |
| NX20 | [One lazy selected-message inspector](llm-harness-roadmap.md#nexus-nx20) | P1 / M | Planned | Unassigned | H035/H050 |
| NX21 | [Searchable relationship editing with revision and undo](llm-harness-roadmap.md#nexus-nx21) | P1 / M | Planned | Unassigned | H037/H119 |
| NX22 | [Comfortable, Compact, and Focus reading modes](llm-harness-roadmap.md#nexus-nx22) | P1 / S | Planned | Unassigned | H050 |
| NX23 | [Restrained motion with effective off controls](llm-harness-roadmap.md#nexus-nx23) | P1 / S | Planned | Unassigned | H050 |
| NX24 | [Accessible contrast, keyboard routes, and status announcements](llm-harness-roadmap.md#nexus-nx24) | P1 / M | Planned | Unassigned | H050/H129 |
| NX25 | [Bounded rendering and scroll-anchor preservation](llm-harness-roadmap.md#nexus-nx25) | P1 / L | Planned | Unassigned | H049/H059 |
| NX26 | [Mobile-first source navigation and inspector sheets](llm-harness-roadmap.md#nexus-nx26) | P1 / M | Planned | Unassigned | H050 |
| NX27 | [Localized and bidirectional message metadata](llm-harness-roadmap.md#nexus-nx27) | P1 / M | Planned | Unassigned | H129 |
| NX28 | [Authorized reads and privacy-safe visual metadata](llm-harness-roadmap.md#nexus-nx28) | P0 / M | Planned | Unassigned | H026/H052 |
| NX29 | [A visually explicit draft-review and reply-target panel](llm-harness-roadmap.md#nexus-nx29) | P1 / M | Planned | Unassigned | H053/H100/H127 |
| NX30 | [Visual regression and usability qualification for real state combinations](llm-harness-roadmap.md#nexus-nx30) | P1 / M | Planned | Unassigned | H021/H049/H050 |

### Nexus findings and evidence

All NXF findings remain open. NXF01 was reproduced with a temporary mocked
browser probe; NXF02-NXF10 are source findings or explicitly identified
coverage limits. The later operator-authenticated live review measured layout
and observed NXF11's response-contract failure. It did not exploit the
authorization finding, send messages, or mutate organization data.

| Finding | Work to close it | Status | Evidence required |
|---|---|---|---|
| [NXF01](llm-harness-roadmap.md#nexus-finding-nxf01) | NX01/NX30 | Open | Linked finding's regression gate |
| [NXF02](llm-harness-roadmap.md#nexus-finding-nxf02) | NX05/NX12/NX30 | Open | Linked finding's regression gate |
| [NXF03](llm-harness-roadmap.md#nexus-finding-nxf03) | NX03/NX10/NX13 | Open | Linked finding's regression gate |
| [NXF04](llm-harness-roadmap.md#nexus-finding-nxf04) | NX06-NX10 | Open | Linked finding's regression gate |
| [NXF05](llm-harness-roadmap.md#nexus-finding-nxf05) | NX11/NX25 | Open | Linked finding's regression gate |
| [NXF06](llm-harness-roadmap.md#nexus-finding-nxf06) | NX22/NX24/NX30 | Open | Linked finding's regression gate |
| [NXF07](llm-harness-roadmap.md#nexus-finding-nxf07) | NX20/NX21/NX25 | Open | Linked finding's regression gate |
| [NXF08](llm-harness-roadmap.md#nexus-finding-nxf08) | NX23/NX27 | Open | Linked finding's regression gate |
| [NXF09](llm-harness-roadmap.md#nexus-finding-nxf09) | NX09/NX21/NX28 | Open | Linked finding's regression gate |
| [NXF10](llm-harness-roadmap.md#nexus-finding-nxf10) | NX28/NX30; H026 | Open | Linked finding's regression gate |
| [NXF11](llm-harness-roadmap.md#nexus-finding-nxf11) | NX12/NX20/NX21/NX28/NX30; H044 | Open | Typed JSON/error contracts and readable degraded-state regressions |

Baseline checks on 2026-09-27: the existing appearance browser fixture passed
light/dark 1440/390/320 px checks and mocked production-login routing;
`npm.cmd test -- src/__tests__/message-stream.test.tsx src/__tests__/general-page.test.tsx`
passed 62 tests. A separate collapsed-branch toggle probe still reproduced
NXF01. Passing baseline tests therefore does not qualify the new designs or
prove complete Classic restoration, authorization, accessibility, or scaling.
See the roadmap for synthetic baseline screenshots and exact limitations.

The [live deployment review](llm-harness-roadmap.md#nexus-live-review) adds six
sanitized captures from the operator's authenticated local/TrueNAS views and
a content-free measurement record. It strengthens NX04/NX20/NX22/NX25/NX26
as the first visual slice after NX01/NX28 gates: the TrueNAS sample rendered
200 rows and 40,800 organization options, with a 205 px mobile toolbar and a
266 px incoming bubble inside a 390 px viewport. The local organization
response was HTTP 200 HTML rather than JSON; TrueNAS returned the expected
JSON shape. Neither observation closes an implementation item.

The revised acceptance criteria include a proposed 96 px closed mobile toolbar
budget, at least 85% viewport width for ordinary mobile text bubbles, zero
mounted relationship choices when the inspector is closed, and clear failure
states for unsupported organization responses. Add privacy-safe synthetic
versions of these live workloads to NX30. Keep graph expansion behind the
reading, identity, and rendering foundations.

When implementation starts, assign owners, record evidence per NX item, and
update related H/F rows only where their own acceptance criteria are met.
Keep the Nexus proposals and core status totals distinct.
