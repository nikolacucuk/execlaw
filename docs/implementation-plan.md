# Implementation plan: all 154 enhancements

Accepted scope: **H001-H154**, updated 2026-10-07 (initial H001-H130 scope: 2026-09-27). The operator intends to implement
all 154 items. This is the delivery ledger, not a request to implement every
item in a single change. No item may be silently dropped, marked complete
for lack of time, or described as shipped solely because it appears here.

The [Nexus visual extension](#nexus-visual-extension) separately tracks NX01-NX30 and NXF01-NXF11. These additional chat-appearance proposals do not renumber the core H001-H154 baseline or alter its status totals.

The [Paperclip extension](#paperclip-coordination-and-interoperability), reviewed 2026-10-06, covers H131-H154 in the main item ledger for native management and optional worker/lead/executive interoperability. Existing H/NX progress and qualification remain unchanged.

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
| Planned | 56 | Accepted items without delivery evidence attached yet |
| Implemented; revalidate | 53 | Implementation is recorded; current acceptance and supported-platform evidence remains to be executed |
| Partial | 44 | Implemented slices with open acceptance gates; see the individual rows below |
| Reopened | 0 | No reopened enhancement items |
| Verification blocked | 1 | H020 requires production SQLCipher qualification and reconciliation of F04/F16 |
| Qualified | 0 | All applicable acceptance, security, and supported-platform evidence passes |
| **Total** | **154** | Every accepted requirement is represented below |

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

**2026-10-01 harness/memory/workspace update:** H036 and H037 now meet their
documented implementation acceptance checks and move from Partial to
Implemented; revalidate. H036 offline replay covers unknown event kinds,
non-image synthetic media, raw SSE framing, policy decisions, captured tool
catalogs, executor recovery, and incident/release-linked CI fixtures. H037
requires Controller-linked approved evidence for every injected memory asset,
invalidates linked assets on correction/retraction/source forget, and exposes
lineage in receipts and a local safe export. The 10 H040 workspace-coding tests
and pinned-image Docker/LSP smoke were rerun. H038 still records `http_connect`
with no hybrid or answer scores; H039 live held-out skill evaluation and H040
held-out repair qualification remain open because no local inference service is
reachable, and H040 also retains H023-H029 write-authority prerequisites.
The exit-captured CI-equivalent `cargo test --workspace --locked
--no-fail-fast` rerun completed with exit code 0. Core passed 751/751,
eval-harness passed 21/21, and server passed 1,226 with 7 ignored; all
remaining workspace unit, integration, and doc-test targets passed. The
machine-captured run record is in `target/full-workspace-final.log`.

Roadmap checkboxes are the implementation shorthand: checked for Implemented
or Qualified, unchecked for Planned, In progress, Partial, Reopened, or
Verification blocked. Update them with the ledger rather than treating them
as an independent status system. H005, H009, H011, and H018 have completed
their requested implementation and focused acceptance checks. H020 remains
unchecked and verification-blocked.

H084 and H085 now have opt-in trial implementations. Their performance,
cross-platform, transport-adapter, and measured-benefit gates remain open; the
trials are not enabled in release builds and are not production adoption.

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
| S7 | Agent work management and Paperclip interoperability | 24 | Core work/run contracts, scoped worker API, bridge plugin and external adapter |

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
| H022 | [Make task completion an explicit, verifiable contract](llm-harness-roadmap.md#enhancement-022) | S4 | Partial | Unassigned | Real TrueNAS-model API tasks reached VerifiedComplete through headless, editor-style, routine, and agent entry points; the real plugin tool and research job produced run-owned attachments verified by scope/hash and Controller review. Fabricated proof is rejected. The actual headless CLI and editor adapter processes were not exercised, and the matrix needs a settled-build rerun; see [qualification notes](h022-h025-qualification.md) |
| H023 | [Unify production executor semantics and recovery](llm-harness-roadmap.md#enhancement-023) | S1 | Partial | Unassigned | Held-inference host kill and child kill regressions passed. Expired-budget startup retry loop was fixed and observed terminalizing once on disposable restart. Runner resource names are database-scoped; a real runner turn passed. A held runner kill persisted attempt 1, but its budget expired during rebuilds before final replay could be qualified. Prompt replay and per-group spawn-gate changes still need final-binary live proof. Streaming/routine/runner parity, tool-pair effects, and full kill matrix remain; see [qualification notes](h022-h025-qualification.md) |
| H024 | [Add client request idempotency and explicit effect reconciliation](llm-harness-roadmap.md#enhancement-024) | S1 | Partial | Unassigned | Same-key retry returned the original run. An installed debug copy killed after a disposable sink accepted a send restarted to Unknown with one attempt and one sink effect. Windows Application Control 4551 blocked release compilation; the alternate Docker SQLCipher image build was stopped before completion because host RAM became scarce. No release-installed transport kill or idempotent real-sink matrix has passed; see [qualification notes](h022-h025-qualification.md) |
| H025 | [Enforce capability-specific network egress at connection time](llm-harness-roadmap.md#enhancement-025) | S1 | Partial | Unassigned | TrueNAS inference passed scoped approval/revocation; live OpenMeteo and research public fetch succeeded. MCP private approval, denial, and redirect fixture kept the forbidden listener at zero; focused public automation and Google OAuth policy checks passed. Full final-tree adversarial suite, authenticated external services/SPA, and cross-adapter DNS/rebinding/proxy matrix remain; see [qualification notes](h022-h025-qualification.md) |
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
| H036 | [Turn failures into consented, replayable regression fixtures](llm-harness-roadmap.md#enhancement-036) | S2 | Implemented; revalidate | Unassigned | Consent-based redacted export and effect-disabled CI replay cover catalog, policy, framing, unknown event kinds, synthetic image/audio/video/PDF replacements, and tool recovery through the production executor. The checked-in fixture catalog requires incident/release references. Core fixture tests (11), harness replay tests (11), and the CI `replay-fixtures` command pass; the fresh report contains three HMAC-verified fixtures with effects disabled and incident/release links. Supported-platform/release qualification remains |
| H037 | [Make memory evidence inspectable and correctable](llm-harness-roadmap.md#enhancement-037) | S5 | Implemented; revalidate | Unassigned | Controller linking requires approved scope/trust-matched assertions and HMAC-verified event-backed evidence; prompt admission fails closed for unlinked or hidden evidence. Receipts/admin views expose assertion IDs. Correction, retraction, and source forgetting archive linked assets and remove FTS projections in the same transaction. The Controller review page corrects/retracts and downloads loaded assertion/evidence records locally without inspected quote text. Core memory-assets (9), memory-assertion (8), server (5), and SPA (4) focused tests pass; TypeScript lint passes. Supported-platform and full-suite revalidation remains |
| H038 | [Qualify trust-first hybrid and temporal retrieval end to end](llm-harness-roadmap.md#enhancement-038) | S5 | Partial | Unassigned | `MemoryAssetStore::search_eligible` filters scope, trust, lifecycle, owner visibility, time, and allowed injection modes before lexical/vector ranking; bounded eligibility fails closed and stale vectors are ignored. Local embeddings are generated through the configured local endpoint, model/index identities are versioned, and the Controller can rebuild the derived index. Chat, in-process, runner, agent, and research prompts deduplicate source hashes and record retrieval receipts; TOOL_ONLY assets stay out of context. The held-out evaluator creates evidence-linked synthetic assets and records lexical/hybrid recall, answer accuracy, forbidden hits, index build time, and p50/p95 latency. Latest report: lexical recall 0.75, p95 3 ms, zero forbidden hits; hybrid/answer results unavailable (`http_connect` at 127.0.0.1:8000). Attempted native `qualify-model` against the running local DB; it has 24 applied migrations and lacks `config_local_endpoint_approvals.scope`, so the current resolver cannot reach the configured Standard backend. Do not migrate while the old server is active. Held-out pass and supported-hardware measurements remain |
| H039 | [Evaluate learned skills by execution and support safe rollback](llm-harness-roadmap.md#enhancement-039) | S5 | Partial | Unassigned | Skill evaluation executes bounded tool loops in disposable workspaces with deterministic mock integrations, exact workspace/call assertions, forbidden-action/output checks, and per-case character plus cumulative token limits. Parent and candidate versions use identical cases; promotion rechecks the current suite hash and requires every case to pass. Live-model held-out qualification remains |
| H040 | [Ship a complete workspace coding plugin](llm-harness-roadmap.md#enhancement-040) | S4 | Partial | Unassigned | Controller-registered roots support bounded read/search, isolated checkpoints, and SHA-checked patching in a durable run checkout. Manifest-declared read/search/patch/terminal/diagnostics tools use durable host dispatch and per-run execution receipts. Pinned-image containers have a read-only sanitized mount, network disabled, dropped capabilities, and CPU/memory/PID/time/output limits. The 10 workspace-coding server tests and Docker LSP sandbox smoke pass (network denied, read-only mount, seven diagnostics); the offline multi-file repair fixture has exact final-file and successful-test evidence. Held-out model repair and H023-H029 write-authority prerequisites remain |
| H041 | [Add workspace checkpoints, run forks, and safe diff application](llm-harness-roadmap.md#enhancement-041) | S4 | Implemented; revalidate | Unassigned | Content-addressed snapshots, isolated per-run copies, conflict-aware previews, idempotent apply, owned-change restore, and fresh linked forks are covered by 4 core and 6 server workspace tests. Database reopen preserves checkpoint/apply receipts; crash-state backup recovery, competing runs, external edits, hardlinks, and fresh forks pass. Installed-process-kill and privileged Windows symlink qualification remain |
| H042 | [Make child-agent work a durable, inspectable run tree](llm-harness-roadmap.md#enhancement-042) | S4 | Implemented; revalidate | Unassigned | Process-kill child inference recovery and completed-join replay tests pass; parent cancellation closes active nested descendants, typed contracts and budget reservations persist, and the inspector exposes child artifacts. Criterion `durable_spawn_reservation` measured 1.6854–1.8185 ms in the local dev profile (reservation only; excludes model inference). Installed restart and supported-runner qualification remain |
| H043 | [Support durable steering, queued messages, and useful stop controls](llm-harness-roadmap.md#enhancement-043) | S4 | Partial | Unassigned | Core request-key replay/conflict and delivery-ack tests pass; server recovery reconciles a queued event committed before its control acknowledgment without duplicating it. A held local inference request is stopped with an authenticated acknowledgment under the tested 500 ms budget. In-process steering/pause/resume and runner delivered-but-unacknowledged reconnect recovery remain |
| H044 | [Expose a stable headless, terminal, and editor client contract](llm-harness-roadmap.md#enhancement-044) | S4 | Implemented; revalidate | Unassigned | The version-1 contract validates against its pinned schema; an older terminal client accepts a compatible server and rejects a higher minimum; the LSP adapter and same-key chat replay/conflict tests pass. Installed SPA-to-client transfer and WebAuthn-assisted authentication qualification remain |
| H045 | [Complete negotiated, bounded MCP interoperability](llm-harness-roadmap.md#enhancement-045) | S6 | Implemented; revalidate | Unassigned | HTTP fixtures (7) and the complete stdio client suite (15) pass. Coverage includes pinned negotiation on both transports, session expiry without tool replay, wrong IDs, oversize bodies, and return on a still-open SSE stream. Installed endpoint and cross-platform process qualification remain |
| H046 | [Make research and browser results evidence-verifiable](llm-harness-roadmap.md#enhancement-046) | S6 | Partial | Unassigned | Fetched source notes retain bounded text snapshots, fetch time, normalized-URL source IDs, body hashes, and truncation state through persistence. Report lines require fetched source IDs and ordered, same-sentence evidence-term support with negation parity; unavailable/invented citations are flagged, stale/hash-changed evidence and mixed-negation/numeric conflicts are surfaced, and non-fetched URLs are unlinked/redacted. Web-scraper fetch/extract/clip/crawl responses now carry the same bounded evidence metadata. Semantic entailment, broader browser-action qualification, and held-out local-model scoring remain |
| H047 | [Deliver cancellable, low-latency local voice sessions](llm-harness-roadmap.md#enhancement-047) | S6 | Partial | Unassigned | Runtime map locking is isolated per session; a stalled-STT cross-session interrupt regression is added. Voice capture now pre-mints a saved conversation from Welcome and routes through authenticated chat; local TTS receives sentence chunks only after the chat route returns the committed response, preventing interim model text from announcing an external effect early. Incognito capture stays a composer draft. Cancellation reaches chat turns and TTS; inbound frames are byte-capped and Whisper buffers at most 30 seconds per utterance. Push-to-talk, non-streaming Whisper, continuous server VAD, and supported-hardware noise/reconnect qualification remain |
| H048 | [Make privacy retention and deletion cover every projection](llm-harness-roadmap.md#enhancement-048) | S5 | Partial | Research deletion, governed assets, memory evidence, and skill forgetting use persistent tombstones and hide or scrub live projections. Forced database restore now reapplies current research, memory-asset, memory-source, and skill-name tombstones to the restored snapshot; completed research rows are removed and absent forgotten skills receive a content-free tombstone shell. Exports/diagnostics, plugin storage, tombstone recovery when restoring to a new empty database, and full seeded-projection qualification remain |
| H049 | [Enforce performance budgets and benchmark local optimizations](llm-harness-roadmap.md#enhancement-049) | S2 | Partial | Unassigned | Added a stable `execlaw-bench` self-hosted Criterion gate with stored per-run estimates, same-runner/compiler fingerprint checks, 95% confidence-interval comparison plus per-case noise tolerances, and benchmark coverage for replay, trust-filtered memory search, catalog assembly, runner framing, contended inference queue wait, and 4 MiB artifact publishing. Three new gate tests prove a deliberate regression fails, a noisy in-tolerance change passes, and runner identity mismatches are rejected. A qualified runner baseline and optimization trials with identical task-quality/policy checks remain |
| H050 | [Make first success and recovery accessible and diagnosable](llm-harness-roadmap.md#enhancement-050) | S2 | Partial | Unassigned | Added a Controller-only scrubbed support-bundle endpoint and Backends download/summary UI covering database encryption mode, schema migration count, backend readiness, per-capability qualification counts, plugin/tool-policy authority counts, and recovery queue statuses. Tests verify capability summaries and the full serialized bundle omit raw model identity, prompt, token, and observation values. Hardware capacity is a heuristic, not measured model performance; disposable-backend browser journeys, accessibility checks, and per-OS first-task measurements remain pending |
| H051 | [Carry typed sensitivity and provenance through the entire run](llm-harness-roadmap.md#enhancement-051) | S1 | Implemented; revalidate | Unassigned | Added migration 0074 and append-only, content-digest-bound information-label events. Durable run inputs, memory writes/reads, history compaction summaries, skills, general artifacts, and child-result artifacts carry conservative labels and provenance; transport staging joins the current run label and delivery reloads it before checking recipient policy. Controller-only declassification records actor, destination, and scope and permits one matching export. Core, skills, and server regressions plus the workspace suite pass |
| H052 | [Recheck live authority immediately before dispatch](llm-harness-roadmap.md#enhancement-052) | S1 | Partial | Unassigned | Direct tool dispatch snapshots the append-only policy revision and rechecks it together with the persisted principal before invocation. Queued transport payloads bind principal and policy receipts, reject legacy unstamped rows, and are revalidated against current principal state and policy revision; MCP calls also fingerprint target and credential version. Revocation, policy-change, and transport regressions pass. Script HTTP sinks and remaining non-transport effect adapters still need final-boundary authority binding |
| H053 | [Render approvals from canonical typed actions](llm-harness-roadmap.md#enhancement-053) | S1 | Partial | Unassigned | Effectful chain approvals include structured operation, target, changed fields, reversibility, and scope from the saved plan; cold-contact approvals now expose typed trust/block/ignore/claim actions and their persistent scope. Secret-like values are redacted and the SPA renders every supplied action with semantic list/detail markup. Existing effect hashes bind approvals to saved actions. Manual keyboard/screen-reader review and any newly added approval kind remain |
| H054 | [Broker secrets without placing credentials in model context](llm-harness-roadmap.md#enhancement-054) | S1 | Partial | Unassigned | MCP bearer references are moved into per-server vault namespaces; legacy referenced rows migrate; missing or invalid credentials fail closed; secret versions invalidate stale clients on rotation; and configured bearer values are redacted from returned MCP text/errors. MCP scope, rotation, and redaction tests pass. General plugin/provider credential brokerage with request-method grants and active-request cancellation remains |
| H055 | [Check outbound data at the actual delivery boundary](llm-harness-roadmap.md#enhancement-055) | S1 | Partial | Unassigned | Deterministic credential patterns and host-owned destination labels are enforced at transport text/attachment delivery, MCP tool arguments, Rhai sidecar POST/PUT, and general Rhai HTTP POST/PATCH plus query-bearing GET/DELETE requests. Attachment bytes are hash-checked and scoped to the sending conversation; Controller declassification enables a single destination-scoped export. Policy, script, transport, MCP, and workspace tests pass. Browser-form sinks and false-positive/missed-detection feedback telemetry remain |
| H056 | [Offer task-scoped safety profiles with visible enforcement](llm-harness-roadmap.md#enhancement-056) | S1 | Implemented; revalidate | Unassigned | SQLite-backed inspect-only, workspace-edit, and approved-integration capability profiles are shown before chat submission and snapshotted into the immutable user event. Catalog visibility and host dispatch enforce the snapshot; imported plugin tools cannot widen it, and unavailable workspace isolation blocks that profile. Permission-matrix and recovery regressions are present; adversarial integration and supported-platform host/OS qualification remain |
| H057 | [Simulate policy changes before enabling them](llm-harness-roadmap.md#enhancement-057) | S1 | Implemented; revalidate | Unassigned | Controller-only endpoint and UI stage policy edits against append-only decision metadata plus fixtures for all six trust classes; previews report allow/deny/approval-gated changes without effects. Policy revisions and decisions are append-only; rollback creates a fresh revision and does not revive expired approval tokens. Broad historical-volume and supported-release recovery qualification remain |
| H058 | [Define and test power-loss durability separately from crash recovery](llm-harness-roadmap.md#enhancement-058) | S1 | Implemented; revalidate | Unassigned | New connections use WAL with `synchronous=FULL`; subprocess-abort recovery and injected `SQLITE_FULL` recovery tests pass. Criterion 256-byte commit: NORMAL 109.70 µs, FULL 1.0315 ms (9.4× on this Windows/OneDrive run). A real hard-reset test on disposable supported storage remains; durability assumes the OS/filesystem/device honor flushes |
| H059 | [Move blocking database work behind a bounded execution service](llm-harness-roadmap.md#enhancement-059) | S2 | Implemented; revalidate | Unassigned | Added one FIFO database worker with 16 pending slots, typed 503 backpressure, queue/service/transaction metrics, and serialized transaction-load tests. Message search uses 128-event transaction batches on the worker; support-bundle collection and memory-embedding reads/writes are also offloaded. Live streaming/approval contention and full async-call-site qualification remain |
| H060 | [Manage disk pressure and WAL growth as first-class health states](llm-harness-roadmap.md#enhancement-060) | S2 | Implemented; revalidate | Unassigned | Support diagnostics and Backends UI show DB/WAL/SHM/journal/reference-blob sizes, free space, queue/transaction metrics, and checkpoint progress. Warning/critical reserves are 2 GiB/512 MiB; a backlog of 1,000 uncheckpointed frames also warns. Embedding rebuilds and HF model downloads pause below the warning reserve or above a 1 GiB WAL. Controller passive checkpoint, long-reader progress, injected SQLite-full recovery, and retained-row checks pass. Pressure response does not delete content; existing retention sweepers remain the content-cleanup path. Supported-volume and managed-container download qualification remain |
| H061 | [Version event payloads and replay transformations explicitly](llm-harness-roadmap.md#enhancement-061) | S2 | Partial | Unassigned | New payloads carry a schema identity inside HMAC-covered bytes; legacy payloads remain readable and unknown versions fail clearly. Focused replay and tamper tests added. Deterministic transformations for every historical event generation and a documented reader-version matrix remain |
| H062 | [Rebuild derived projections safely while the service runs](llm-harness-roadmap.md#enhancement-062) | S2 | Partial | Unassigned | Added resumable generation checkpoints and validation-gated atomic activation. Search/archive/memory/graph rebuild workers, live-write and tombstone reconciliation, and reference-corpus equivalence remain |
| H063 | [Detect rollback of an otherwise valid database snapshot](llm-harness-roadmap.md#enhancement-063) | S2 | Partial | Unassigned | Added HMAC-signed external conversation-head manifests and comparison that reports rollback, divergence, or advancement; focused tamper/old-snapshot tests added. Operator-facing independent export, retention, and explicit reconciliation workflow remain |
| H064 | [Test schema evolution across supported release histories](llm-harness-roadmap.md#enhancement-064) | S2 | Partial | Unassigned | Added synthetic upgrade coverage from schema prefixes 1, 20, 35, 50, 68, 74, and 75, with 512 seeded events and an interrupted/retried migration. Released database fixtures, realistic multi-table scale, duration/disk measurements, and recovery-snapshot verification remain |
| H065 | [Describe tool effects and concurrency semantics in manifests](llm-harness-roadmap.md#enhancement-065) | S1 | Partial | Unassigned | Added a shared manifest contract for resources, effects, idempotency, reconciliation, cancellation, sensitivity, and concurrency. Omitted fields normalize to unknown and retry/parallel helpers fail closed. Runtime enforcement in dispatch/retry paths and false-declaration conformance fixtures remain |
| H066 | [Support prepare/preview/execute with resource preconditions](llm-harness-roadmap.md#enhancement-066) | S1 | Partial | Unassigned | Added host resource-version storage, `chain.plan` version capture, effect-free `chain.preview`, execution-time rechecks, and version tokens in queued effects. Missing and stale observations fail closed; effectful steps also require provider-declared conditional updates. Tests prove version drift blocks execution before a run or outbox effect is created. Provider observation hooks for file/draft/record/recipient mutations and relay-side conditional enforcement across integrations remain |
| H067 | [Model compensating actions for partially completed workflows](llm-harness-roadmap.md#enhancement-067) | S6 | Implemented; revalidate | Unassigned | Effectful chain steps may declare compensation separately. A failure reports completed effects and residual delivery state; only confirmed-delivered originals create a separate compensation run and approval. A unique origin ledger and stable original-run/step idempotency key prevent blind redrive; results say compensation enqueued and never claim the original effect was undone. Partial-failure, approval, and duplicate-resume tests pass. Provider-specific reversal reconciliation and live delivery-outcome drills remain |
| H068 | [Parallelize only independent tool work](llm-harness-roadmap.md#enhancement-068) | S4 | Implemented; revalidate | Unassigned | The runner carries manifest read/write and dependency contracts into a bounded scheduler (maximum four calls). Only declared read-only tools with resource sets and no unmet dependencies batch; unknown tools and writes stay ordered. Per-read timeout, cancellation, and panic outcomes are collected before durable results are committed in call order, preserving tool-use/tool-result pairs. Tests prove overlap, width bounds, failure/cancellation collection, dependency gating, and plugin metadata propagation. Adversarial false-declaration fixtures and production latency benchmarks remain |
| H069 | [Specify timer behavior across clock changes and restart](llm-harness-roadmap.md#enhancement-069) | S2 | Partial | Unassigned | Run budgets now retain UTC deadlines and a persisted wall-clock high-water mark; timer decisions reject rollback, and retry/effect reservations and step-lease claims observe that guard. Routine ticks accept injected UTC, monotonic Tokio waits remain elapsed-time based, and tests cover rollback, immutable deadlines, restart recovery, bounded missed work, and DST behavior. Approval expiry and all non-run leases/retry queues still need the same clock guard and forward-jump qualification |
| H070 | [Give routines explicit missed-run and overlap policies](llm-harness-roadmap.md#enhancement-070) | S6 | Implemented; revalidate | Unassigned | Added persisted skip/coalesce/bounded catch-up and forbid/queue/replace policies, occurrence-key uniqueness, serialized active claims, and timezone-aware previews. Definition edits cancel queued occurrences; tests cover DST folds/gaps, downtime policies, occurrence deduplication, overlap serialization, edits, scheduler execution, and the SPA policy payload. Supported-platform scheduler qualification remains |
| H071 | [Provide safe dead-letter inspection and controlled redrive](llm-harness-roadmap.md#enhancement-071) | S2 | Partial | Unassigned | Controller dead-letter view covers exhausted outbox effects, failed memory extraction, failed automation runs, and bounded-retry automation bus events; summaries omit payloads and classify failure causes. Same-identity redrive is audited for outbox, extraction, and automation run IDs; unknown outbox outcomes remain on the receipt-resolution path. Per-job UI redrive, core retry/audit tests, and a UI test added. Startup now resumes only pending/running automation runs with durable Controller redrive audit entries, using their frozen definition and source event. Handler-level dead-letter events remain inspect-only until a target-scoped retry can avoid fanning out to newly enabled automations; live poison-job fairness/reconciliation matrix remains |
| H072 | [Enforce distinct least-privilege runtime profiles](llm-harness-roadmap.md#enhancement-072) | S2 | Partial | Unassigned | Added shared runner/browser/parser/coding/integration-sidecar profiles with CPU, memory, PID, file-descriptor, tmpfs, network, rootfs, capability, and non-root image requirements. Runner, coding executor, and sidecar launch consume the profiles; native-process fallback and root images fail closed. Sidecar host/home/socket mounts are rejected; subprocesses use a curated inherited environment. Explicit per-profile UID mapping, egress allowlisting, and tool-compromise denial drills remain |
| H073 | [Qualify isolation on every supported operating system](llm-harness-roadmap.md#enhancement-073) | S2 | Partial | Unassigned | Container profiles now require a Linux Docker daemon and the runner/native path cannot silently fall back to an unsandboxed process. Added portable negative contract tests. Actual packaged Windows, macOS, and Linux container/native denial tests and the enforced/emulated/unavailable capability matrix remain |
| H074 | [Budget processes, disk, descriptors, and output as well as memory](llm-harness-roadmap.md#enhancement-074) | S2 | Partial | Unassigned | Coding and runtime profiles enforce descendant PID/CPU/memory/file-descriptor limits, bounded tmpfs, disabled coding egress, 24 KiB retained command output, bounded LSP frames, and existing per-artifact output caps; sidecar logs rotate at a fixed size. Startup/hourly artifact sweeping removes expired references and crash-leftover blobs after a grace period. Persistent host-backed sidecar/workspace volume quotas, aggregate artifact accounting, and flood/other-conversation responsiveness tests remain |
| H075 | [Drain safely during shutdown, update, and host suspend](llm-harness-roadmap.md#enhancement-075) | S2 | Partial | Unassigned | Unix SIGTERM/Ctrl-C and Windows SCM stop/shutdown/preshutdown/suspend now trigger Axum graceful drain with a 30-second deadline; plugin lifecycle hooks/processes, outbox workers, managed backends, sidecars, and runners receive bounded teardown. Runner volumes and durable leases remain recoverable. Process-kill-at-each-phase qualification, live suspend/update drills, and forced-deadline unresolved-effect evidence remain |
| H076 | [Reduce secret lifetime and document host-compromise limits](llm-harness-roadmap.md#enhancement-076) | S2 | Implemented; revalidate | Unassigned | Vault master/HMAC key buffers and SQLCipher key configuration zeroize on drop; Windows key files receive a DACL before bytes are written; plugin, MCP, native-service, Docker, helper, and hardware-probe processes inherit only an explicit OS/Docker allowlist, with MCP per-server environment values added deliberately. Rotation/redaction/key-permission and environment-filter tests exist. OS crash-dump, hibernation, and fully compromised-host limits are documented; packaged-process and external-credential rotation drills remain |
| H077 | [Give artifacts transactional references and safe garbage collection](llm-harness-roadmap.md#enhancement-077) | S2 | Implemented; revalidate | Unassigned | Content-addressed publication uses synced temporary files and atomic rename before SQLite reference commits; deduplicated purge checks both attachment and artifact references; active reads/open handles coordinate with the collector; an hourly sweeper removes expired rows and aged orphan/temp blobs. Tests cover renamed-before-commit recovery, live-reference preservation, cross-scope child-result denial, and reader/collector locking. Supported-volume crash and concurrent-process qualification remain |
| H078 | [Qualify useful hardware tiers beyond the primary GPU path](llm-harness-roadmap.md#enhancement-078) | S3 | Partial | Unassigned | Benchmark records now include per-task latency, live CPU/RAM/GPU probes, Linux battery/thermal/throttle observations, and explicit unavailable telemetry; offline fixtures remain marked as scoring-only. Reproducible model-quality/latency reports for each advertised CPU/Apple/Intel/AMD/NVIDIA tier, plus visible approved-model fallback that preserves required capabilities, remain |
| H079 | [Ship a plugin author conformance kit](llm-harness-roadmap.md#enhancement-079) | S6 | Implemented; revalidate | Unassigned | `execlaw-plugin-conformance` generates script/subprocess sample projects, runs them through mock inputs, validates input/result schemas and declared capabilities, checks lifecycle fixtures, and rejects authority-expanding or trust-relaxing upgrades with actionable messages. Server integration coverage installs the generated author sample through the public lifecycle API. Manifest JSON Schema is generated from plugin-sdk structs. Independent third-party author and release qualification remain |
| H080 | [Negotiate plugin API compatibility explicitly](llm-harness-roadmap.md#enhancement-080) | S6 | Implemented; revalidate | Unassigned | Manifests can declare a semver host API range and required protocol features; unsupported ranges, unknown security features, and unknown compatibility fields fail during install parsing. Legacy bundles without the table remain supported. Deprecated `host_log` remains an alias for `log_info`; enable-time diagnostics name the replacement without changing its authority or effect. Supported-release fixture qualification remains |
| H081 | [Pin executable tool versions for in-flight runs](llm-harness-roadmap.md#enhancement-081) | S6 | Implemented; revalidate | Unassigned | Durable run snapshots now store plugin artifact-tree digests, versions, schemas, and MCP declared-server configuration identities/schema hashes. Dispatch rechecks the current identity before calls; run-scoped mutation leases prevent run pin creation racing with plugin upgrade/disable/uninstall and block those operations until all pinned runs drain. Tests cover snapshot immutability, active-run retention, changed-code denial, and mutation blocking. Real operator-managed deployment verification and release/platform qualification remain |
| H082 | [Support publisher revocation and offline compromise response](llm-harness-roadmap.md#enhancement-082) | S2 | Partial | Unassigned | Added Controller-approved publisher/digest revocation records with source, freshness, issue/expiry times, recovery package references, audit events, impact inventory, and checks on verified reinstall and digest-pinned OCI authorization. Local staged plugin trees now receive a content digest, and startup quarantines signed plugin artifacts whose publisher/digest has since been revoked. Tests cover non-Controller denial, offline metadata retention, impact inventory, clear/recover, reinstall denial, and OCI cache denial. Live running-sidecar stop/drain, operator-facing import/recovery workflow, and cached legacy/local package attribution remain |
| H083 | [Make plugin hook ordering, failure, and reentrancy predictable](llm-harness-roadmap.md#enhancement-083) | S6 | Partial | Unassigned | Event subscribers now sort by plugin ID and handler rather than enable timing; a reverse-enable-order regression test records the contract. Event hook execution is not currently wired into a dispatcher, so failure isolation/abort semantics, invocation budgets, reentrancy controls, and versioned conformance traces remain |
| H084 | [Trial a restricted WebAssembly plugin tier](llm-harness-roadmap.md#enhancement-084) | S6 | Partial | Unassigned | Added an opt-in Wasmtime 36.0.17 core-module tier with no imports, no host functions or ambient capabilities, and manifest validation requiring capability-free pure transform tools with no host effect surfaces. The typed JSON ABI uses a fresh store per call, 16 MiB memory, 1,024 table elements, 10 million fuel, 256 KiB stack, and bounded input/output. Module compilation is serialized off the async reactor and transform calls use a four-worker cap. Install/enable/hydrate/disable and conformance paths share plugin lifecycle; schema validation wraps results. Seven tests pass for imports, memory/table limits, fuel, input/output size, typed output, per-call isolation, and lifecycle. Deployment-size/overhead comparison against Rhai and subprocess plus supported-platform qualification remain |
| H085 | [Trial delegation between explicitly paired operator-owned hosts](llm-harness-roadmap.md#enhancement-085) | S4 | Partial | Unassigned | Added durable peer pairing with Ed25519 identity, HTTPS local/VPN endpoint and local-inference endpoint checks, signed inbound/outbound task envelopes, authority/data-label intersection, typed signed artifacts, monotone task transitions, revocation, and database-reopen recovery. Tests reject unpaired/public peers and forged authority/signatures, verify narrowing and typed results, and retain tasks across DB reopen. The live network adapter, transport reconnection drill, external artifact transfer, and measured task/hardware benefit remain |
| H086 | [Make instruction precedence inspectable and resistant to injection](llm-harness-roadmap.md#enhancement-086) | S3 | Implemented; revalidate | Unassigned | Prompt states the operator/system > task > repository/skill > retrieved/tool-data hierarchy; governed assets are explicitly labeled untrusted. Persisted hash-only source/version receipts are exposed through run trace inspection. Tests cover conflicting host/task instructions, receipt secrecy, and the operator inspection response; executable authority remains gated by host policy. Supported-model injection qualification remains |
| H087 | [Detect dependency cycles and capacity deadlocks](llm-harness-roadmap.md#enhancement-087) | S4 | Partial | Unassigned | Child dependencies are bounded to 128 tasks/64 edges each, duplicate edges are rejected, and reservation checks reject cycles with an actionable path in the same transaction. Regression tests cover cycle-path reporting, transactional rejection without partial reservation, and freeing the only remaining per-model slot for a child after parent inference returns. A shared wait graph for resource locks and approval waits remains |
| H088 | [Cache qualified read results with authorization and freshness checks](llm-harness-roadmap.md#enhancement-088) | S3 | Partial | Unassigned | Persistent cache keys scope results by conversation, principal/grant fingerprint, tool/version, canonical args, and fresh provider resource revisions. Dispatcher caches only explicit plugin read-only contracts, rechecks live access for each hit, skips writes/built-ins/MCP, and reports expired or revision-changed entries as stale before doing a live read. Tests cover actual dispatch hits, authority changes, conversation isolation, revisions, reauthorization, expiry, stale status, and effects. Criterion measured cache-hit lookup at 17.1-20.7 us and stale inspection at 38.4-43.7 us in the local isolated build; deletion invalidation probes and an end-to-end live-provider comparison remain |
| H089 | [Resolve entities without silently merging identities](llm-harness-roadmap.md#enhancement-089) | S5 | Implemented; revalidate | Unassigned | Added evidence-backed temporal aliases and merge proposals for person/project/place/resource entities. Same-name candidates remain ambiguous until a Controller decision; accept/reject/reverse records actor and reason append-only, reversal restores distinct candidates, and semantic redirects carry no trust. Controller APIs expose candidate ambiguity and proposal review. Core tests cover time bounds, ambiguity, merge, cycle rejection, reversal, and decision history; broader retrieval integration remains |
| H090 | [Separate explicit preferences from inferred personalization](llm-harness-roadmap.md#enhancement-090) | S5 | Implemented; revalidate | Unassigned | Added scoped explicit/inferred preference records with evidence, expiry, proposal/approval/reject/retract transitions, and Controller-only management APIs. One-task scopes cannot be persisted as preferences. Controller prompt loadouts include only approved global/agent-scope values and store hash-only receipts; other trust classes receive no preference block. Core tests cover governance, expiry, correction/removal, and inference states; server tests cover role gating and Controller-only prompt loadout |
| H091 | [Ingest documents with page, cell, and region evidence](llm-harness-roadmap.md#enhancement-091) | S5 | Planned | Unassigned | Pending |
| H092 | [Maintain a revision-aware local code and documentation index](llm-harness-roadmap.md#enhancement-092) | S4 | Partial | Unassigned | Added a Controller-only workspace symbol index with per-file SHA-256 updates, stale-revision receipts, bounded symbol lookup, and reconciliation for renames/deletions; the regression test covers those changes and excluded secret files. Reference/caller edges, broader language coverage, usefulness measurements against plain text search, and execution of the new server test remain |
| H093 | [Apply patches with explicit file preconditions and transactions](llm-harness-roadmap.md#enhancement-093) | S4 | Partial | Unassigned | Multi-file checkout patches now validate all hashes and bounds before writes, reject duplicate paths, preserve permissions, and roll back already-applied files on a later I/O failure while protecting concurrent edits. Same-request replay accepts already-applied files so a reclaimed durable job can finish. Server regression tests were added but remain unverified because unrelated missing core modules currently stop server compilation; process-kill recovery and permission/encoding matrices remain |
| H094 | [Use structured command specifications and platform-aware execution](llm-harness-roadmap.md#enhancement-094) | S4 | Partial | Unassigned | Workspace jobs now carry argv, a validated workspace-relative cwd, and bounded stdin; the sandbox rejects standard shell entry points and credential-like flags/URL user-info, uses a fixed environment, and returns exit and timeout separately. The request-validation test passes. Actual Windows/POSIX process behavior, universal secret echo prevention, and cancellation-to-container cleanup qualification remain |
| H095 | [Manage development servers as owned run resources](llm-harness-roadmap.md#enhancement-095) | S4 | Planned | Unassigned | Pending |
| H096 | [Make test and build evidence independently verifiable](llm-harness-roadmap.md#enhancement-096) | S4 | Partial | Unassigned | Host-generated workspace run evidence binds recognized test/build argv, exit status, content revision, approved image identity, bounded-output SHA-256, test additions/changes/deletions, skip count, and stale state; cached evidence is downgraded after checkout edits. Focused evidence and workspace tests pass; broader test-framework skip parsing and required-test policy remain |
| H097 | [Treat dependency installation as an explicit execution boundary](llm-harness-roadmap.md#enhancement-097) | S4 | Partial | Unassigned | Workspace containers now force network-disabled read-only source mounts, ephemeral HOME/cache/target paths, Cargo/npm/Python offline modes, npm lifecycle-script suppression, and a fixed credential-free environment; profile regression passes. Dedicated lock/source-verified install/fetch receipts, controlled warm-cache reproducibility, and package-diff review remain |
| H098 | [Validate application migrations against disposable local databases](llm-harness-roadmap.md#enhancement-098) | S4 | Partial | Unassigned | Controller workspace tool applies bounded SQL fixtures and migrations to a host-created in-memory SQLite database, emits fixture/migration hashes and schema additions/removals/changes, flags destructive statements, and states forward-only rollback limits. Focused migration regression passes; non-SQLite engines, generated fixtures, and engine-specific rollback/recovery qualification remain |
| H099 | [Verify UI changes in an isolated browser with attributable evidence](llm-harness-roadmap.md#enhancement-099) | S4 | Planned | Unassigned | Pending owned preview lifecycle (H095), isolated browser-context execution, revision-bound screenshot/accessibility artifacts, and approved-origin navigation gates |
| H100 | [Bind sensitive sends to recipient and audience identities](llm-harness-roadmap.md#enhancement-100) | S1 | Partial | Unassigned | Drafts snapshot exact channel/recipient, native group ID, opaque observed member IDs, group epoch/hash, source event, and revision; Controller UI shows those identities, and send admission revalidates the observed roster. Core regression passes after a member is added. Authoritative complete transport rosters, offline membership-change detection, and atomic membership-epoch fencing through effect admission remain |
| H101 | [Normalize message edits, deletion, reactions, and reply lineage](llm-harness-roadmap.md#enhancement-101) | S6 | Partial | Unassigned | Append-only operation receipts, deterministic revision projection, reaction state, reply targets, deleted-text search exclusion, manifest capability checks, visible fallback, and Slack event mapping are implemented. Core archive tests passed 4/4, Slack decoder tests 4/4, and plugin-host capability test 1/1; other provider adapters still declare create-only support |
| H102 | [Make cross-channel continuity an explicit identity operation](llm-harness-roadmap.md#enhancement-102) | S6 | Partial | Unassigned | Controller APIs require two observed transport identifiers, record explicit links/unlinks, and persist selected direct-message content with origin and destination. Core identity tests passed 2/2; operator UI and authenticated end-to-end transfer journey remain |
| H103 | [Coalesce trigger bursts without losing event meaning](llm-harness-roadmap.md#enhancement-103) | S6 | Partial | Unassigned | Agent mailbox admission has durable availability windows, urgent keyword bypass, bounded same-recipient/version batches, and append-preserving correction supersession. The 1,000-event restart/correction fixture passed; automation workflow admission remains |
| H104 | [Qualify webhook replay resistance and credential rollover](llm-harness-roadmap.md#enhancement-104) | S1 | Partial | Unassigned | Exact-body HMAC supports signed timestamp/event identity, bounded parsing, durable receipt claims, overlap keys, retryable handler failure, and sanitized accepted/rejected outcomes. Existing webhook integration suite passed 11/11; the new acknowledged-rejection outcome case and workspace release suite remain unverified |
| H105 | [Support an explicit operator takeover and hand-back state](llm-harness-roadmap.md#enhancement-105) | S6 | Partial | Unassigned | Prior owner and operator-era events survive hand-back; pending sends are canceled and in-flight sends become unknown for reconciliation; the draft inbox refreshes ownership after reconnect. Core ownership/outbox tests passed; draft/approval preservation and installed journey qualification remain |
| H106 | [Preserve multimodal grounding through transformations](llm-harness-roadmap.md#enhancement-106) | S5 | Partial | Unassigned | Hash-bound image/audio evidence, coordinates/time ranges, caller-scoped citations, stale visual action rejection, untrusted media policy boundary, and separate grounding/text-quality scoring are implemented with unit tests. OCR/crop/caption/transcript production and answer-path persistence/integration remain |
| H107 | [Model-check the critical state transitions](llm-harness-roadmap.md#enhancement-107) | S1 | Partial | Unassigned | Bounded breadth-first model explores lease ownership, restart, expiry, approval, outbox, completion, and cancellation; focused model and source-level lease/pairing tests pass. Hosted mutation/counterexample qualification and broader implementation/model correspondence remain |
| H108 | [Fuzz full protocol conversations and untrusted parsers](llm-harness-roadmap.md#enhancement-108) | S1 | Partial | Unassigned | Six isolated cargo-fuzz targets cover runner/server frames and conversations, registration version frames, manifests, ZIP staging, JSON Schema, JSON, and TOML; all targets compile. Timed fuzz campaigns, crash corpus retention, and stateful dispatch-level fuzz coverage remain |
| H109 | [Mutation-test the enforcement tests themselves](llm-harness-roadmap.md#enhancement-109) | S1 | Partial | Unassigned | CI mutation gate targets trust, approval, tool pairing, attachment scope, dispatch, and new grounding/model checks; no mutation outcome report has run on this snapshot, so survivors/equivalent cases remain unreviewed |
| H110 | [Prove local-only operation with denied-network integration tests](llm-harness-roadmap.md#enhancement-110) | S1 | Partial | Unassigned | Linux CI runs workspace tests inside a network namespace and rejects traced non-loopback connect/sendto/sendmsg attempts while allowing loopback fixtures. Hosted evidence and explicit inference/embedding/reranking/speech/judge/skill-evaluation startup, fallback, upgrade, and optional-component coverage remain |
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
| H131 | [Persistent goals, projects and work items above runs](llm-harness-roadmap.md#enhancement-131) | S7 | Planned | Unassigned | Pending; related H022/H042/H124 |
| H132 | [Reporting structure and versioned operating charters](llm-harness-roadmap.md#enhancement-132) | S7 | Planned | Unassigned | Pending; related H042/H052/H086/H087 |
| H133 | [Atomic work checkout and reassignment fencing](llm-harness-roadmap.md#enhancement-133) | S7 | Planned | Unassigned | Pending; related H023/H024/H042/H052/H069 |
| H134 | [Evidence-based task disposition, review and structured blockers](llm-harness-roadmap.md#enhancement-134) | S7 | Planned | Unassigned | Pending; related H022/H053/H096/H105/H112/H127 |
| H135 | [Durable continuation and stranded-work reconciliation](llm-harness-roadmap.md#enhancement-135) | S7 | Planned | Unassigned | Pending; related H023/H043/H069/H103/H113 |
| H136 | [Portfolio allocation for local execution resources](llm-harness-roadmap.md#enhancement-136) | S7 | Planned | Unassigned | Pending; related H030/H049/H074/H119 |
| H137 | [Governed staffing, specialization and retirement](llm-harness-roadmap.md#enhancement-137) | S7 | Planned | Unassigned | Pending; related H013/H026/H056/H075/H105/H119 |
| H138 | [Management dashboard and evidence-backed executive briefings](llm-harness-roadmap.md#enhancement-138) | S7 | Planned | Unassigned | Pending; related H035/H042/H115/H128; NX20 |
| H139 | [Explicit analysis, coding-worker and manager execution profiles](llm-harness-roadmap.md#enhancement-139) | S7 | Planned | Unassigned | Pending; related H040/H042/H051/H052/H056 |
| H140 | [A generic scoped worker admission and lifecycle API](llm-harness-roadmap.md#enhancement-140) | S7 | Planned | Unassigned | Pending; related H024/H026/H044/H052 |
| H141 | [A first-class external Paperclip adapter for execlaw](llm-harness-roadmap.md#enhancement-141) | S7 | Planned | Unassigned | Pending; related H043/H044/H079/H080 |
| H142 | [An optional manifest-driven Paperclip bridge plugin](llm-harness-roadmap.md#enhancement-142) | S7 | Planned | Unassigned | Pending; related H027/H044/H065/H079/H080/H081 |
| H143 | [Pairing and least-privilege credentials at both boundaries](llm-harness-roadmap.md#enhancement-143) | S7 | Planned | Unassigned | Pending; related H026/H052/H054/H056 |
| H144 | [Durable wake normalization with one scheduling owner](llm-harness-roadmap.md#enhancement-144) | S7 | Planned | Unassigned | Pending; related H024/H043/H070/H103/H104 |
| H145 | [Explicit external task, invocation and local-run state mapping](llm-harness-roadmap.md#enhancement-145) | S7 | Planned | Unassigned | Pending; related H022/H024/H035 |
| H146 | [Cross-system cancellation, restart and session resumption](llm-harness-roadmap.md#enhancement-146) | S7 | Planned | Unassigned | Pending; related H023/H043/H052 |
| H147 | [Outbox-mediated Paperclip mutations and reconciliation](llm-harness-roadmap.md#enhancement-147) | S7 | Planned | Unassigned | Pending; related H024/H065/H066/H071 |
| H148 | [Isolated task context, memory and workspace binding](llm-harness-roadmap.md#enhancement-148) | S7 | Planned | Unassigned | Pending; related H037/H041/H048/H051/H086 |
| H149 | [Safe work-product, progress and usage reporting](llm-harness-roadmap.md#enhancement-149) | S7 | Planned | Unassigned | Pending; related H035/H049/H054/H055/H077/H122 |
| H150 | [Revision-bound approvals and questions without authority laundering](llm-harness-roadmap.md#enhancement-150) | S7 | Planned | Unassigned | Pending; related H010/H053/H105/H112 |
| H151 | [Qualified local networking and local-only delegated inference](llm-harness-roadmap.md#enhancement-151) | S7 | Planned | Unassigned | Pending; related H025/H034/H054/H085/H110 |
| H152 | [Qualified Paperclip worker, lead and executive operating profiles](llm-harness-roadmap.md#enhancement-152) | S7 | Planned | Unassigned | Pending; related H022/H042/H053/H119 |
| H153 | [Pinned compatibility, upgrade and supply-chain qualification](llm-harness-roadmap.md#enhancement-153) | S7 | Planned | Unassigned | Pending; related H028/H079/H080/H081/H082/H114 |
| H154 | [Staged integration drills and management-quality evaluation](llm-harness-roadmap.md#enhancement-154) | S7 | Planned | Unassigned | Pending; related H021/H023/H049/H107/H110/H111 |

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
| [F13](llm-harness-roadmap.md#finding-f13) | [H029](llm-harness-roadmap.md#enhancement-029), [H045](llm-harness-roadmap.md#enhancement-045) | Closed | Unassigned | HTTP MCP tests (7) and the stdio client suite (15) pass; responses are bounded and incremental, wrong IDs and version changes fail closed, session expiry does not replay tool calls, and a matching SSE result returns before EOF |
| [F14](llm-harness-roadmap.md#finding-f14) | [H027](llm-harness-roadmap.md#enhancement-027), [H054](llm-harness-roadmap.md#enhancement-054), [H072](llm-harness-roadmap.md#enhancement-072) | Open | Unassigned | Pending |
| [F15](llm-harness-roadmap.md#finding-f15) | [H047](llm-harness-roadmap.md#enhancement-047) | Closed | Unassigned | `voice_runtime::tests::stalled_stt_flush_does_not_block_another_sessions_interrupt` passed: one session's blocked STT flush did not delay another session's interrupt beyond the 500 ms bound; session state is isolated behind per-session mutexes |
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

- 2026-10-07: implemented the H081 run pin path and H084/H085 lab trials.
  H081 persists local plugin and MCP server/schema identities in each durable
  run, rechecks them before dispatch, and holds a mutation lease while pinned
  runs are active. H084 adds the opt-in no-import Wasmtime JSON transform tier;
  H085 adds signed, durable paired-host task contracts and authority narrowing.
  Focused core, plugin-host, plugin-sdk, and WASM adversarial tests pass; the
  server check and MCP schema-drift regression also pass. Live paired-host
  transport and measured-benefit gates remain open.

- 2026-10-07: began H082 revocation and H083 deterministic hook-order slices.
  Added migration 0080 and focused adversarial tests. H082 remains Partial
  because live sidecar drain and the operator import/recovery flow are open;
  H083 remains Partial because the event hook dispatcher and its execution
  contract are not implemented. Focused core provenance and plugin-host order
  tests subsequently passed; live sidecar drain and event dispatch remain open.

- 2026-10-06: continued the main numbering with H131-H154 for the 24
  Paperclip-related enhancements; merged their rows into the item ledger and
  updated links/totals. Requirements, GitHub references and existing status
  evidence are preserved.

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
The main ledger above covers H001-H154.

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

## Paperclip coordination and interoperability

Added 2026-10-06. The [Paperclip roadmap section](llm-harness-roadmap.md#paperclip-roadmap)
owns the current-code comparison, pinned upstream evidence, two-way architecture,
compatibility findings and H131-H154 acceptance criteria. These 24 enhancements continue the numbering at H131-H154 and are Planned.
Their main-ledger rows have no assigned owner or integration-test evidence.
Existing H001-H130 and NX01-NX30 progress remains unchanged.

Two tracks can proceed independently: H131-H139 extend native work management;
H140-H154 qualify optional Paperclip participation. Reuse current durable
children, mailboxes, ownership/review, workspace, budget and client contracts.
The analysis was pinned to Paperclip commit
`f77fcbf4bfc2bf1fb995abda63f2345a34652dd4`; upstream code was read, not executed.
Prior execlaw test reports were not rerun or promoted to fresh qualification.

H131-H154 are listed in the [main item ledger](#item-ledger), with the same
status, owner and evidence fields as all earlier enhancements. Priority, effort,
acceptance criteria and pinned GitHub references remain in the roadmap.

### Paperclip delivery gates

| Stage | Scope | Required evidence before advancing |
|---|---|---|
| Native management | H131-H139 | Durable task ownership across runs; evidence-based disposition; no privilege inherited from an org title; bounded continuation and aggregate resource accounting |
| Paired observer | H140-H143, H148, H151, H153 | Scoped credentials, exact paired identities, approved local connectivity; observation only, with no issue checkout, assigned-task execution or progress/status writes |
| Bounded worker, including locally read-only tasks | H144-H150, H154 and observer gates | Actual adapter/plugin restart and acknowledged cancellation; outbox attribution and reconciliation before any checkout/release/report; scoped artifacts/usage, independent approval gates and verified local artifact |
| Lead/executive | H152 and prior worker gates | Parent/goal-linked bounded delegation; board staffing/budget proposals; review and escalation tests; no self-approval or cloud delegation |

Keep H154 tests alongside every stage. A generic HTTP success or CLI exit is
not evidence that issue work is complete. The external-task Draft v1 is not
the worker execution contract. Production compatibility requires the pinned
adapter types and actual upstream API behavior to pass, including cancellation
and unknown-outcome cases.

H146/H147 are prerequisites for the first assigned-worker pilot, not only for
local tool writes: Paperclip checkout and progress reporting already mutate
external state. Enabling coding or other local effects additionally requires
the corresponding H040/executor/policy qualification gates.

Assign an owner and record revision, tested Paperclip/bridge versions, models,
hardware/platform, fixture/live scope, results and open limitations when a
slice begins. Related enhancement qualification remains binding; successful Paperclip work
does not automatically qualify its parent H item. All external mutations in
future tests must use disposable operator-approved instances and scoped test
credentials, never the operator's live task board.

Review record: documentation and source/contract analysis only. No Paperclip
installation, company/agent/task creation, live callbacks, inference, commit,
or deployment was performed for this addition.
