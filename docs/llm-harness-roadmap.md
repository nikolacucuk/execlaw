# Local LLM Harness Roadmap

Status: accepted implementation scope, 2026-09-27. **All 130 enhancements
(H001-H130) are accepted implementation scope.** This document owns their scope,
acceptance criteria, and the F01-F19 review findings. The
[`implementation-plan.md`](implementation-plan.md) ledger owns delivery status,
assigned owners, dependencies, and verification evidence; it must be updated
with each implementation change. [`remaining-improvements-todo.md`](remaining-improvements-todo.md)
is the immediate work queue, not a replacement for that complete ledger.

The [Nexus visual extension](#nexus-visual-roadmap) adds separately tracked
NX01-NX30 proposals and NXF01-NXF11 findings. These refine the chat appearance
experience without renumbering the H baseline or implying that a redesign has
already shipped.

A checked item records reported implementation, not independent release
qualification. Open review findings and missing acceptance evidence can reopen
it. H005, H009, H011, and H018 have completed their requested implementation
and focused acceptance checks. H020 remains verification-blocked. H021's
implementation and offline scorer checks are recorded below; live benchmark
qualification remains pending.
Component design documents refine the implementation without dropping roadmap
scope. "API access" means Ollama or an OpenAI-compatible
endpoint served on the operator's hardware or explicitly approved local
network. Cloud LLM providers and default internet inference are out of scope.
The local endpoint policy, SQLite configuration, event log, trust ladder,
outbox, and plugin contract remain authoritative.

1. [x] <a id="enhancement-001"></a> **Select native Ollama for an operator-managed endpoint.** The existing
   remote form assumed OpenAI compatibility, whose Ollama shim can drop tool
   calls. The form now records `binary_hint: ollama` and requires a model tag;
   the resolver test verifies native client selection. OpenAI-compatible stays
   the default, and the local endpoint policy still checks the URL.
2. [x] <a id="enhancement-002"></a> **Add backend protocol conformance probes.** A healthy `/api/tags` or
   `/v1/models` does not prove tool-call or streaming compatibility. Exercise
   a minimal text turn, streamed turn, and no-effect tool-call round trip for
    both protocols against a local fixture and show pass/fail per capability.
    `POST /api/admin/inference/conformance` reports bounded text, stream, and
    declared no-op tool checks for the Standard backend; it never dispatches
    a tool. The Backends page shows pass/fail per capability. Local fixtures
    cover Ollama native and OpenAI-compatible wire formats.
3. [x] <a id="enhancement-003"></a> **Reject an unavailable model explicitly in production turns.** A stub
   response can look like a real assistant answer when a managed backend is
   starting or down. Return a typed, visible unavailable state while retaining
    the deliberate dev-only stub path; test both absent and recovering backends.
    Web chat returns `inference_unavailable` (503) before persisting attachments;
    routines and inbound transport turns return the same typed condition and
    surface an alert without committing a synthetic answer. Tests cover all
    three paths and preserve the explicit development-only stub behavior.
4. [x] <a id="enhancement-004"></a> **Capture backend readiness and version transitions.** An endpoint can
   serve HTTP before loading the chosen model. Record model ID, last successful
   probe, loading/error status, and the transition time in SQLite; test
    supervisor restart and model switch without a server restart.
    Migration 0030 stores derived managed-backend stage, model, last observation,
    and last healthy time. Backend edits clear stale readiness; reopen and
    model-switch tests cover the store, and the status API distinguishes a
    stopped supervisor from its previously healthy observation.
5. [x] <a id="enhancement-005"></a> **Bound inference retries by error class and deadline.** Retrying an
   invalid model or malformed request burns latency, while transient local
   failures merit a bounded retry. Persist attempt metadata per run and test
    timeout, 429/503, cancellation, and non-retryable 4xx separately.
    Non-streaming runner turns use a bounded deadline-aware retry loop;
    `state_run_inference_attempts` records each attempt before the request and
    its retry or terminal state. Invalid 4xx responses are not retried. The
    production streaming path now retries transient errors only while opening
    the response, before exposing any chunks; after chunks are visible, failures
    are surfaced without replay. Stream opening and retry backoff observe
    cancellation in web chat, incognito chat, and both runner paths; API tests
    cover cancellation before response headers and during retry delay alongside
    error-class and deadline cases. Every production retry consumer supplies
    its active turn cancellation flag.
6. [x] <a id="enhancement-006"></a> **Prove tool-call schemas against each supported model adapter.** A
   model can emit invalid tool arguments even when a catalog is present. Add
   fixtures for Qwen, Llama, and generic OpenAI-compatible shapes; reject
   invalid calls before dispatch and test repaired calls stay within budget.
    The shared runner envelope is schema-validated before dispatch and has
    fixtures for each supported wire shape; malformed calls do not dispatch.
7. [x] <a id="enhancement-007"></a> **Make tool catalog budgets observable.** Large plugin catalogs waste
   context and prefill time. Measure schema tokens per tool and per turn,
   trim by capability and relevance before inference, and prove unavailable
   tools never appear in routing prose or the advertised catalog.
    The shared catalog builder enforces a 24 KiB serialized-declaration cap,
    logs included bytes and exclusions, and derives routing prose from the
    same retained tool names.
8. [x] <a id="enhancement-008"></a> **Complete the two-pass untrusted-content boundary.** The safe-analysis
   planner receives only framework-owned metadata and untrusted text, has no
   tools, and produces a bounded handoff for the executor. Untrusted content
   remains tainted across turns; tool-capable execution uses only the reviewed
   handoff and safe framework context. Tests cover the planner request, tool
   stripping, and persisted trust boundary.
9. [x] <a id="enhancement-009"></a> **Run a process-kill recovery matrix for every tool boundary.** The
   durable step store exists, but restart semantics need evidence. Kill runner
   and host before/after claim, dispatch, outbox enqueue, and commit; verify
   paired events and exactly one externally visible effect per idempotency key.
    Outbox claims now carry expiring process leases and are reclaimed after
    restart; enqueue, send-request, retry, delivery, and dead-letter transitions
    are persisted in order. A subprocess test kills the relay after claim,
    reopens the file-backed DB, and reclaims the expired lease. Automation bus
    dispatch now uses expiring, owner-fenced leases; a recovery test simulates a
    killed handler, restart, lease reclaim, stale-owner rejection, and ack by
    the recovered owner. Production automation persists immutable graph
    definitions and completed node outputs, then resumes a redelivered run.
    The runner replays completed model/tool checkpoints, recovers paired event
    commits without a second model turn, and refuses to redispatch an in-flight
    tool without a durable idempotency fence. Process-kill tests cover
    automation lease reclaim, runner tool checkpoint recovery, host recovery
    after paired commit, and relay restart after sink acceptance; the sink
    returns the same receipt for the stable idempotency key, proving one visible
    effect. Unknown unfenced effects stay explicitly unresolved for operator
    reconciliation instead of being repeated.
10. [x] <a id="enhancement-010"></a> **Bind approvals to a specific pending effect and replay state.** A
    stale approval must not authorize a rephrased or replaced action. Effectful
    chain approvals bind the pending plan hash, conversation, Controller
    principal, and expiry into the signed token; the shared UI/sideband route
    rechecks all claims and the live plan before resume. Tests cover expiry,
    duplicate use, changed plan, and principal mismatch.
11. [x] <a id="enhancement-011"></a> **Provide an auditable transport delivery timeline.** "Drafted",
    "send requested", and "delivered" are different states. Project outbox
    and transport acknowledgments into the chat UI with event references;
   test retries, dead letters, and a restart between send and acknowledgment.
   Chat drafts and manual sends record distinct draft, send-request, accepted,
   delivered, and failed states with event references. A send request is not
   treated as delivery; only an explicit plugin receipt advances it to
   delivered. Outbox transitions now persist an ordered delivery timeline,
   reclaim expired send leases, and store an opaque receipt returned by a
   dispatcher only after sink acknowledgment. `GET /api/chats/:id/messages`
   projects both transport-review decisions and outbox transitions onto the
   originating message; the SPA displays transition, attempt, time, and receipt.
   Restart after remote acceptance but before acknowledgment is committed is
   covered with a file-backed process-kill test and an idempotent sink fixture.
   The chat projection and SPA timeline regression verify the operator-visible
   sequence and opaque receipt.
12. [x] <a id="enhancement-012"></a> **Index conversation search without weakening trust scope.** Full
    event replay is costly for long threads. Build a derived SQLite FTS index
    keyed by conversation and event seq; verify HMAC-backed reads, incognito
    deletion, source filters, and no cross-conversation results. These safety
   checks are implemented. FTS5 is a derived per-conversation/event-sequence
   projection with a persisted incremental watermark. Search is Controller-only,
   replays and verifies the HMAC chain before reading indexed content, applies
   conversation/source filters, and removes indexed rows on incognito deletion.
   Whole-chain verification remains the integrity check on each search.
13. [x] <a id="enhancement-013"></a> **Finish governed memory-asset loadouts.** Metadata and HOT memory
    exist, but role/task bindings are not fully resolved into turns. Enforce
    trust filtering before ranking, byte budgets, version hashes, and an
    approval gate; test lower-trust and expired-asset exclusion. Controller-only
    binding management feeds chat, routine, and child-agent prompt assembly;
    loadouts filter trust, owner scope, lifecycle, expiry, and turn bytes before
    rendering content, with source hashes and versions included.
14. [x] <a id="enhancement-014"></a> **Close the memory lifecycle review loop.** Promotion/demotion
    candidates without a sweeper and controller approval UI accumulate
    silently. Add the server sweeper and decision surface; test stale
    proposals, evidence citations, and restart idempotency.
    The hourly bounded server worker creates idempotent proposals. The
    controller-only Approvals feed exposes the exact memory-row reference and
    approve/reject actions; approval refuses a target whose tier changed, and
    core tests cover stale proposals and duplicate sweep proposals.
15. [x] <a id="enhancement-015"></a> **Evaluate skill capture against held-out local tasks.** A reusable
    skill can also preserve a bad solution or a secret. Secret scanning remains
    mandatory on skill writes; Controller-authored held-out cases run through
    the configured local Standard backend with tools disabled. Evaluation
    records bind immutable body, suite, evaluator, model, and backend identities; promotion
    requires a passing run for the current body and suite. Before/after scores
    compare only when the parent version used the same suite and local backend.
    No cloud judge or inference path is used.
16. [x] <a id="enhancement-016"></a> **Version prompts, model settings, and tool catalogs per turn.** A
    replay should explain not just the event sequence but the exact inputs
    used by the local model. Persist hashes and immutable references; test
    that replay identifies drift after a backend or plugin upgrade. Migration
    0034 stores versioned SHA-256 fingerprints, reopening rejects drift, and
    `execlaw replay` reports hashes without copying sensitive prompt text.
17. [x] <a id="enhancement-017"></a> **Add offline adversarial evaluation suites.** Prompt injection,
    malformed tool calls, Unicode spoofing, SSRF, and cross-trust memory
    leakage require recurring tests. Run deterministic fixtures in CI; optional
    local-model quality evaluations remain separate from the enforcement gate.
    The deterministic suite runs focused policy, runner, and server tests offline;
   its attack classes, test locations, and command are documented in
   [`adversarial-evaluations.md`](adversarial-evaluations.md). Coverage includes
   delimiter smuggling, malformed tool arguments, Unicode controls/homoglyphs,
   SSRF, and conversation-scoped memory search with HMAC tampering.
18. [x] <a id="enhancement-018"></a> **Attribute latency and context cost by phase.** The inference probe
    exists, but operators need actionable turn breakdowns. Track prompt
    construction, local prefill/decode, tool wait, retries, and stream delay
    without logging sensitive prompt text; define regression budgets.
    The Ollama client no longer logs request bodies or echoes upstream
    error/NDJSON bodies. The admin metrics API and SPA report
    bounded p50/p95 prompt-assembly, local inference, tool-wait, retry-attempt,
    and first-visible-token latency by consumer. Inference duration is the
    local endpoint round-trip, not backend-internal prefill/decode timing.
    They also report serialized
    request bytes and estimated tokens at prompt assembly. Only numeric samples
    are retained in bounded 256-sample rings. After 16 observations, the
    endpoint compares the older and newer halves and flags a p95 increase
    greater than 25%; the relative budget is hardware-independent and
    process-local.
    Runner failure logs and optional diagnostics retain only request sizes,
    content lengths, timings, and error-chain lengths; prompt, partial output,
    and raw upstream errors are omitted. The broader source review also removed
    response bodies from inference decode errors, frame payloads from runner
    transport logs, and generated skip/block/error text from skill-capture logs.
    Sentinel tests protect the runner snapshot and inference response boundary.
    The source review also covered MCP request/notification bodies, WebSocket
    close reasons, skill capture, inference decoding, and tool failures; only
    bounded sizes, counts, codes, and error classes remain in those logs.
19. [x] <a id="enhancement-019"></a> **Validate resource-aware model routing.** Ollama, vLLM, voice, and
    vision compete for local RAM/VRAM. Check capacity before spawn, decline
    overcommitted configurations, and test recovery when a model releases
    memory or a device disappears. Managed model specs can declare
    `required_vram_mb`; the supervisor sums reservations per selected GPU and
    refuses an overcommit before downloads or spawn. It retries admission on
    later reconcile passes, so released capacity or an edited estimate can
    recover. Each declared `required_ram_mb` is compared with live available
    host RAM. Declared VRAM is compared with live free NVIDIA VRAM; missing
    samples, disappeared devices, and unmonitored GPU vendors fail closed.
    Non-NVIDIA managed models that declare VRAM need a vendor-specific live
    probe before they can start.
20. [ ] <a id="enhancement-020"></a> **Drill backup, key rotation, and secret incident recovery.** A backup
    is useful only if it restores encrypted SQLite, vault references, plugin
    state, and the HMAC chain. Automate a disposable restore check and test
    credential rotation/redaction without exposing secrets in logs or CI. The
    CLI verifies SQLite integrity, schema, and the complete event HMAC chain
    on encrypted backup and restore. A SQLCipher-only disposable drill checks
    pre-rotation backup readability, vault/plugin rows, stable event-signer
    verification, and post-rotation writes. `execlaw rotate-keys`
    takes a verified old-key recovery snapshot, rekeys the live DB, verifies a
    new-key snapshot, then durably replaces the master-key file and refreshes
    the OS keyring cache. Event signing uses a separate protected
    `event-hmac.key`, so database-key rotation does not rewrite history or
    invalidate signed checkpoints. OAuth client/token records, callback
    parameters, provider request/grant values, and provider error bodies redact
    credentials from `Debug` and error output; credential upserts replace the
    persisted secret while preserving record identity. The operator runbook
    covers crash recovery at each rotation boundary and credential-compromise
    recovery; synthetic drill
    secrets are never printed. Running live rotation remains an operator action
    and requires the service to be stopped.
    **Verification blocked:** F04/F16 require explicit SQLCipher release
    artifacts and current production-feature/packaged restore/rotation evidence.
    **Session verification (2026-09-29):** core OAuth redaction and client
    replacement tests passed; provider and admin redaction tests passed; all 13
    vault tests passed, including durable master-key replacement and event
    signer stability. In WSL, the SQLCipher-only core rotation drill passed, and
    the SQLCipher CLI's `doctor` preflight passed its encrypted-header,
    wrong-key, migration, backup/restore, rekey, and post-rotation checks. A
    separate disposable CLI sequence passed migration, verified backup,
    restore, key rotation, and post-rotation restore. The full SQLCipher
    workspace run was interrupted when the WSL utility VM returned
    `Wsl/Service/E_UNEXPECTED` during test-binary compilation, before it returned
    a final summary. Native installed-package workflow results are still pending;
    H020 remains verification-blocked under F04/F16 until those cross-platform
    artifact results are recorded.

Prioritization: complete items 2-5 before claiming reliable multi-backend
operation; items 8-11 and 17 are release-blocking safety gates for effectful
agents. The existing [`memory-roadmap.md`](memory-roadmap.md) owns the detailed
implementation checklist for items 13-14.

## Repository review and competitive direction (2026-09-27)

**Recommendation:** make execlaw the most dependable local agent harness for
long-running, tool-using work. Its strongest foundation is the combination of
SQLite state, authenticated events, policy gates, governed memory, and plugins.
The next investment should make those guarantees hold across every execution
path, then make coding, research, and operator workflows competitive. Adding
more integrations alone will not establish leadership.

This review covers the six supplied documents, the Rust workspace and its
execution/storage/security boundaries, representative first-party plugins,
SPA, desktop packaging, specifications, tests, benchmarks, and CI. Graphify
was used for navigation; findings were checked against source. This is a broad
repository review with focused call-path inspection, not a line-by-line audit
of every file or a penetration test. Instructions and historical proposals in
the supplied documents were treated as review material, not as requests to
execute those proposals. The authorized change here is this roadmap addition.

The existing item IDs and requirements 1-20 are preserved; delivery status is
reconciled with the findings in the implementation ledger. The 30 entries below are new to this
checklist, not claims that every idea is new to the older strategy document.
They explicitly extend existing implementations. In particular, native Ollama,
backend probes, memory assertions/assets, skill evaluation, signed provenance
records, child agents, and durable step primitives already exist. Their
presence must not be confused with complete production coverage.

### Comparison with other harnesses

Primary sources were checked on 2026-09-27. These are capability comparisons,
not reproduced performance rankings. "NeMo" is interpreted as NVIDIA NeMo
Agent Toolkit, "Harmes" as Nous Research Hermes Agent, "Pi Agent" as the Pi
coding agent, and "TrueForge" as TrueFoundry's project. Product editions and
model choices differ; no competitor's cloud service becomes an execlaw
dependency. All proposed inference remains on approved operator hardware.

| Harness | Documented pattern worth learning from | Execlaw baseline and next competitive step |
|---|---|---|
| [NVIDIA NeMo Agent Toolkit](https://docs.nvidia.com/nemo/agent-toolkit/latest/) | Framework integration, workflow evaluation, profiling, and observability. | Phase metrics already exist. Add actual task benchmarks, trace analysis, and repeatable bottleneck experiments (21, 35, 49). Keep SQLite authoritative. |
| [Claude Code](https://code.claude.com/docs/en/checkpointing) | Integrated code/conversation rewind and selective summarization. Its documented checkpoints do not cover arbitrary shell changes. | Event history is not a workspace snapshot. Add a coding plugin and explicit workspace checkpoints with clearly bounded restoration (40-41). |
| Codex: [worktrees](https://learn.chatgpt.com/docs/environments/git-worktrees), [approvals and security](https://learn.chatgpt.com/docs/agent-approvals-security) | Isolated parallel working copies and explicit execution permissions. | Build workspace isolation, inspectable delegated runs, and policy explanations on the existing capability system (26, 40-44). A worktree alone is not a sandbox. |
| [Cursor Agent](https://cursor.com/docs/agent/overview) | Editor-integrated search/edit/run/browser tools, checkpoints, and queued or immediate steering. | The SPA provides chat and agent administration. Add editor access, safe steering, reviewable diffs, and acceptance evidence (22, 41, 43-44). |
| [TrueForge](https://github.com/truefoundry/trueforge) | Chat/API/embeddable SDK surfaces, deferred tools, large-result offloading, compaction, and approval checkpoints. | Retain execlaw's local deployment boundary; add context-efficient discovery and a stable client contract (31-34, 44). Upstream benchmark claims were not reproduced. |
| [OpenHands Software Agent SDK](https://docs.openhands.dev/sdk) | Modular software-agent APIs, workspace tools, persistence, and agent-server interfaces. | General-purpose automation is already broad. Develop a focused coding workspace plugin and external client adapter rather than putting language tooling into core (40, 44). |
| [Hermes Agent](https://github.com/NousResearch/hermes-agent) | Experience-derived skills, memory, session recall, messaging, and scheduled work. | Execlaw already has analogous learning and transport features. Differentiate through evidence inspection, executable skill tests, and revocable promotion (37-39). |
| [Pi coding agent](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/README.md) | A small extensible terminal agent with skills, extensions, RPC, and SDK entry points. | Keep the host small; make optional capabilities discoverable through plugins and expose a thin terminal/editor client (33, 40, 44). |

Two comparison tracks are necessary. A **harness-only** track uses the same
local model, quantization, hardware, tools, tasks, and budgets wherever both
harnesses support them. A **product** track compares each supported product
configuration and labels model/provider differences; it cannot attribute a
win solely to harness design. Closed products that cannot use the selected
local model belong only in the second track. Competitor runs must not receive
operator data or become a cloud inference path inside execlaw.

### How to prioritize and prove progress

P0 blocks a trustworthy release; P1 materially improves task completion or
daily usability; P2 expands reach after the foundations pass. Effort is
relative: S is a contained change, M spans several components, L requires a
new subsystem or extensive integration. These are planning estimates, not
calendar promises. Acceptance thresholds below are proposed gates, not
measurements achieved by this review.

First close findings F01-F08 and the remaining work in 5/11, then establish
21-24 and 29 as production acceptance gates. Develop context efficiency
(31-34) and the coding experience (40-44) against those gates. Memory,
research, voice, and ecosystem expansion should follow measured task demand.
Use the detailed memory roadmap for implementation ownership instead of
creating a second competing memory backlog.

Track verified task completion, unauthorized effects, lost/duplicate work,
p50/p95 time to first visible output and completion, peak RAM/VRAM, tokens,
operator interventions, and recovery success. Record failures and timeouts in
the denominator. Publish hardware/model/dataset identities and repeated-run
uncertainty. A faster run that silently drops work or weakens policy fails.

## Enhancements 21-50

21. [x] <a id="enhancement-021"></a> **Build a reproducible real-task harness benchmark.** **P0 / L.**
    Extend `crates/eval-harness/src/main.rs`, whose present rubric/mock mode
    does not run an agent task, with held-out coding, research, memory, and
    automation workloads. Score workspace tests, source-backed answers, and
    mock-sink effects; use local judges only for supplementary judgments.
    Persist dataset/model/backend/quantization/hardware identities, seeds,
    budgets, results, and failures. **Accept:** a clean offline installation
    can reproduce the scoring pipeline; repeated baseline/candidate runs
    report uncertainty and task success per hardware tier. Publish a small
    release suite and a larger periodic suite. This establishes the evidence
    needed for any "best harness" claim. The scorer, suite fixtures,
    offline fixture-validation mode, per-task records, and paired comparison
    are in `crates/eval-harness/` and `evals/benchmark/`. Offline fixture mode
    validates the scorer only. Live local-model and hardware-tier results
    remain to be collected before comparative capability claims.
    **Session progress (2026-09-29):** the offline scorer, benchmark task
    records, release/periodic fixtures, and paired comparison are implemented;
    only scorer/fixture validation is offline. Live local-model and
    hardware-tier runs remain open, so the item is checked for implementation
    while comparative benchmark qualification is pending.

22. [ ] <a id="enhancement-022"></a> **Make task completion an explicit, verifiable contract.** **P1 / M.**
    Build on `crates/core/src/runs.rs` and agent run history: persist user
    acceptance criteria, required artifacts, verifier results, and blocked or
    incomplete reasons. A successful model response or tool return must not
    automatically mean the user's task succeeded. Show a reviewable result
    with links to evidence and unfinished work. **Accept:** a task with a
    failing required test, missing artifact, or unconfirmed delivery cannot
    become verified-complete; a permitted partial result is labelled partial.
    Keep verification deterministic where possible, and avoid endless repair
    loops by using the run's remaining budget. **Session progress (2026-09-29):**
    chat, headless, and allowlisted editor clients submit required/optional
    criteria, artifacts, and delivery requirements. Durable executors persist
    these before inference; forks inherit requirements but reset evidence; the
    inspector links trace cursors, downloadable attachments, and HTTPS verifier
    reports, and distinguishes partial results from blocked required checks.
    Agent definitions now carry immutable criteria into each durable agent run;
    the Agents history can record Controller verifier, artifact, and delivery
    evidence. Forks retain requirements and reset evidence. Core tests (709),
    server tests (1,161), the focused evidence-review UI test, the full SPA
    suite (531), and TypeScript lint pass. Most criteria still rely on recorded
    verifier evidence rather than an autonomous deterministic verifier; broad
    real-task acceptance coverage remains. A later focused pass added an
    opt-in run-step JSON verifier: a required test criterion can compare a
    host-owned durable checkpoint field with an expected value, and manual
    evidence cannot override that result. The new core regression passed;
    Scheduled routines now accept completion contracts through API and editor;
    each fire freezes its contract before dispatch. A passing checkpoint or
    manually confirmed evidence cannot mark a still-running executor complete;
    failed and interrupted runs become blocked. Core, server-route, and
    routine-editor regressions pass. Scheduled agent runs can now verify exact
    fields in committed structured output without manual pass overrides; this
    checks output shape, not external task success. Automatic artifact/delivery
    proof and broader real-task coverage remain.
    **2026-09-30 follow-up:** a real TrueNAS-model task demonstrated required
    review, optional partial, missing-artifact blocking, and delivery-pending
    states. The core now rejects fabricated or stale produced attachments and
    rechecks file hashes, run scope, Controller attestations, and delivered
    outbox receipts when building reports. The prior fake-attachment result
    changed from verified-complete to Blocked; repeating it returns HTTP 400.
    Later disposable TrueNAS-model tasks reached VerifiedComplete through
    headless and editor-style API requests, a routine fire, and agent runs.
    A plugin tool and research job produced run-owned attachments with valid
    artifact and Controller review evidence. The actual headless CLI and VS
    Code editor adapter processes and a settled-build matrix remain unproved;
    H022 stays Partial. See [qualification notes](h022-h025-qualification.md).

23. [ ] <a id="enhancement-023"></a> **Unify production executor semantics and recovery.** **P0 / L.**
    `runner-local`, `runner-binary`, and `server/src/chats.rs` have overlapping
    loops with different retry, cancellation, and replay behavior. Define a
    shared transition contract for model rounds, tool attempts, approvals,
    completion, and restart. Resume completed checkpoints rather than
    rejecting or redispatching them; wire recovery discovery into startup.
    **Accept:** the same fixtures pass through streaming and in-process
    paths, routines, and child runs. A real process-kill matrix demonstrates
    recovery with paired events and no repeat of completed effects. This
    extends item 9 beyond store reopen tests; see F06-F08 and F11.
    **Session progress (2026-09-29):** startup advances completed checkpoints
    and empty terminal cursors without redispatch; the SPA and headless client
    can resume eligible non-transport runs using saved input and timezone.
    Startup explicitly classifies interrupted agent runs and requeues their
    unacknowledged mailbox inputs. The routine scheduler resumes pending fires
    under the same run id and synthetic conversation. Startup now dispatches
    eligible recovered chat runs automatically; the server suite passes,
    including host-kill recovery after a paired commit. Completed child joins
    now replay the persisted result without a second inference request
    (focused regression passes). Recovery from a kill during active child
    inference, cross-path transition parity, and the full process-kill matrix
    remain. A later process-kill regression covers an inference attempt held
    by a killed child process: the lease blocks premature re-entry, then the
    next worker records the interrupted attempt and reclaims it. Startup chat
    recovery now revisits inherited runs after lease expiry. The full path
    parity and production kill matrix are still open.
    **2026-09-30 follow-up:** an isolated Windows executable killed during
    held inference resumed the same durable run with one user event and one
    model reply. The exact TrueNAS model passed text, stream, tool, structured,
    and 16K effective context checks via native Ollama. Host and runner now
    carry its explicit context and thinking controls. A focused real
    child-process kill during inference now passes: recovery reclaims the
    interrupted model attempt once and publishes one artifact. Parent
    tool-pair process evidence, execution-path parity, and the kill matrix are
    still open; a runner-enabled isolated server cannot share this Docker
    daemon safely until orphan sweeping is installation-scoped. An isolated
    timed-out run also kept retrying startup recovery after its wall-clock
    budget expired. A later budget guard and disposable restart terminalized
    it once. Runner names are now database-scoped and a real runner turn
    passed, but a held runner kill did not recover before its budget expired
    during rebuilds. Prompt replay and spawn-gate changes need final-binary
    qualification, as do streaming/routine parity and the full kill matrix;
    H023 stays Partial. See [qualification notes](h022-h025-qualification.md).

24. [ ] <a id="enhancement-024"></a> **Add client request idempotency and explicit effect reconciliation.**
    **P0 / M.** Extend the chat API, `core/src/runs.rs`, and outbox contract
    with caller request IDs scoped to principal and conversation, canonical
    body hashes, and durable response lookup. Retries after a lost HTTP
    response must resolve to the same run; conflicting reuse must fail.
    Separate sink-supported idempotency, reconciliation, and unknown outcome.
    **Accept:** duplicate submissions across restart produce one run;
    send-before-ack failures are reconciled against a test sink. When a real
    sink lacks deduplication or status lookup, expose uncertainty and require
    an explicit retry decision instead of promising exactly-once delivery.
    **Session progress (2026-09-29):** scoped request reservations, durable
    response replay, body-conflict detection, and safe-runner resume are
    implemented. Ambiguous effects without sink guarantees park as unknown;
    Controller resolution requires evidence and is audited. The production
    relay handles plugin text-transport bridges and wakeups. Model-invoked text
    sends are staged until the tool-use/result commit, and ambiguous sink
    outcomes park without automatic replay. Attachment and research-PDF sends
    now enter the same outbox relay; tool attachment rows wait for the paired
    event commit and task bridges use stable scoped keys. Outbox (17),
    attachment relay (4), core pair-release, and server (1,161) tests pass,
    including post-acceptance kill/reopen and unknown-outcome cases. Full
    production process-kill qualification remains. A later real child-process
    kill regression shows a no-guarantee effect becomes `unknown` after the
    relay lease expires, with zero automatic redispatch. Installed-binary
    relay and transport kill qualification remains open.
    **2026-09-30 follow-up:** same-key HTTP retry after an isolated inference
    kill returned the original run. A copied Windows debug executable was
    killed after a disposable subprocess transport persisted its send and
    before acknowledgement. Restart parked the production transport outbox
    as unknown with one attempt and one sink effect. Lease reclaim now has a
    distinct timeline transition. Release-installed relay/transport and
    idempotent sink process-kill qualification remain open. A later Windows
    release build was blocked by Application Control 4551 on generated Cargo
    executables. A Docker SQLCipher release build reached dependency
    compilation but was stopped before an image was produced when host RAM
    became scarce. H024 stays Partial; see
    [qualification notes](h022-h025-qualification.md).

25. [ ] <a id="enhancement-025"></a> **Enforce capability-specific network egress at connection time.**
    **P0 / M.** Extend the existing `local-endpoint-policy` approach to
    `server/src/tool_apis_http.rs`, research, plugin HTTP, and MCP connection
    boundaries. Keep distinct policies for local inference, approved private
    integrations, and public web fetching. Resolve and validate all candidate
    addresses, pin the connection, normalize mapped IPs, and validate every
    redirect before connecting. **Accept:** DNS rebinding and private-hop
    fixtures receive zero prohibited requests; an explicitly authorized
    local sidecar still works. Apply policy to proxies too. This closes F03
    without banning legitimate local services or internet transport plugins.
    **Session progress (2026-09-29):** guarded clients pin approved addresses
    across web, research, plugin, sidecar, inference, and automation paths;
    redirects, mapped addresses, cross-origin credentials, and ambient proxies
    receive explicit handling. Script-plugin HTTP refuses automatic redirects so
    authorization headers cannot follow a different origin without a new policy
    decision. OAuth token and userinfo clients disable proxies and redirects and
    fail closed if secure client construction fails. Endpoint-policy tests pass
    (8), covering mixed DNS answers, mapped addresses, redirect blocking, and
    pinned resolution; server tests pass (1,161), including guarded HTTP
    requests. Full adversarial DNS/rebinding, redirect, proxy, and
    supported-endpoint qualification remains. A later adversarial pass
    prevents caller client-builder options from restoring proxies or redirect
    following after policy validation; a live loopback test and public/private
    mixed-DNS and mapped-IP fixtures pass (11 policy tests). Cross-adapter
    supported-endpoint and DNS/proxy qualification remains open. OAuth token
    and userinfo requests now create a fresh public-policy client at request
    time, and a plugin-HTTP mixed-DNS fixture checks approved private access
    and denied public/metadata/loopback answers. That fixture exposed a
    plugin-HTTP bypass: an approved DNS name resolving only to loopback was
    classified as approved DNS; the adapter now rejects loopback and mapped
    loopback before either public or private policy selection.
    **2026-09-30 follow-up:** the approved TrueNAS inference endpoint passed
    exact-model qualification; removing its LocalInference CIDR returned 503,
    and a PrivateIntegration-only grant did not authorize inference. A
    sidecar-HTTP redirect fixture now proves another listener receives zero
    requests. Later live public OpenMeteo and research fetches succeeded;
    MCP private approval, denial, and redirect probes left the forbidden
    listener at zero requests. Focused public automation and Google OAuth
    policy checks passed. The full final-tree adversarial script,
    authenticated external endpoint/SPA checks, and cross-adapter DNS,
    rebinding, and proxy matrix remain open. H025 stays Partial; see
    [qualification notes](h022-h025-qualification.md).

26. [x] <a id="enhancement-026"></a> **Make authorization and session revocation systematic.** **P0 / M.**
    Centralize protected admin-route construction in `server/src/routes.rs`
    and require explicit public-route exceptions. Add a generated route/role
    matrix covering plugin lifecycle, settings, media, and WebSocket paths.
    Persist revocable session identities or authentication epochs; revoke
    refresh credentials on password changes and support logout-all.
    **Accept:** anonymous and lower-role requests cannot mutate Controller
    state; revoked access/refresh tokens and active connections lose authority
    across restart. Show operators the principal, scope, reason, and exact
    action for approval requests. F01 and F05 are release blockers.

27. [x] <a id="enhancement-027"></a> **Make plugin upgrades transactional and panel authority explicit.**
    **P0 / L.** Extend existing manifest/schema/provenance validation with
    safe version components, canonical destination containment, bounded ZIP
    expansion, and isolated staging. Verify before replacing any live bytes;
    retain an atomic rollback path. Move optional UI panels behind a
    sandboxed origin/frame and manifest-scoped host RPC instead of exposing
    the operator token. **Accept:** failed validation/upgrade/restart leaves
    the previous plugin usable; malicious versions cannot escape staging;
    a test panel cannot read credentials or call unrelated admin routes.
    Publisher signatures remain necessary but do not grant unrestricted UI
    authority. See F02 and F14. **Session progress (2026-09-29):** safe version
    and panel paths, bounded ZIP expansion, canonical staging containment,
    isolated candidate directories, and rollback after candidate failure are
    implemented. Panels use opaque-origin frames and manifest-scoped RPC
    without the operator token. Focused browser and rejected-upgrade regression
    checks pass; cross-platform release qualification remains.

28. [x] <a id="enhancement-028"></a> **Qualify the actual production release artifacts.** **P0 / M.**
    Fix SQLCipher feature selection in all three `scripts/build-*` paths.
    Add packaged-binary checks for cipher availability, encrypted headers,
    wrong-key rejection, migration, backup restoration, and key rotation.
    Extend existing provenance with verifiable platform signing where
    applicable and an offline update/rollback bundle. **Accept:** each OS
    artifact passes its own installation and encryption smoke test; a keyed
    production startup refuses plaintext SQLite. Upgrade failure preserves
    the prior recoverable database. Do not downgrade a migrated schema by
    merely replacing the binary. Builds and tests must verify F04 is closed.
    **Session progress (2026-09-29):** Linux, macOS, and Windows build scripts
    explicitly enable SQLCipher; installed-binary `doctor` now performs an
    encrypted backup/restore and disposable key-rekey/recovery drill in addition
    to encrypted-header, wrong-key, migration, and reopen checks. Bundle
    workflows attach OIDC Sigstore provenance bundles to each installer and
    create signed offline update archives. The archive includes the previous
    published installer when available, a self-contained verifier, and explicit
    pre-upgrade database backup/restore instructions that prohibit binary-only
    schema downgrade. Native runner executions, platform release qualification,
    and actual upgrade/rollback runs are deferred to the later verification pass.

29. [x] <a id="enhancement-029"></a> **Treat streaming as a tested protocol state machine.** **P0 / M.**
    Replace ad hoc framing in `inference-api/src/lib.rs` and reuse bounded
    framing primitives where appropriate in `server/src/mcp_http_client.rs`.
    Buffer bytes across UTF-8 boundaries, support legal SSE line endings,
    validate protocol-specific terminal states, and cap frames/tool deltas.
    Preserve typed partial-output errors and retry only before safe replay
    boundaries. **Accept:** every byte split of multilingual text and tool
    arguments yields identical results; premature EOF never marks success or
    dispatches incomplete calls; a matching MCP response returns without
    waiting for the connection to close. Completes item 5's streaming work.
    **Session progress (2026-09-29):** SSE and Ollama NDJSON framing are
    incremental and bounded, terminal frames are required, and split-offset
    fixtures cover multilingual/tool payloads. Truncated streams preserve
    emitted content, return typed `IncompleteStream`, and do not replay the
    request. The runner sends a typed stream failure with partial output; if the
    runner process dies, the host records accumulated visible deltas as an
    incomplete error `model_turn` and terminalizes the durable run as failed.
    No model checkpoint or tool request is emitted before the terminal marker.
    Follow-up tests passed for inference API (36), runner binary (13),
    runner-local (23), protocol (10), and one focused host process-kill
    recovery case. The full stream process-kill matrix remains open.

30. [x] <a id="enhancement-030"></a> **Schedule local inference with fair hierarchical budgets.**
    **P1 / L.** Resource admission in item 19 prevents some overcommit but
    does not establish fairness between chat, research, voice, and agents.
    Extend `server/src/inference_resolver.rs` and `agent_supervisor.rs` with
    bounded queues, workload priorities, aging, per-model concurrency, and
    parent/child token, time, retry, and effect budgets. Reserve foreground
    capacity without starving background jobs. **Accept:** a research burst
    cannot exhaust interactive slots; children cannot multiply a parent's
    budget; cancellation releases reservations and restart reconstructs
    outstanding accounting. Benchmark queue delay on each supported tier.
    **Session progress (2026-09-29):** bounded global/per-model and background
    admission, queue limits/aging, and durable parent-scoped child token
    reservations are implemented. Durable run records now enforce wall-clock,
    retry, and effect quotas. In-process and runner-stream retries debit the
    same run counter; the host cancels a runner if it cannot persist a retry
    reservation. Child reservations atomically cover tokens, time, retries, and
    effects, settle measured usage after execution, and bind admission to the
    durable parent; delegated children have no tool/effect authority and a
    bounded inference deadline. Runner containers receive a memory cap and are
    admitted under a serialized host-RAM reservation ceiling rebuilt from active
    container labels. Follow-up tests passed for the 709-test core suite, 23
    runner-local tests, and 18 server inference-resolver/admission tests,
    including child reservation/reopen and cancellation cleanup. Hardware-tier
    queue-delay measurement and time/retry/effect restart qualification remain.

31. [x] <a id="enhancement-031"></a> **Compile context against the full budget on every model round.**
    **P1 / M.** Extend `context-window`, `core/src/history_budget.rs`, and
    both executor loops beyond the current estimates and initial trimming.
    Include system instructions, tools, summaries, arguments, tool results,
    images, and output reserve. Use qualified local tokenizer support where
    available and conservative measured fallbacks otherwise. Offload large
    results to scoped artifacts with bounded retrieval. **Accept:** actual
    requests fit the qualified context limit after every tool round; Unicode,
    long JSON, images, and oversized summaries are covered; paired calls and
    mandatory user constraints survive. See F12.

32. [x] <a id="enhancement-032"></a> **Give compaction a provenance and quality contract.** **P1 / M.**
    Build on existing history summaries and input fingerprints with a compact
    record of retained constraints, pending work, source event ranges,
    discarded content, and summary version. Summaries must retain source
    trust and must not turn untrusted instructions into trusted policy.
    Allow authorized retrieval of original evidence without replaying all
    history. **Accept:** multi-compaction task suites preserve acceptance
    criteria and unresolved approvals; stale summaries invalidate on source
    changes; malicious text cannot gain authority through summarization.
    Compare task success and prompt size against today's history policies.

33. [x] <a id="enhancement-033"></a> **Add on-demand tool discovery and progressive schema loading.**
    **P1 / M.** Extend `server/src/chats.rs::build_runner_tool_catalog` beyond
    item 7's declaration cap. Advertise concise authorized capabilities and
    load exact schemas only when selected; rank by task relevance with a
    deterministic fallback. Keep the final dispatch policy authoritative,
    pin schema versions per run, and separate discovery from execution.
    **Accept:** a large synthetic plugin/MCP catalog has bounded initial
    context cost while useful tools remain discoverable; disabled,
    lower-trust, and removed tools never become callable through search;
    task success does not regress against the full-catalog baseline.

34. [x] <a id="enhancement-034"></a> **Qualify model-specific capability profiles and structured outputs.**
    **P1 / M.** Extend backend probes and `model-adapter` using exact model,
    quantization, chat-template, backend-version, and parser identities.
    Persist observed tool/schema/vision/context behavior separately from
    declared support. Select native structured decoding only when qualified;
    retain host-side validation and bounded correction. **Accept:** routing
    refuses incompatible tasks before execution, model/template changes
    invalidate stale qualifications, and a held-out matrix reports executable
    call rates and latency. Compare approved local alternatives on measured
    task success; never fail over to a cloud provider.

35. [x] <a id="enhancement-035"></a> **Provide a durable execution inspector with reconnectable traces.**
    **P1 / M.** Build on events, run steps, item 11's delivery timeline, and
    item 18's aggregate metrics. Correlate model rounds, tool retries,
    approvals, child runs, queue waits, and delivery receipts using durable
    IDs and resumable cursors. Offer optional local trace export with
    metadata-only defaults. **Accept:** after a UI disconnect/restart, the
    inspector identifies gaps and reloads authoritative state; operators can
    distinguish model delay, a stalled tool, and uncertain delivery. No
    prompt, credential, or raw tool-output content enters default logs.

36. [ ] <a id="enhancement-036"></a> **Turn failures into consented, replayable regression fixtures.**
    **P1 / M.** Connect `core/src/eval.rs` flagged ranges to the evaluation
    harness. Export selected trajectories with source provenance, redaction,
    expected state transitions, and mock tool responses; default to effects
    disabled. Include synthetic replacements for secrets and personal data.
    **Accept:** an operator can reproduce a catalog, policy, framing, or
    recovery failure offline and verify its fix in CI; fixture export never
    sends private transcripts to a service. Link the resulting regression
    artifact to the incident and release so a previously fixed failure cannot
    silently recur. **Session progress (2026-09-29):** explicit-consent export
    binds flagged ranges to source hashes, applies local redaction and synthetic
    identities, preserves transitions/mock results, and disables effects.
    Fixture validation checks provenance hash shape, bounded metadata and
    payload sizes, unique mock responses, tool pairing, and disabled effects.
    Production executor replay and fix-to-release regression gating remain.
    **Session progress (2026-09-30):** the harness now replays every regular
    JSON fixture in a directory, records each fixture hash and validation
    result, and the Rust CI job uses this catalog command. Fixture replay now
    drives the production `TurnExecutor` against a loopback scripted local
    OpenAI-compatible endpoint, dispatches only fixture mocks, and compares
    emitted transitions and payloads with the recorded trajectory. Catalog
    replay requires incident and release references. Unsupported event kinds,
    attached media, catalog/policy-only trajectories, and raw streaming-frame
    replay are still outside the production-executor fixture contract; the
    broader offline replay and release qualification gate remains open.

37. [ ] <a id="enhancement-037"></a> **Make memory evidence inspectable and correctable.** **P1 / M.**
    Extend `core/src/memory_assertions.rs`, `memory_assets.rs`, and the memory
    admin UI rather than creating another store. Show source events/spans,
    direct versus inferred assertions, validity windows, revisions, approval,
    and exactly why an asset entered a turn. Allow Controller correction,
    retraction, and safe export. **Accept:** each injected fact can be traced
    to authorized evidence; retraction invalidates derived loadouts and
    summaries; an outdated conflicting fact is not silently presented as
    current. Keep lifecycle/loadout implementation under the memory roadmap.
    **Session progress (2026-09-29):** prompt assembly persists metadata-only
    per-turn HOT-loadout receipts; the run inspector shows scope, trust,
    binding/source hashes, injected length, and selection reasons. A retraction
    regression verifies deleted assets are omitted on a later turn. Agent-run
    history now records the metadata-only HOT-loadout receipt in each run
    checkpoint. The legacy unreceipted HOT key/value prompt block is no longer
    injected; governed asset loadouts remain trust filtered and receipted.
    Agent-run restart/invalidation coverage and full retraction qualification
    across every derived projection remain open.

38. [ ] <a id="enhancement-038"></a> **Qualify trust-first hybrid and temporal retrieval end to end.**
    **P1 / L.** `core/src/memory_assets.rs` already implements lexical/vector
    fusion; the enhancement is a measured full retrieval path. Apply scope,
    trust, lifecycle, and time eligibility before candidate ranking; add
    versioned local embeddings/reranking, fresh-index checks, and evidence
    deduplication. **Accept:** held-out recall and answer accuracy improve
    over lexical search under a fixed latency budget, while forbidden assets
    cannot displace authorized candidates or enter model context. Rebuild
    after an embedding-model change without making SQLite facts dependent
    on a derived index. **Session progress (2026-09-29):** eligible search now
    applies scope, trust, lifecycle, owner visibility, time, and injection-mode
    filters before ranking; prompt consumers deduplicate source hashes and
    record query/rank/source receipts. TOOL_ONLY assets stay out of prompt
    context. Versioned embedding/reranking and rebuild, research/agent
    consumers, held-out quality, and hardware latency gates remain. Embedding
    writes now require a bounded model identity, finite vector values, and a
    source hash matching the current asset; the legacy vector path also filters
    stale and inactive rows. Settings now configure a local embedding model and
    rebuild a versioned index; chat, durable-run, agent, and research prompt
    paths consume eligible assets and deduplicate source hashes. Held-out
    recall, answer-accuracy comparison, fixed-budget hardware latency, and live
    local-backend evidence remain open. The `eval-harness
    qualify-memory-retrieval` command now builds a held-out versioned index,
    compares lexical/hybrid recall and local answers, tests forbidden/expired/
    archived/stale-vector candidates, and records build time plus retrieval
    p50/p95 against a fixed budget. Its current report records `http_connect`
    because the configured local inference service is unavailable; H038 remains
    unqualified.

39. [ ] <a id="enhancement-039"></a> **Evaluate learned skills by execution and support safe rollback.**
    **P1 / L.** Extend item 15 and `server/src/skills_admin.rs` beyond
    tools-disabled substring evaluation. Run procedural skills on isolated
    workspaces/mock integrations; assert outputs, forbidden actions, and
    resource budgets. Compare candidate and parent versions on identical
    held-out cases, then require governed promotion and retain rollback.
    **Accept:** merely mentioning expected words cannot pass a failed task;
    a skill that improves one case but leaks a secret or breaks an existing
    case cannot promote. Report real task outcomes rather than claiming
    autonomous learning gains from prose scores. **Session progress
    (2026-09-30):** evaluation suites now support forbidden-output checks and
    per-case output/token bounds; suite hashes include these assertions and a
    violation fails promotion scoring. Evaluations now run skills through a
    bounded local tool loop in a temporary workspace with deterministic mock
    integrations, exact workspace/call assertions, forbidden-action checks,
    and paired parent/candidate results. Denied actions and cumulative output
    tokens fail a case. Full live-model held-out qualification and promotion
    evidence remain open.

40. [ ] <a id="enhancement-040"></a> **Ship a complete workspace coding plugin.** **P1 / L.**
    Implement the existing strategy's coding-workspace proposal through
    `plugin-sdk`, `plugin-host`, and the container manager: root-scoped file
    reads, search, patching, terminal jobs, and language-server diagnostics.
    Make filesystem/network/process capabilities explicit and route effectful
    operations through durable host dispatch. **Accept:** the benchmark can
    solve multi-file repairs with test evidence; traversal, symlink/junction
    escapes, secret-file reads, and unapproved network access fail closed.
    Plugins may supply language support; core must not hardcode individual
    plugin identities. Depend on 23-29 before granting write authority.
    **Session progress (2026-09-30):** a Controller-only run-checkout patch
    route now applies bounded UTF-8 replacements after checking each file's
    expected SHA-256. It uses the isolated checkout and leaves the registered
    workspace root for the existing reviewed diff/apply flow. Manifest-declared
    read/search/patch/terminal/diagnostics tools now use durable run-scoped host
    dispatch. Terminal and generic LSP jobs mount only a secret-filtered
    read-only snapshot, use a digest-pinned Controller-approved image, disable
    networking, drop capabilities, and enforce CPU/memory/PID/output/time
    limits. A digest-pinned Rust/TypeScript toolchain image is built locally;
    Controller config requires verified provenance or explicit approval of the
    exact image digest. A disposable Rust crate passed `cargo test --offline`
    through the manager. The Docker smoke confirmed blocked networking, a
    read-only workspace mount, and Rust Analyzer diagnostics. The Rust-side LSP
    late-push regression and server route tests compile but have not executed
    because the Windows test binary is blocked by Application Control 4551 and
    WSL terminates during builds. The held-out multi-file model-repair benchmark
    and H023-H029 write-authority prerequisites remain open, so H040 is not
    complete.

41. [x] <a id="enhancement-041"></a> **Add workspace checkpoints, run forks, and safe diff application.**
    **P1 / L.** Extend the coding plugin and run store with isolated working
    copies, content-addressed snapshots, and branch/run lineage. Record which
    files a checkpoint covers, detect concurrent human edits, and preview
    conflicts before applying or restoring. A conversation fork gets fresh
    effect IDs and no reusable pending approvals. **Accept:** two runs cannot
    overwrite one another or the operator's work; restore touches only owned
    changes; external sends are never described as undone. Commits, pushes,
    and publication remain explicit operator decisions.

42. [x] <a id="enhancement-042"></a> **Make child-agent work a durable, inspectable run tree.** **P1 / L.**
    Extend existing agent definitions/mailboxes, `server/src/tool_apis_subagent.rs`,
    and run-step kinds with durable spawn/join, typed task/result contracts,
    artifact handoff, inherited trust ceilings, and aggregate budgets. Show
    dependencies, progress, approvals, and errors in the Agents UI.
    **Accept:** restart while waiting for children resumes the same joins;
    cancelled parents stop eligible descendants; no child gains authority
    through another child's text; conflicting workspace edits require
    resolution. Benchmark delegation overhead so simple tasks remain single
    agent by default.

43. [ ] <a id="enhancement-043"></a> **Support durable steering, queued messages, and useful stop controls.**
    **P1 / M.** Extend chat events, the composer, and runner protocol with
    distinct queue-next-turn, steer-at-safe-boundary, pause, and cancel
    operations. Persist user intent and acknowledgments so reconnection does
    not lose a correction. Wake blocked model/tool waits on cancellation and
    label already-dispatched effects accurately. **Accept:** a correction
    survives restart and applies once at the intended boundary; stop is
    acknowledged within a defined tested latency even when inference or a
    tool stalls. Never convert elapsed silence into approval.

44. [x] <a id="enhancement-044"></a> **Expose a stable headless, terminal, and editor client contract.**
    **P2 / L.** Build versioned clients from `spec/` and the server API with
    typed sessions, event cursors, artifacts, approvals, and cancellation.
    Add a thin terminal client and an optional editor protocol adapter that
    maps to existing host permissions; keep `runner-protocol` internal.
    **Accept:** the same task can move between SPA and client without losing
    state or duplicating effects; contract tests catch schema drift and
    older-client incompatibility. This provides Pi/TrueForge/OpenHands-style
    integration surfaces while retaining one local authority and SQLite
    configuration.

45. [x] <a id="enhancement-045"></a> **Complete negotiated, bounded MCP interoperability.** **P1 / M.**
    Extend the stdio client and `server/src/mcp_http_client.rs` with verified
    version negotiation, session headers/lifecycle, bounded streaming,
    response-ID validation, cancellation, and capability discovery. Gate any
    new tasks/resources/prompts/elicitation support on its negotiated contract;
    never treat server annotations as authority. **Accept:** pinned reference
    fixtures cover both transports, restart, session expiry, wrong IDs,
    oversized bodies, and a matching response on a still-open SSE connection.
    Use the [MCP transport specification](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)
    as the current client's baseline, not an unverified "latest" assumption.

46. [ ] <a id="enhancement-046"></a> **Make research and browser results evidence-verifiable.** **P1 / M.**
    Extend `server/src/research/` and the browser plugin with bounded local
    source snapshots, retrieval times, content hashes, and stable source IDs.
    Validate citations against fetched sources and expose unsupported claims,
    stale pages, extraction failures, and contradictory evidence. Browser
    actions must use the same trust/egress/effect controls as other tools.
    **Accept:** an invented URL cannot pass as a fetched citation; a report
    retains inspectable evidence after a page changes; held-out research
    tasks score supported claims, not citation-shaped text. Cite private
    sources only within their authorized scope.

47. [ ] <a id="enhancement-047"></a> **Deliver cancellable, low-latency local voice sessions.** **P2 / L.**
    Build on `voice-pipeline` and `server/src/voice_runtime.rs`, whose current
    route is push-to-talk, with continuous endpointing, bounded audio queues,
    incremental transcription where supported, and sentence-streamed TTS.
    Remove network awaits under the shared session lock and propagate barge-in
    through playback, inference, and pending tools. **Accept:** slow STT in
    one session cannot block another's interrupt; measured noise, silence,
    reconnect, and barge-in suites pass on supported hardware. Spoken output
    must not claim an external action succeeded before its durable result.

48. [ ] <a id="enhancement-048"></a> **Make privacy retention and deletion cover every projection.**
    **P1 / M.** Extend current incognito/search deletion and backup controls
    to memory evidence, vectors, skills, research artifacts, exports,
    Obsidian/Graphify projections, diagnostic captures, and plugin storage.
    Track derivation and deletion jobs in SQLite; distinguish live deletion
    from separately retained backups. **Accept:** seeded private content is
    absent from every searchable/live projection after deletion, queued jobs
    cannot reintroduce it, and a restore reapplies applicable tombstones.
    Make retention and export consent visible. Do not promise physical
    erasure from immutable backups or storage snapshots without a supported
    mechanism.

49. [ ] <a id="enhancement-049"></a> **Enforce performance budgets and benchmark local optimizations.**
    **P1 / M.** Connect existing Criterion benchmarks to baseline comparison
    in stable-hardware CI. Cover replay, search, catalog assembly, framing,
    queue wait, and large artifacts, with noise tolerances and stored results.
    Separately trial prefix reuse, model residency, batching, and speculative
    decoding only on qualified local backends. **Accept:** a deliberate
    regression fails the gate; improvements publish numbers with identical
    task-quality and policy checks. Never enable an optimization because its
    upstream throughput claim looks attractive. Item 18's process-local p95
    alert remains useful operationally but is not this release gate.

50. [ ] <a id="enhancement-050"></a> **Make first success and recovery accessible and diagnosable.**
    **P1 / M.** Extend setup/doctor/backends UI with protocol qualification,
    measured hardware suitability, encryption status, plugin authority,
    recovery state, and a scrubbed support bundle. Add real-browser journeys
    against a disposable backend for setup, approvals, streaming, reconnect,
    and failed-upgrade recovery, including keyboard and screen-reader use.
    **Accept:** users complete these flows without reading server logs;
    errors name a specific corrective action; diagnostics reveal no tokens
    or prompts. Measure time to the first verified task on each OS and
    prevent accessibility and recovery regressions in CI.

## Extended enhancement portfolio 51-130

Added 2026-09-27. These 80 additional proposals extend the first 50; they are
not assertions that every underlying primitive is absent. Each describes a
separately testable deliverable. Referenced earlier items are prerequisites or
parent initiatives, not work to implement twice. P0/P1/P2 and S/M/L retain the
definitions above; **Lab** means a bounded experiment with an explicit
adoption gate. All 130 items are accepted scope, delivered in bounded increments.
Keep only a few initiatives active. Lab items require implementing and
evaluating the specified trial; production activation still requires its gate.
A failed trial is blocked pending redesign or an explicit operator scope
decision, not silently dropped from the plan.

| Range | Area | Existing foundations / main implementation owners |
|---|---|---|
| 51-57 | Policy and authority | `crates/policy/`, server dispatch/approvals, vault |
| 58-64 | Durable storage | `crates/core/src/db.rs`, events, migrations, projections |
| 65-71 | Effects and workflow semantics | Plugin manifests, tool execution, outbox, routines |
| 72-78 | Isolation and local resources | Container manager, runner/sidecar supervision, vault |
| 79-85 | Plugin and integration ecosystem | Plugin SDK/host, provenance, Rhai, runner protocol |
| 86-92 | Context and knowledge | Context window, memory assertions/assets, Graphify |
| 93-99 | Coding execution | Workspace plugin proposed in 40, runner, browser sidecar |
| 100-106 | Messaging and multimodal work | Transport registry, principal groups, attachments |
| 107-113 | Evaluation and agent judgment | Eval harness, policy tests, run contracts |
| 114-120 | Operations and maintainability | Build scripts, CLI/service, observability, config |
| 121-130 | Documents and automation UX | Attachments, research/Python, automations, SPA |

### Policy and authority

51. [ ] <a id="enhancement-051"></a> **Carry typed sensitivity and provenance through the entire run.**
    **P1 / L; extends 8/32/48.** Associate observations, artifacts, memories,
    and child results with source, owner, trust, and permitted destination
    labels. Propagate labels through transformations; changing data's format
    must not change its authority. Keep labels outside model-editable text.
    **Accept:** tests trace sensitive material through a summary, skill,
    artifact, and child run; downstream policy still prevents an unauthorized
    export. Explicit authorized declassification records its actor and scope.

52. [ ] <a id="enhancement-052"></a> **Recheck live authority immediately before dispatch.** **P0 / M;
    extends 10/26.** Bind decisions to a policy revision and scoped grant,
    then revalidate principal status, grant revocation, expiry, and target at
    the effect boundary. Catalog construction and earlier approval cannot
    freeze permission indefinitely. **Accept:** removing permission while a
    tool waits in a queue prevents its later execution; replay cannot revive
    revoked grants. Already-accepted external actions remain explicitly
    separate from cancellable pending work.

53. [ ] <a id="enhancement-053"></a> **Render approvals from canonical typed actions.** **P1 / M;
    extends 10/22.** Generate recipient, target, operation, changed fields,
    reversibility, and approval scope from validated arguments, not solely
    an LLM-written explanation. Provide before/after previews when available
    and distinguish approval of one action from a bounded standing grant.
    **Accept:** misleading model prose cannot conceal a different target or
    operation; changed canonical arguments invalidate approval; keyboard and
    screen-reader users can inspect the same consequential fields.

54. [ ] <a id="enhancement-054"></a> **Broker secrets without placing credentials in model context.**
    **P1 / L; extends 25/27.** Give tools scoped credential references that
    the host resolves only for an authorized account, destination, method,
    and lifetime. Avoid passing general vault access to a plugin that only
    needs one authenticated request. **Accept:** a plugin cannot use a token
    reference against another service or account; request failures and tool
    results do not reveal credential values. Record secret-use metadata and
    revoke outstanding references on credential rotation.

55. [ ] <a id="enhancement-055"></a> **Check outbound data at the actual delivery boundary.** **P1 / M;
    depends on 51/54.** Combine deterministic secret detection with sensitivity
    labels and the authorized recipient set before messages, attachments,
    browser submissions, or HTTP bodies leave the host. Define explicit
    exceptions for operator-authorized exports. **Accept:** synthetic secrets
    inserted into model output, files, and plugin results are caught at each
    sink; legitimate approved sharing remains possible. Report both missed
    detections and false blocks; scanning is defense in depth, not proof of
    complete data-loss prevention.

56. [ ] <a id="enhancement-056"></a> **Offer task-scoped safety profiles with visible enforcement.**
    **P1 / M; extends 26/40.** Define inspect-only, workspace-edit, and
    approved-integration profiles as SQLite-backed capability sets, not
    prompt instructions. Show effective filesystem, process, network, secret,
    and destination permissions before starting a run. **Accept:** selecting
    inspect-only makes write attempts fail at host/OS boundaries; importing
    a skill cannot broaden a profile. Unsupported enforcement on a platform
    is visible and prevents use of a profile claiming that guarantee.

57. [ ] <a id="enhancement-057"></a> **Simulate policy changes before enabling them.** **P1 / M;
    extends 17/26.** Replay saved, authorized decision metadata through a
    candidate policy without executing tools. Display newly allowed, newly
    denied, and newly approval-gated actions, with rule explanations and a
    policy-revision rollback. **Accept:** a proposed trust-floor change shows
    its effects on fixtures across every trust class; simulation performs no
    external action and cannot overwrite historical decisions. Reverting
    policy must not resurrect expired approval tokens.

### Durable storage

58. [ ] <a id="enhancement-058"></a> **Define and test power-loss durability separately from crash recovery.**
    **P0 / M; extends 9/23.** `core/src/db.rs` currently selects WAL with
    `synchronous=NORMAL`; specify the persistence boundary required before
    effects are eligible for dispatch. Evaluate `FULL` or a documented
    equivalent for effect-critical commits and quantify the latency cost.
    **Accept:** storage fault tests distinguish process termination from
    hard-reset loss, and no durability promise exceeds the tested storage
    contract. SQLite documents the distinction in its
    [WAL durability discussion](https://sqlite.org/wal.html). See F17.

59. [ ] <a id="enhancement-059"></a> **Move blocking database work behind a bounded execution service.**
    **P1 / L; extends 30/49.** The current `Database` wraps one synchronous
    connection mutex. Introduce measured queueing, short write transactions,
    and bounded read execution while preserving ordering and SQLCipher
    initialization. Add a reader pool only if benchmarks justify it.
    **Accept:** a long search or export cannot stall streaming/approval
    handling on async workers; queue saturation produces typed backpressure
    rather than unbounded tasks. Test transactional consistency under load.

60. [ ] <a id="enhancement-060"></a> **Manage disk pressure and WAL growth as first-class health states.**
    **P1 / M; extends 49/50.** Track database/WAL/blob sizes, checkpoint
    progress, transaction duration, and free-space reserves. Throttle optional
    indexing/downloads before critical state writes fail; perform bounded
    maintenance without dropping durable work. **Accept:** a long reader,
    disk-full injection, and stalled checkpoint produce actionable states;
    cleanup respects retention and artifact references. Recovery after space
    is restored preserves event integrity and pending effects.

61. [ ] <a id="enhancement-061"></a> **Version event payloads and replay transformations explicitly.**
    **P1 / M; extends 16/23.** Add explicit payload schema identities and
    deterministic adapters for historical events. Verify original signed
    bytes before deriving a newer in-memory representation; never rewrite
    history just to fit a new struct. **Accept:** fixtures from supported
    releases reconstruct equivalent state, unknown required semantics fail
    clearly, and replay requires neither network nor model calls. Document
    which reader versions can understand each event generation.

62. [ ] <a id="enhancement-062"></a> **Rebuild derived projections safely while the service runs.**
    **P1 / L; extends 12/38.** Provide resumable, versioned rebuilds for
    search, archive, memory, and graph projections using authoritative SQLite
    records plus applicable verified events, watermarks, and atomic activation.
    Memory assertions are not assumed reconstructible from events alone.
    Validate a replacement before exposing it. **Accept:** interruption
    resumes bounded work;
    old/new projections agree on a reference corpus; readers never observe
    a partially populated index as complete. Live events and deletion
    tombstones arriving during rebuild cannot be lost or resurrected.

63. [ ] <a id="enhancement-063"></a> **Detect rollback of an otherwise valid database snapshot.**
    **P2 / L; extends 20.** Offer operator-controlled export of signed
    conversation heads or checkpoint roots to an independently retained
    local/offline record. A valid old database plus its old internal head
    cannot establish freshness by itself. **Accept:** restoring an older
    snapshot is reported relative to the independent reference; legitimate
    recovery requires an explicit reconciliation step. State the trust
    assumption: if an attacker can replace both copies or steal all keys,
    this mechanism cannot establish independent freshness.

64. [ ] <a id="enhancement-064"></a> **Test schema evolution across supported release histories.**
    **P1 / M; extends 28.** Build fixture databases from released schema
    versions, including realistic large tables, legacy event formats, and
    interrupted migrations. Measure upgrade duration and disk requirements;
    use resumable backfills where atomic migration would block too long.
    **Accept:** supported upgrade paths preserve keys, events, projections,
    and configuration; failures leave a recoverable snapshot. Append new
    migrations, preserve shipped migration history, and refuse unsupported
    downgrades rather than attempting an improvised reverse migration.

### Effects and workflow semantics

65. [ ] <a id="enhancement-065"></a> **Describe tool effects and concurrency semantics in manifests.**
    **P1 / M; extends 6/24.** Extend tool contracts with declared read/write
    resources, external effect class, idempotency/reconciliation support,
    cancellation semantics, and sensitivity. Use conservative host defaults
    and operator policy; a plugin's self-description is not a security grant.
    **Accept:** unknown effect semantics disable automatic effect retries
    and parallel writes; conformance fixtures identify falsely declared
    behavior. All runtime tiers expose the same normalized contract.

66. [ ] <a id="enhancement-066"></a> **Support prepare/preview/execute with resource preconditions.**
    **P1 / L; depends on 53/65.** For participating tools, prepare a bounded
    proposal tied to resource versions, then execute only if the approved
    versions still match. Show stale previews and recompute rather than
    silently applying changed meaning. **Accept:** modifying a file, draft,
    record, or recipient between preview and execution causes a conflict;
    preview itself has no external effect. Integrations lacking conditional
    updates must disclose that limitation and use conservative handling.

67. [ ] <a id="enhancement-067"></a> **Model compensating actions for partially completed workflows.**
    **P2 / L; depends on 24/65.** Let workflows declare separate, authorized
    compensation steps where a service supports reversal. Preserve the
    original effects, compensation attempts, and residual consequences in
    history. **Accept:** failure after several steps yields an accurate
    partial-completion report; recovery never blindly repeats compensation.
    An irreversible message or third-party operation is not labelled undone.
    Compensation with new consequences needs its own applicable approval.

68. [ ] <a id="enhancement-068"></a> **Parallelize only independent tool work.** **P1 / M;
    depends on 30/65.** Build a bounded scheduler from declared read/write
    sets and task dependencies. Overlap safe reads while serializing
    conflicting mutations and preserving model-visible call/result pairing.
    Default unknown tools to sequential execution. **Accept:** independent
    slow reads improve measured completion time; same-resource writes retain
    deterministic ordering; one failed/cancelled child does not orphan the
    other results or release effects outside the parent budget.

69. [ ] <a id="enhancement-069"></a> **Specify timer behavior across clock changes and restart.**
    **P1 / M; extends 23/30.** Distinguish persisted UTC deadlines from
    monotonic elapsed-time accounting. Define lease, approval-expiry, retry,
    and timeout behavior for sleep/resume, clock rollback, and large forward
    jumps. Inject clocks into timing-sensitive code. **Accept:** deterministic
    tests cannot extend an expired grant by changing wall time or create
    immediate retry storms after wake; restart reconstructs deadlines without
    granting additional run budget. Fail closed when rollback makes expiry
    unverifiable after restart; a monotonic clock alone cannot prove continuity.

70. [ ] <a id="enhancement-070"></a> **Give routines explicit missed-run and overlap policies.**
    **P1 / M; extends 30.** Extend `core/src/routines.rs` and
    `server/src/routine_runner.rs` beyond timezone-aware cron selection with
    skip, coalesce, or bounded catch-up behavior and forbid/queue/replace
    overlap modes. Preview upcoming occurrences in the operator's timezone.
    **Accept:** daylight-saving folds/gaps, extended downtime, long previous
    runs, and edits near a deadline have deterministic outcomes; unique
    occurrence IDs prevent duplicate local run creation.

71. [ ] <a id="enhancement-071"></a> **Provide safe dead-letter inspection and controlled redrive.**
    **P1 / M; extends 11/24/35.** Add an operator view for exhausted outbox,
    extraction, and automation jobs with sanitized cause, attempts, affected
    resources, and reconciliation status. Redrive either continues the same
    effect identity or explicitly creates a newly authorized action.
    **Accept:** one poison job cannot monopolize a queue; repeated redrive
    cannot duplicate a known accepted effect; unknown delivery is resolved
    or surfaced before retry. Bulk actions retain per-job audit records.

### Isolation and local resources

72. [ ] <a id="enhancement-072"></a> **Enforce distinct least-privilege runtime profiles.** **P1 / L;
    extends 40/56.** Define separate runner, browser, parser, coding, and
    integration-sidecar profiles through the single container manager.
    Narrow mounts, user IDs, writable directories, network access, and
    kernel capabilities; avoid home-directory and Docker-socket exposure.
    **Accept:** each profile has executable denial tests; a tool compromise
    cannot access unrelated conversations or the control-plane vault through
    its declared runtime resources. Record platform-specific residual risks.

73. [ ] <a id="enhancement-073"></a> **Qualify isolation on every supported operating system.**
    **P1 / M; depends on 72.** Test the real packaged runtime boundaries,
    including Windows junctions, macOS file permissions, Linux mounts, and
    container/native-process differences. Publish which capabilities are
    enforced, emulated, or unavailable. **Accept:** a profile advertised as
    isolated passes the same negative-access contract on each supported
    platform; fallback to a native process cannot silently expand access.
    Unsupported profiles fail before launching tools.

74. [ ] <a id="enhancement-074"></a> **Budget processes, disk, descriptors, and output as well as memory.**
    **P1 / M; extends 19/30.** Add per-run/plugin limits for child processes,
    file descriptors, artifact growth, stdout/stderr, request bodies, and
    queue depth. Account for aggregate descendants and reserve capacity for
    control-plane recovery. **Accept:** synthetic fork/output/disk floods
    terminate the offender with bounded retained diagnostics while another
    conversation stays responsive; cleanup releases accounting after crashes.
    A timeout alone is not a resource limit.

75. [ ] <a id="enhancement-075"></a> **Drain safely during shutdown, update, and host suspend.**
    **P1 / M; extends 23/28.** Stop admitting new work, checkpoint eligible
    steps, preserve approval waits, terminate owned process trees, and record
    unresolved effects before the host service exits. Define a bounded drain
    deadline and restart reconciliation. **Accept:** OS stop/sleep and update
    requests at every run phase leave no unowned background process or
    falsely successful run; expired leases cannot let an old worker commit
    after its replacement starts.

76. [ ] <a id="enhancement-076"></a> **Reduce secret lifetime and document host-compromise limits.**
    **P1 / M; extends 20/54.** Inventory copies of signing and credential
    material across process memory, temporary files, inherited environments,
    crash dumps, and diagnostics. Prefer narrowly scoped handles, deliberate
    zeroization where effective, protected file creation, and separate keys
    for distinct purposes. **Accept:** normal error paths and child-process
    launches contain no unnecessary credentials; rotation retires old uses.
    State clearly that encrypted storage cannot protect plaintext already
    accessible to a fully compromised authorized host process.

77. [ ] <a id="enhancement-077"></a> **Give artifacts transactional references and safe garbage collection.**
    **P1 / M; extends 31/48.** Coordinate blob creation, hashes, SQLite
    references, reference lifetimes, and deletion so a crash cannot expose
    incomplete output or remove a live dependency. Use scope-aware access
    even when physical blobs are deduplicated. **Accept:** crashes between
    file write, rename, DB commit, and collection are recoverable; concurrent
    readers retain a valid reference. Sharing a content hash never authorizes
    access or reveals another conversation's artifact existence.

78. [ ] <a id="enhancement-078"></a> **Qualify useful hardware tiers beyond the primary GPU path.**
    **P2 / L; extends 19/34/49.** Establish measured CPU, Apple, Intel, AMD,
    and NVIDIA profiles only where locally supported, with live resource
    probes and tested fallback behavior. Include battery/thermal throttling
    and memory-pressure observations where available. **Accept:** each
    advertised tier has a reproducible task/latency/quality report; missing
    telemetry does not become fictitious free capacity. Fallback to another
    approved local model must be visible and preserve capability requirements.

### Plugin and integration ecosystem

79. [ ] <a id="enhancement-079"></a> **Ship a plugin author conformance kit.** **P1 / M;
    extends 6/27/45.** Generate minimal plugin projects, schema fixtures,
    mock host APIs, and lifecycle tests for each runtime tier. Check manifest,
    tool/result, sidecar, webhook, UI, and upgrade contracts without requiring
    a configured operator installation. **Accept:** an independently authored
    sample plugin passes the same suite as bundled plugins; invalid schemas,
    undeclared authority, and unsafe upgrades fail with actionable messages.
    Keep the manifest schema the single source for generated documentation.

80. [ ] <a id="enhancement-080"></a> **Negotiate plugin API compatibility explicitly.** **P1 / M;
    extends 27/44.** Version host primitives and protocol capabilities;
    record supported ranges and required features separately from cosmetic
    metadata. Unknown security-critical declarations must fail closed.
    **Accept:** a plugin requiring unsupported semantics fails at installation,
    not mid-turn; supported older bundles pass compatibility fixtures.
    Deprecation diagnostics identify the primitive and replacement without
    silently changing its trust or effect meaning.

81. [ ] <a id="enhancement-081"></a> **Pin executable tool versions for in-flight runs.** **P1 / L;
    extends 16/27.** Bind local plugin dispatch to verified implementation
    digests and schemas, retaining an authorized version until its run drains
    or invalidating the run on upgrade. For MCP, pin schemas and declared
    server identity; executable pinning additionally requires verified
    operator-managed deployment evidence. Otherwise invalidate/requalify on
    change rather than promise knowledge of remote code. Reevaluate approvals
    if behavior or permissions change. **Accept:** upgrading a local plugin
    between planning and execution cannot substitute new code behind an old approval;
    rollback and concurrent old/new runs preserve deterministic identities
    without resurrecting revoked artifacts.

82. [ ] <a id="enhancement-082"></a> **Support publisher revocation and offline compromise response.**
    **P1 / M; extends 27/28.** Build on `core/src/artifact_provenance.rs`
    with operator-approved publisher/digest revocations, inventory impact,
    quarantine, and recovery packages. Treat a signature as identity evidence,
    not proof that code is safe. **Accept:** a revoked artifact cannot start
    through reinstall, rollback, or sidecar cache reuse; already-running work
    follows a documented stop/drain policy. Offline revocation imports record
    their source and freshness limitations.

83. [ ] <a id="enhancement-083"></a> **Make plugin hook ordering, failure, and reentrancy predictable.**
    **P1 / M; extends 23/79.** Specify hook order, allowed operations,
    resource budgets, recursion depth, and whether a failure aborts or isolates
    a hook. Route effects through host dispatch; never hold a DB transaction
    while an arbitrary hook performs network work. **Accept:** slow, failing,
    or recursively triggered hooks cannot deadlock the host or silently run
    twice after restart. Conformance traces identify which hook altered a
    result and under which plugin version.

84. [ ] <a id="enhancement-084"></a> **Trial a restricted WebAssembly plugin tier.** **Lab / L;
    depends on 65/72/79.** Prototype pure transforms and parsers with an
    explicit import surface, bounded memory/CPU, and no ambient filesystem,
    network, or credentials. Compare deployment size and overhead with Rhai
    and subprocess tiers before adoption. **Accept:** adversarial fixtures
    cannot escape declared imports or starve the host; lifecycle and typed
    tool contracts remain identical. Review the chosen runtime's
    [security model](https://docs.wasmtime.dev/security.html); WebAssembly
    alone does not establish safe host APIs.

85. [ ] <a id="enhancement-085"></a> **Trial delegation between explicitly paired operator-owned hosts.**
    **Lab / L; depends on 25/42/45.** Consider an external-agent protocol
    adapter only for verified local/VPN operator hardware, with peer identity,
    narrowed authority, data labels, durable task IDs, and typed artifacts.
    Keep the current single-operator model and one authority per task.
    **Accept:** disconnect/reconnect preserves task state; an unpaired or
    cloud-inference peer is rejected; received instructions cannot grant
    authority. Adopt only if measured hardware/task distribution benefits
    justify the additional trust and recovery surface.

### Context and knowledge

86. [ ] <a id="enhancement-086"></a> **Make instruction precedence inspectable and resistant to injection.**
    **P1 / M; extends 32/39/40.** Resolve operator instructions, task requests,
    repository guidance, skills, and retrieved content through a documented
    hierarchy with source/version receipts. Importing a repository or reading
    an attachment must not grant it host authority. **Accept:** conflicting
    instruction fixtures resolve predictably; a file masquerading as a system
    message cannot broaden capabilities; the operator can inspect which
    legitimate instruction applied without exposing hidden secrets.

87. [ ] <a id="enhancement-087"></a> **Detect dependency cycles and capacity deadlocks.** **P1 / M;
    depends on 30/42/68.** Maintain a bounded wait graph for child joins,
    inference slots, tool resource locks, and approval waits. Reject cyclic
    dependencies and release reclaimable capacity while a parent waits for
    a child. **Accept:** a parent cannot hold the only model slot while
    waiting for a child that needs it; cyclic joins yield an actionable
    blocker. Valid long waits remain healthy rather than being cancelled
    simply because a generic inactivity timer expires.

88. [ ] <a id="enhancement-088"></a> **Cache qualified read results with authorization and freshness checks.**
    **P2 / M; depends on 38/48/65.** Cache only approved read operations,
    keyed by canonical arguments, scope, tool version, source revision, and
    expiry. Reauthorize each hit and preserve source provenance and stale
    status. **Accept:** repeated valid reads reduce measured latency;
    permission changes, deletions, and resource updates invalidate reuse;
    no cross-conversation disclosure occurs. Keep this separate from model
    prefix caching and never interpret a cached effect result as permission
    to replay that effect.

89. [ ] <a id="enhancement-089"></a> **Resolve entities without silently merging identities.** **P1 / M;
    extends 37/38.** Add evidence-backed aliases and proposed entity merges
    for people, projects, places, and resources. Keep identity authorization
    separate from semantic similarity; provide split/undo operations and
    preserve temporal names. **Accept:** same-name contacts stay distinct
    until authoritative evidence resolves them; a mistaken merge can be
    reversed without rewriting source events or inheriting another entity's
    trust. Retrieval exposes ambiguity when multiple identities still fit.

90. [ ] <a id="enhancement-090"></a> **Separate explicit preferences from inferred personalization.**
    **P1 / M; extends 37/39.** Store operator-declared preferences separately
    from proposed inferences, with scope, expiry, evidence, and a clear
    correction surface. Distinguish a one-task instruction from a lasting
    preference and require the configured governance before persistence.
    **Accept:** an isolated request does not silently rewrite global behavior;
    correcting or removing a preference changes subsequent loadouts; a
    third-party message cannot impersonate the operator's preference.

91. [ ] <a id="enhancement-091"></a> **Ingest documents with page, cell, and region evidence.**
    **P1 / L; extends 31/37/46.** Add optional local extraction/OCR plugins
    for PDFs, scans, tables, and office documents, using attachment hashes,
    parser versions, coordinates, and explicit extraction failures. Preserve
    source trust after OCR. **Accept:** citations open the original page,
    sheet/cell, or image region; unsupported pages are reported rather than
    silently omitted; extraction is sandboxed and bounded. Embedded macros,
    links, and instructions never execute merely because a document is read.

92. [ ] <a id="enhancement-092"></a> **Maintain a revision-aware local code and documentation index.**
    **P1 / L; extends 38/40.** Build on the memory roadmap's CodeGraph/wiki
    tables and developer Graphify integration with incremental symbol,
    reference, and documentation indexing keyed to workspace revision.
    Expose freshness and coverage before using impact results. **Accept:**
    renames/deletions update affected nodes; excluded paths and secrets stay
    out; stale or incomplete indexes cannot justify claiming no callers or
    no affected tests. Measure usefulness against plain text search.

### Coding execution

93. [ ] <a id="enhancement-093"></a> **Apply patches with explicit file preconditions and transactions.**
    **P1 / M; depends on 40/41.** Require expected file hashes or contextual
    preconditions, bound patch size, and handle multi-file changes without
    leaving undocumented half-applied state. Respect permissions, encodings,
    line endings, and symlink/junction boundaries. **Accept:** concurrent human
    edits cause a conflict instead of overwrite; interruption can restore or
    finish the owned change set; malformed patches cannot write beyond the
    workspace. Return a verified diff rather than an optimistic edit summary.

94. [ ] <a id="enhancement-094"></a> **Use structured command specifications and platform-aware execution.**
    **P1 / M; depends on 40/54.** Prefer executable/argument arrays, declared
    working directories, bounded stdin, and sanitized inherited environments;
    treat shell interpretation as a separate capability. Provide tested
    PowerShell/POSIX behavior without translating commands by string replacement.
    **Accept:** spaces, Unicode, shell metacharacters, and quoted paths cannot
    alter command intent; credentials never appear in arguments or echoes;
    process exit, timeout, and cancellation remain distinguishable.

95. [ ] <a id="enhancement-095"></a> **Manage development servers as owned run resources.** **P1 / M;
    depends on 40/74/75.** Give background servers and terminal jobs durable
    ownership, bounded logs, port leases, readiness probes, and lifecycle
    controls. Bind previews to approved interfaces and scope browser access
    to the correct run. **Accept:** repeated start/restart does not leave
    orphan listeners or kill another user's process; a child server ends
    when its ownership expires; the UI can distinguish starting, ready,
    failed, and intentionally detached jobs.

96. [ ] <a id="enhancement-096"></a> **Make test and build evidence independently verifiable.**
    **P1 / M; depends on 22/40.** Record the actual command, exit status,
    checked revision, environment identity, and bounded output digest from
    the executor. Flag changed/deleted tests and unexpected skips separately
    from passing results. **Accept:** agent prose cannot fabricate a passing
    check; evidence becomes stale after relevant edits; deleting a required
    test does not satisfy its acceptance criterion. Preserve the operator's
    right to approve a legitimate test correction explicitly.

97. [ ] <a id="enhancement-097"></a> **Treat dependency installation as an explicit execution boundary.**
    **P1 / M; depends on 25/40/72.** Use locked dependencies, declared package
    sources, controlled caches, and policy for lifecycle/build scripts in
    coding environments. Distinguish fetching bytes from executing package
    code and keep host credentials out of both. **Accept:** a malicious
    install script cannot access the host vault or arbitrary network;
    offline cached builds remain reproducible; changing the lockfile produces
    a reviewable dependency diff and invalidates stale build evidence.

98. [ ] <a id="enhancement-098"></a> **Validate application migrations against disposable local databases.**
    **P2 / M; depends on 40/66/96.** Provide a coding-plugin workflow for
    schema diffing, fixture generation, migration tests, and explainable
    rollback limitations. Use scoped disposable datasets by default;
    production connection identities are separate protected capabilities.
    **Accept:** generated application migrations are exercised on realistic
    fixtures without reaching production; destructive changes are visible;
    the model cannot switch database targets by editing a command or config
    file after approval. This is separate from execlaw's own migrations in 64.

99. [ ] <a id="enhancement-099"></a> **Verify UI changes in an isolated browser with attributable evidence.**
    **P1 / M; depends on 40/46/95.** Run user journeys against the owned
    preview, capture revision-bound screenshots and accessibility results,
    and assert meaningful behavior rather than visual similarity alone.
    Keep browser downloads, cookies, and credentials isolated per task.
    **Accept:** a wrong preview URL or stale build cannot pass verification;
    navigation off the approved app is gated; results cite the test action,
    viewport, and revision. Sensitive regions are excluded from shareable
    captures under the operator's policy.

### Messaging and multimodal work

100. [ ] <a id="enhancement-100"></a> **Bind sensitive sends to recipient and audience identities.**
     **P0 / M; extends 10/24/51.** Resolve exact recipients and group
     membership epochs before approving disclosure, then revalidate at send.
     Native conversation IDs may remain stable while their audiences change.
     **Accept:** an added lower-trust member, ambiguous contact alias, or
     changed destination invalidates stale disclosure authorization; private
     history is not silently carried into a wider audience. Show the affected
     audience without assuming display names establish identity.

101. [ ] <a id="enhancement-101"></a> **Normalize message edits, deletion, reactions, and reply lineage.**
     **P1 / L; extends 12/48.** Extend transport/archive contracts with
     append-only revisions and manifest-declared support for these operations.
     Preserve source IDs and distinguish current display state from audit
     history and deletion policy. **Accept:** duplicate/out-of-order edits
     and delete-before-create events converge; revoked text is not reused
     as current evidence; unsupported operations have a visible fallback.
     Reactions do not become approvals unless explicitly bound to a secure
     approval protocol.

102. [ ] <a id="enhancement-102"></a> **Make cross-channel continuity an explicit identity operation.**
     **P1 / M; extends 26/37.** Allow verified account linking and controlled
     context transfer between the operator's channels, recording origin,
     audience, and selected history. Never merge conversations solely because
     names or message text match. **Accept:** unlinking an account revokes
     future continuity; group/private transitions cannot expose private
     history; replying through one transport preserves the intended thread.
     All channel adapters use the same host contract.

103. [ ] <a id="enhancement-103"></a> **Coalesce trigger bursts without losing event meaning.**
     **P1 / M; extends 30/42.** Add per-source debounce, bounded batching,
     event identity, and stale-event policies ahead of agent/workflow
     admission. Preserve the source sequence and let explicitly designated
     urgent events bypass coalescing. **Accept:** a thousand-event fixture
     generates the configured bounded work while every original event remains
     attributable; restart preserves pending windows; a later correction
     supersedes stale proposed work without deleting its audit trail.

104. [ ] <a id="enhancement-104"></a> **Qualify webhook replay resistance and credential rollover.**
     **P1 / M; extends 25/79.** Test plugin-declared webhook authentication
     against exact body bytes, bounded parsing, replay windows, duplicate
     IDs, and overlapping old/new credentials during rotation. Retain the
     intentional acknowledgment behavior needed to avoid retry storms.
     **Accept:** forged/replayed requests never create duplicate authorized
     work; rotation does not drop valid deliveries; acknowledged-but-rejected
     requests remain distinguishable in sanitized metrics. Do not assume
     every external provider offers the same signature mechanism.

105. [ ] <a id="enhancement-105"></a> **Support an explicit operator takeover and hand-back state.**
     **P1 / M; extends 22/43.** Let the operator take responsibility for a
     conversation/task while preserving drafts, pending approvals, and run
     context. Suppress new autonomous sends under that ownership until an
     explicit hand-back; reconcile already-dispatched work. **Accept:** a
     race between agent completion and takeover cannot cause two replies;
     reconnect preserves ownership; handing back includes the operator's
     intervening actions so the agent does not repeat them.

106. [ ] <a id="enhancement-106"></a> **Preserve multimodal grounding through transformations.**
     **P2 / L; extends 34/46/47/91.** Link crops, OCR spans, captions, and
     audio segments to original artifact hashes, coordinates, and time ranges.
     Permit evidence-linked answers and require fresh visual state before
     coordinate-based actions. **Accept:** an edited screenshot invalidates
     old action coordinates; text in images cannot become trusted policy;
     noisy/occluded fixtures measure unsupported claims separately from text
     quality. All references must resolve within the caller's artifact scope.

### Evaluation and agent judgment

107. [ ] <a id="enhancement-107"></a> **Model-check the critical state transitions.** **P1 / M;
     depends on 23/24.** Build a small executable model of run, step,
     approval, lease, and outbox ownership, exploring bounded interleavings
     of duplicate requests, two workers, restart, expiry, and cancellation.
     **Accept:** explored traces admit no two current effect owners or
     successful completion with unresolved required work; each counterexample
     becomes a source-level regression. Treat the model as a checked contract,
     not proof that unmodeled external services behave correctly.

108. [ ] <a id="enhancement-108"></a> **Fuzz full protocol conversations and untrusted parsers.**
     **P1 / M; extends 17/29/45.** Cover runner registration, unsolicited
     tool results, reordered/duplicate frames, version skew, ZIP manifests,
     JSON schemas, document parsers, and streams with bounded stateful fuzz
     targets. **Accept:** generated inputs produce neither unauthorized
     dispatch nor unbounded resource use; panics, hangs, and invalid success
     states yield minimized reproducible fixtures. Run parser fuzzing without
     live credentials, network effects, or the operator database.

109. [ ] <a id="enhancement-109"></a> **Mutation-test the enforcement tests themselves.** **P1 / M;
     extends 17/107.** In disposable builds, deliberately remove selected
     trust, capability, approval-hash, pairing, and scope checks. Measure
     whether the intended regression tests catch each critical mutation.
     **Accept:** every defined security-critical mutation is detected or
     explicitly justified as equivalent; surviving meaningful mutations
     block that component's release. This tests the sensitivity of the test
     suite rather than treating a large passing test count as evidence alone.

110. [ ] <a id="enhancement-110"></a> **Prove local-only operation with denied-network integration tests.**
     **P1 / M; extends 25/34.** Run the complete inference, embeddings,
     reranking, speech, judge, and skill-evaluation paths in an environment
     that permits only configured local fixtures. Separately permit explicit
     non-inference integration endpoints where a scenario needs them.
     **Accept:** missing models fail visibly instead of contacting another
     provider; unexpected DNS/socket attempts fail the test. Cover startup,
     failure fallback, upgrades, and optional components, not only normal
     chat requests.

111. [ ] <a id="enhancement-111"></a> **Protect held-out tasks and calibrate local judges.** **P1 / M;
     extends 21/39/96.** Keep protected answers and verifier policy outside
     the task workspace; prevent their capture into memory or promoted
     skills. Track dataset lineage, exclusions, and evaluator disagreement
     on a human-reviewed calibration set. **Accept:** repeating expected
     words or editing a verifier cannot manufacture success; holdout canaries
     never enter task prompts or training captures. Separate clean evaluation
     runs from demonstrations that intentionally reveal solutions.

112. [ ] <a id="enhancement-112"></a> **Measure when to clarify, proceed conservatively, or abstain.**
     **P1 / M; extends 22/43.** Evaluate missing requirements, ambiguous
     identities, conflicting evidence, and unknown tool outcomes. Base
     decisions on evidence and task policy, not a model's self-reported
     confidence alone. **Accept:** report unnecessary-question rate,
     wrong-action rate, and completion after clarification; ambiguous effect
     targets are never guessed. A safe partial result should retain useful
     work and state exactly what information is missing.

113. [ ] <a id="enhancement-113"></a> **Detect non-progress and bound strategy changes.** **P1 / M;
     extends 22/30.** Extend identical-call limits with durable evidence of
     progress: changed artifacts, improving verifier outcomes, new source
     coverage, or resolved dependencies. Recognize alternating loops and
     arguments paraphrased only to evade repetition limits. **Accept:**
     oscillating fixtures stop or perform one budgeted replan; legitimate
     iterative repair continues when evidence improves. Report partial work
     and blockers instead of spending the remaining budget on repeated plans.

### Operations and maintainability

114. [ ] <a id="enhancement-114"></a> **Make release builds reproducible and dependency inventories actionable.**
     **P1 / L; extends 28/82.** Pin build inputs/toolchains and generate
     verified inventories for Rust, SPA, native wrappers, plugins, and
     sidecars. Compare independent rebuilds and document unavoidable
     nondeterminism. **Accept:** artifact differences can be explained from
     recorded inputs; a revoked/vulnerable dependency maps to installed
     artifacts and affected surfaces. License and provenance checks are
     evaluated for the complete package, not just the Rust dependency graph.

115. [ ] <a id="enhancement-115"></a> **Export standard telemetry through a replaceable local adapter.**
     **P2 / M; extends 35/49.** Map durable IDs and numeric measurements to
     a pinned [OpenTelemetry GenAI convention revision](https://github.com/open-telemetry/semantic-conventions-genai).
     Keep the canonical SQLite schema independent of that external format,
     use metadata-only defaults, and bound export queues. **Accept:** a
     local collector can correlate runs/model/tool spans; a failed collector
     cannot stall execution or grow storage indefinitely; convention upgrades
     pass compatibility/redaction fixtures without rewriting event history.

116. [ ] <a id="enhancement-116"></a> **Provide a complete air-gapped installation and update path.**
     **P2 / L; extends 28/50.** Assemble an operator-verifiable offline kit
     containing supported models, tokenizers, runtime images, plugins,
     dependencies, licenses, and migration/recovery tools. Make optional
     network integrations visibly unavailable rather than silently hanging.
     **Accept:** installation and a representative local task succeed on a
     fresh disconnected host; no runtime download is unexpectedly required;
     updates verify all inputs before altering the installation.

117. [ ] <a id="enhancement-117"></a> **Separate host-service authority from desktop and tool processes.**
     **P1 / M; extends 26/72/76.** Document and minimize OS account,
     keyring, IPC, and filesystem rights for the control plane, desktop UI,
     runners, and sidecars. Authenticate local IPC explicitly and avoid
     treating loopback access as identity. **Accept:** an unrelated local
     process cannot impersonate a runner or read service secrets; installing
     or launching the UI does not unnecessarily elevate tool execution.
     Test the actual per-OS service installation configuration.

118. [ ] <a id="enhancement-118"></a> **Rehearse recovery after loss of the entire machine.**
     **P1 / M; extends 20/28.** Inventory the separate recovery requirements
     for encrypted DBs, signing keys, plugin state, artifacts, and external
     account reauthorization. Offer protected offline recovery material and
     measurable recovery-point/recovery-time objectives. **Accept:** restore
     onto a clean replacement host without relying on the old OS keyring;
     verify integrity and reconcile pending effects before restarting sends.
     Report any unrecoverable sidecar state or expired external credentials.

119. [ ] <a id="enhancement-119"></a> **Version configuration changes and preview their operational impact.**
     **P1 / M; extends 57/80.** Keep SQLite configuration authoritative while
     adding revisioned change sets, validation, actor/reason audit, and
     secret-redacted diffs. Explain whether a change affects new runs only,
     invalidates approvals, or restarts a service. **Accept:** concurrent
     admin edits cannot silently overwrite each other; a partially invalid
     change set does not partially apply; rollback respects schema and
     credential revocation constraints instead of replaying stale secrets.

120. [ ] <a id="enhancement-120"></a> **Generate capability and support documentation from verified evidence.**
     **P1 / M; extends 21/28/50.** Maintain a capability inventory linking
     feature flags, runtime paths, manifests, test commands, platforms, and
     last qualified artifacts. Generate support tables and flag contradictory
     prose for review. **Accept:** a stub or declared-but-unimplemented node
     cannot appear as production-ready solely because its enum exists;
     examples run against disposable fixtures; release docs distinguish
     designed, implemented, tested, and qualified capabilities.

### Documents and automation UX

121. [ ] <a id="enhancement-121"></a> **Support resumable, quota-governed attachment uploads.**
     **P1 / M; extends 74/77.** Extend the current data-URL attachment
     path with scoped upload sessions, chunk checksums, progress/cancel,
     expiry, aggregate quotas, and atomic finalization. Validate content
     before making it available to extraction or model tools. **Accept:**
     interrupted transfers resume accepted chunks; corrupt/duplicate chunks
     behave deterministically; unfinished files cannot enter context;
     abandoned temporary bytes are reclaimed without deleting active uploads.

122. [ ] <a id="enhancement-122"></a> **Provide a deliverable library with previews and non-code revisions.**
     **P1 / M; extends 22/77.** Extend attachments/cards with searchable
     reports, tables, charts, source runs, input hashes, approved versions,
     and accessible previews. Distinguish a draft, a reviewed artifact, and
     an externally published copy. **Accept:** users can locate and compare
     deliverables without searching raw chat; a preview matches the verified
     download bytes; an updated report does not silently replace the version
     previously approved or shared. Apply artifact scope to every preview.

123. [ ] <a id="enhancement-123"></a> **Package reproducible data-analysis runs.** **P1 / L;
     extends 22/77/97.** Build on `server/src/python_sandbox/` with analysis
     capsules containing input hashes, executed code order, pinned image and
     packages, seeds, outputs, and numerical tolerances. Expose hidden
     notebook/kernel-state dependencies and approved network inputs.
     **Accept:** a fresh local kernel can reproduce declared outputs or
     explain drift; a chart links to its input data and transformation;
     packaging does not include credentials or unrelated conversation files.

124. [ ] <a id="enhancement-124"></a> **Pin automation runs to immutable published workflow revisions.**
     **P1 / M; extends 16/81.** Add draft/published graph revisions,
     definition hashes, reviewable diffs, and run-pinned revision IDs to
     `core/src/automations.rs` and `automation_runs.rs`. Version trigger and
     node semantics together. **Accept:** editing a live workflow cannot
     alter an in-flight run; historical traces resolve the exact graph;
     rollback affects future triggers without replaying completed effects.
     Invalid drafts remain editable but cannot be activated.

125. [ ] <a id="enhancement-125"></a> **Finish durable approval, wait, composition, and join nodes.**
     **P1 / L; depends on 23/30/69/87/124.** Implement the declared workflow
     control nodes with persisted waits, typed child inputs/results, bounded
     joins, and approval expiry. These are operator-authored graph semantics,
     distinct from agent-selected delegation. **Accept:** restart at each
     wait resumes the same node; cycles and unbounded fan-out fail validation;
     rejection/cancellation ends the correct branches without orphaned work
     or treating a partial join as complete.

126. [ ] <a id="enhancement-126"></a> **Make automation scenarios effect-free and support shadow evaluation.**
     **P0 / M; extends 21/66/124.** The existing editor test-run path is
     not an isolated simulator: introduce explicit mock/effect-recording
     backends and disposable state. Compare candidate and published graphs
     over captured or synthetic event sets with expected branch/action
     assertions. **Accept:** dry/shadow runs cannot write production alerts,
     call live effectful plugins, or send network effects; local model use
     is separately declared and budgeted. An intentional live test is a
     different, explicitly authorized operation with a durable run record.
     See F18.

127. [ ] <a id="enhancement-127"></a> **Give reply drafts a freshness-aware review inbox.**
     **P1 / M; extends 53/100/105.** Build on existing agent reply drafts
     with source-message IDs, draft revisions, recipient/audience snapshots,
     review ownership, and stale-state checks. Show intervening messages and
     changed facts before send. **Accept:** a newer correction or audience
     change marks the draft stale; concurrent reviews cannot send two
     versions; editing approved content requires appropriate renewed review.
     Rejected and superseded drafts remain auditable under retention policy.

128. [ ] <a id="enhancement-128"></a> **Manage operator attention with quiet hours and actionable digests.**
     **P2 / M; extends 35/71.** Extend alert fingerprints, snooze, and
     acknowledgment with SQLite policies for source/severity/channel,
     aggregation windows, and escalation deadlines. Let the operator define
     which urgent incidents may bypass quiet hours. **Accept:** repeated
     low-priority events form one linked digest; acknowledgment cancels
     escalation across restart; suppressed approval notifications do not
     count as consent or hide a blocked task's durable status.

129. [ ] <a id="enhancement-129"></a> **Qualify multilingual behavior beyond translated UI strings.**
     **P1 / M; extends 21/31/50.** Test multilingual retrieval, date/number
     interpretation, language persistence, mixed-language sources, and
     bidirectional display across tools and transports. Retain source text
     and use stable protocol identifiers independently of translated labels.
     **Accept:** locale fixtures preserve decimal values and disambiguate
     dates before consequential actions; RTL/CJK content remains readable;
     safety and identity decisions do not change solely because labels are
     translated. Measure task quality per qualified language/model pair.

130. [ ] <a id="enhancement-130"></a> **Offer incrementally synchronized local knowledge collections.**
     **P1 / L; extends 38/48/91.** Build on the wiki lifecycle and memory
     stores with optional collection adapters for approved folders and
     document sources, explicit exclusions, ownership, change detection,
     import checkpoints, and revision tracking. **Accept:** modifying,
     renaming, or deleting one source updates affected derived records;
     interrupted imports resume without duplication; excluded secrets never
     enter extraction. Detaching a collection clearly distinguishes retained
     approved assertions from removable indexes and source copies.

### 2026-09-30 agent event/schedule slice

The always-on specialist path now has typed trigger/output contracts,
versioned definition snapshots, stable transport-event mailbox identity,
per-recipient archive tools, and calendar fires with time-zone, overlap,
catch-up, and quiet-hours policy. Reply proposals persist source and audience
snapshots, revisions, stale state, and Controller review; automatic sends carry
an ownership-generation fence into outbox admission and relay dispatch. The
Agents UI exposes a draft inbox, takeover/hand-back, and no-effect trigger
preview with an explicitly requested, bounded local-model option. These are
implementation evidence for parts of H022/H023/H038/H100/H103/H105/H126-H128,
not completion of those broad requirements. Their checkboxes remain open until
the remaining acceptance evidence in `implementation-plan.md` and
`remaining-improvements-todo.md` passes, including burst coalescing,
membership changes, external in-flight reconciliation, automation graph
shadowing, full release tests, and installed TrueNAS qualification.
The local core/server library suites passed before the final source-event API
refactor; the fresh core suite passed 726/726 afterward, along with focused
regressions and workspace compilation. The standalone SPA suite passed.
Windows Code Integrity event 3077 recorded an Enterprise signing-policy block
for three compiled test executables in the initial workspace attempts. A fresh
build of the first target passed 1/1, and the final `cargo test --workspace
--quiet` rerun exited 0, including those targets. The server library suite
passed 1,182 with five ignored after the refactor. This closes the local
workspace execution gate, not the remaining live and recovery acceptance
gates. `remaining-improvements-todo.md` records the exact targets and results.

### Delivery sequence for the extended portfolio

Deliver the complete portfolio through staged releases. Fix
the existing critical findings first. Include 52, 58, 100, and 126 in the
relevant security/durability release gates; do not wait for the optional
ecosystem work to close them. Implement one complete user journey at a time:

| Increment | Candidate slice | Exit evidence |
|---|---|---|
| Trustworthy execution | 23-29, 51-58, 65-66, 100, 126 | Authorization, durability, stale-state, and effect-free simulation tests |
| Reliable local service | 59-64, 69-77, 117-119 | Recovery drills, bounded queues/storage, packaged OS tests |
| Effective coding assistant | 40-44, 86-88, 92-99 | Held-out repairs with independently verified tests and safe diffs |
| Useful knowledge and documents | 37-39, 89-91, 106, 121-123, 129-130 | Evidence-linked answers and reproducible deliverables |
| Dependable personal automation | 67-71, 101-105, 124-128 | Traceable trigger handling, review, and restart-safe workflows |
| Sustainable ecosystem | 78-85, 107-116, 120 | Conformance, reproducibility, measured benefit, and maintenance ownership |

The evaluation work runs alongside every increment; its placement in the last
row does not defer testing. Choose the next slice using real task failures,
operator demand, security severity, dependency readiness, and measured
benefit. Record an owner, evidence artifact, and supported-platform scope
before checking off any item. Experimental tiers and delegation adapters remain
in scope as qualified trials; changing or removing a requirement needs an
explicit recorded operator decision. The implementation ledger tracks all 130.

<a id="nexus-visual-roadmap"></a>

## Nexus: optional visual conversation workspace

Review date: 2026-09-27. **NX01-NX30** are a focused visual/interaction
extension to this roadmap. They do not renumber or change the H001-H130
baseline; their separate delivery rows live in
[`implementation-plan.md`](implementation-plan.md#nexus-visual-extension).
All entries below are proposed implementation work, not shipped UI changes.
The existing Classic/Nexus setting already exists. The goal is to improve
Nexus while making disabling it reliable in every interaction state.

### What exists and what was inspected

The reviewed implementation includes `web/src/chat/MessageStream.tsx`,
`Sidebar.tsx`, `useChatAppearance.ts`, `settings/GeneralPage.tsx`,
`styles/theme.scss`, the message/settings tests, the appearance browser
fixture, `crates/server/src/chats.rs`, `chats/nexus.rs`, and migration
`0029_nexus_organization.sql`.

Nexus already provides source colors and labels, incoming/right and
execlaw/left alignment, sequence markers, source navigation, a Relationships
mode, explicit annotation links, branch/tag organization, saved views,
conversation-scoped history search, collapsible groups, buffered arrivals,
streaming, attachments, and delivery-state labels. Preserve and refine these
features rather than rebuilding them under new names. The present
Relationships mode emphasizes messages in a timeline; it is not a spatial
relationship graph.

Appearance is currently browser-local, defaults to Classic, and synchronizes
between tabs. Organization and saved views are SQLite-backed per conversation.
The existing schema does not provide an unrestricted cross-conversation graph
or per-user ownership of saved views. Visual grouping must not merge identities,
change trust, grant access, or send anything to a transport.

The existing appearance browser check passed for dark/light themes at 1440,
390, and 320 pixels, including its current navigation, Classic-restoration,
and mocked production-login checks. The two focused Vitest files passed all
62 tests. These checks use synthetic messages and mocked APIs; they are not
live WhatsApp/Signal, server-authorization, full accessibility, or performance
qualification. The Classic restoration test currently restores after expanding
a branch, leaving the collapsed-state case uncovered.

Baseline captures from that fixture, not operator conversations:

| Existing desktop, dark | Existing mobile, light |
|---|---|
| ![Current Nexus desktop fixture with multi-source messages and dense navigation](screenshots/nexus-baseline-desktop-dark.png) | ![Current Nexus mobile fixture showing the stacked navigation toolbar](screenshots/nexus-baseline-mobile-light.png) |

The captures show a useful source vocabulary, but controls compete with the
conversation, especially on mobile. The next design should use stronger
message hierarchy, less repeated metadata, and progressive disclosure. A
neon graph, animated particles, or glass effects over message text would add
visual activity without solving source attribution or reading continuity.

<a id="nexus-live-review"></a>

### Live deployment screenshot review

The operator supplied a local development conversation on port 5174 and a
TrueNAS conversation on port 3031. Both initially redirected the isolated
review browser to login. The operator signed in directly; the review then
used those authenticated sessions. Captures were taken with Nexus selected in
that review browser, at 1440 x 1000 and 390 x 900, in the served dark theme.
Inspection used scrolling and the Conversation/Relationships view controls.
No message was sent, rerun, cancelled, annotated, or deleted.

This is distinct from the earlier synthetic fixture. The local URL contained
four loaded Web/execlaw messages, including historical development-stub output.
The TrueNAS URL contained a 200-message loaded WhatsApp/execlaw window. Neither
live capture demonstrates WhatsApp and Signal messages mixed within the same
timeline; retain the synthetic mixed-transport cases for that requirement.
Do not interpret an old stub response as proof that inference is currently
unavailable, or the loaded window size as the whole conversation's size.

#### Captures and privacy

Raw captures remain outside the repository. The linked copies obscure message
bodies, personal metadata, private thread labels, and account details while
preserving layout geometry. Gray rectangles are privacy overlays, not rendering
defects. Viewport captures cover the oldest/newest **loaded** positions and
Relationships mode; they are not a full-history export. Deployment build
revisions were not matched to a source commit, so observations describe the
served views and do not certify that every previous source finding is present
on both deployments.

| Capture | What it supports |
|---|---|
| [L1: local desktop](screenshots/nexus-live-local-desktop.png) | Short thread, organization failure state, repeated controls and response actions |
| [L2: local mobile](screenshots/nexus-live-local-mobile.png) | Toolbar/error height, limited reading area, action-heavy response card |
| [L3: local Relationships view](screenshots/nexus-live-local-relationships.png) | All four loaded rows dimmed while organization data is unavailable |
| [L4: TrueNAS desktop](screenshots/nexus-live-truenas-desktop.png) | Long loaded window, repeated WhatsApp labels and large group summary |
| [L5: TrueNAS mobile](screenshots/nexus-live-truenas-mobile.png) | Narrow incoming reading column, stacked controls, floating latest-message control |
| [L6: TrueNAS newest loaded position](screenshots/nexus-live-truenas-newest.png) | Repeated chrome around short messages and cumulative vertical space |

| Local mobile | TrueNAS mobile |
|---|---|
| ![Sanitized live local Nexus at 390 by 900 pixels](screenshots/nexus-live-local-mobile.png) | ![Sanitized live TrueNAS Nexus at 390 by 900 pixels](screenshots/nexus-live-truenas-mobile.png) |

#### Measured observations

Measurements come from the rendered DOM after loading, not estimated screenshot
coordinates. Values are rounded; these are individual capture samples, not
latency or memory benchmarks. The measurement record is
[`nexus-live-review-metrics.json`](screenshots/nexus-live-review-metrics.json).

| Measure | Local development | TrueNAS |
|---|---:|---:|
| Loaded/rendered message rows | 4 | 200 |
| Mounted organization selector options | 32 | 40,800 |
| Desktop Nexus toolbar height | 136.5 px | 99.2 px |
| Mobile Nexus toolbar height | 238.4 px | 205.1 px |
| Mobile transcript viewport height | 466.5 px | 499.8 px |
| First incoming mobile bubble width, before inner padding | 279.8 px | 266.2 px |
| Rows dimmed in desktop Relationships mode | 4 of 4 | 89 of 200 |
| Document-level horizontal overflow in captured states | Not observed | Not observed |

The TrueNAS incoming bubble uses only about 68% of the 390 px viewport before
its own text padding. The local toolbar plus thread header reaches about
315 px from the top; the TrueNAS equivalent reaches about 282 px. Absence of
horizontal overflow therefore does not establish good mobile readability.
The 40,800 mounted options strengthen NXF07's scaling concern but do not by
themselves measure frame rate, latency, or heap use.

The organization request also differs between deployments. Local returned
HTTP 200 with `Content-Type: text/html`, which was not JSON, and displayed
`Organization unavailable`. TrueNAS returned HTTP 200 with JSON containing
the expected annotations/views arrays. This verifies a response-contract
problem on the local endpoint; it does not identify whether the cause is a
proxy, fallback route, missing endpoint, or deployment-version mismatch.
Record this as NXF11 and test the failure explicitly instead of diagnosing
an authorization or database problem from the banner alone.

#### Revisions to the existing proposal

| Live observation | Required proposal refinement |
|---|---|
| Both toolbars advertise two sources, but each sample has one incoming channel plus execlaw | NX03/NX05 must distinguish transport count, author/agent role, and participant identity. In WhatsApp-only views, sender and remote-chat context matter more than repeating the same green provider label. |
| Secondary filters, ordering, and saved-view naming occupy persistent mobile rows | NX04/NX26: move them into an explicit drawer. Target at most 96 px for the default closed mobile Nexus toolbar below the thread header; reserve a separate compact error state. Validate this as a proposed budget, not an achieved result. |
| Incoming alignment, rail, group padding, and bubble padding compound | NX22/NX26: target at least 85% viewport width for ordinary text bubbles at 320/390 px, using source/author headers and restrained edge marks for direction. Do not count a zero-overflow test as sufficient. |
| Short consecutive messages repeat the provider, kind, identity/time line, event node, and Organize control | NX03/NX06/NX22: group compatible consecutive same-author messages visually, preserving individual IDs and boundaries. Require stable author/account/chat identity and a bounded time interval; never group by display name alone. |
| Local organization fails while editing and saved-view affordances remain present | NX12/NX20/NX21/NX28: show a compact degraded capability state, disable unavailable edits with an explanation and Retry, and preserve message reading. Validate status, content type, and response schema. |
| Local Relationships mode fades every loaded message during that failure | NX08/NX09/NX24: distinguish unknown relationship availability from a successful empty result. Keep the timeline readable and show an explanatory empty/degraded state instead of a wall of dim text. |
| Response text shares a large card with destination selection, Send, Cancel, and Rerun | NX16/NX29: separate draft content from a compact review footer; expose recipient/account explicitly and separate external sending from generating another answer. Never hide consequential state merely to save space. |
| A web-origin conversation offers a preselected external transport for historical stub output | NX06/NX29/NX30: clearly label known synthetic output, distinguish Forward from Reply, and require explicit destination/recipient selection for a new external action. The screenshot proves an ambiguous affordance, not an actual misdelivery. |
| Large loaded-window/group totals and `0 / N` navigation counters can look like whole-history or message counts | NX05/NX12/NX14: use explicit labels such as Loaded messages, Matching messages, and No match selected. Distinguish provider-wide grouping from a real remote group and do not fabricate unknown totals. |
| Floating jump-to-latest overlaps mobile message/metadata space | NX15/NX26: reserve a safe location above the composer, account for safe-area/keyboard insets, and verify overlap with long metadata, reply previews, and unread counts. |
| A 200-row view mounts 40,800 organization options | NX20/NX25: closed organization UI should mount zero related-message choices; one open inspector should use bounded target search. Qualify this before adding graph layout or animation. |

**Updated priority:** retain NX01/NX28 correctness and access gates, then make
NX04/NX20/NX22/NX25/NX26 the first visible improvement slice. Follow with
NX03/NX06/NX12/NX16/NX29 for identity, degraded states, and response review.
NX07/NX08 connectors/maps remain later work: the live WhatsApp-heavy view
needs clearer people, reading space, and selective context more urgently than
a graph covering all loaded messages. NX30 must include both a four-message
web/stub case and a realistically long single-transport case, not just the
small mixed-source fixture.

### Proposed visual direction

Use a calm, layered conversation workspace: a compact source/search header,
a readable chronological transcript, and a contextual inspector that opens
only when needed. Keep the normal reading view primary. Add source lanes and
a bounded relationship map as optional lenses over the same authorized data.
Selecting a node, reply, source, or search hit should synchronize selection
across these views while preserving a route back to the reading position.

```text
Conversation title              [Timeline] [Sources] [Relationships] [View]
[All sources] [WhatsApp 12] [Signal 7] [execlaw 4]     [Search] [Filters 2]
-----------------------------------------------------------------------
Today / Unread since last visit                  | Selected message
                                                | Origin / author / time
Signal · Maya · Warehouse                        | Related messages
  Is Thursday confirmed?                        | Evidence / attachments
  [2 replies] [Shipment]                        | Branch / tags / history
                                                |
execlaw · Draft for Signal                       | Actions, shown on demand
  Thursday is confirmed.                        | No automatic sending
  [Based on 2 messages] [1 tool result]           |
  [Review draft]                                |
-----------------------------------------------------------------------
[Replying to Maya in Signal · Warehouse] [Clear target]
[Message composer                                         ] [Send]
```

This is an interaction sketch, not a screenshot of implemented behavior.
Counts, receipt states, selected recipients, and evidence references must
come from actual state; placeholders in a design must never become invented
production facts. On narrow screens the inspector becomes a sheet, secondary
filters move into a drawer, and the source row scrolls within its own region.

Suggested design tokens are starting points to validate, not fixed branding:

| Element | Proposed treatment | Constraint |
|---|---|---|
| Surfaces | Quiet canvas, slightly raised message/inspector surfaces, subtle borders; light and dark variants | Maintain readable contrast without relying on transparency |
| Typography | Existing IBM Plex Sans; approximately 15-16 px body, 1.5 line height, bounded reading width | Respect zoom and user font settings; never shrink essential labels to fit |
| Message hierarchy | Author first; compact channel/account/group chip; timestamp secondary; event ID in inspector | External senders must not be presented as the operator |
| Corners and spacing | Consistent 8/12/16 px rhythm, modest rounded cards, compact related-message previews | Density can change spacing, not hide approvals or change message meaning |
| Source accents | Muted tinted edge and icon chip; stable palette with visible text labels | Separate source identity from success/error/warning colors |
| Relationship edges | Thin directional connectors near a selected message; labelled edge type and provenance | Do not draw edges through body text or render all edges by default |
| Motion | Short functional transitions for expansion/selection; no repeated ambient animation | Respect reduced motion and a Nexus animation setting |
| Depth | Optional subtle shadow on overlays, never heavy blur behind readable text | Solid-surface fallback for forced colors and low-power devices |

Modern patterns worth adapting include scoped search with jump-to-message
results and explicit facets, as documented in
[Teams message search](https://support.microsoft.com/en-us/teams/chat/search-for-messages-and-more-in-microsoft-teams).
Apply those interaction ideas locally rather than copying branding or assuming
another product's behavior proves usability here. Follow W3C guidance on
[color-independent information](https://www.w3.org/WAI/WCAG22/Understanding/use-of-color),
[text contrast](https://www.w3.org/WAI/WCAG21/Understanding/contrast-minimum),
[target sizing](https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html),
and [reducible interaction animation](https://www.w3.org/WAI/WCAG22/Understanding/animation-from-interactions.html).

### Settings and disable behavior: required contract

Keep **Settings -> General -> Chat appearance -> Classic / Nexus** as the
master selector. Classic means Nexus presentation is off. Do not introduce a
second contradictory master switch. Add an authentic live preview and expose
the following settings only as subordinate Nexus preferences:

| Preference | Proposed initial behavior |
|---|---|
| Default view | Timeline; source lanes and relationship map are opt-in |
| Density | Comfortable; Compact and Focus are optional |
| Relationship overlay | Selected message only; All visible is a bounded optional mode |
| Source labels and icons | On; essential origin/recipient identity cannot be hidden by a decorative toggle |
| Source tint / background grid | Subtle tint on; decorative grid off |
| Inspector | Closed until selected; user may pin it on wide screens |
| Motion | Follow system preference; explicit Reduced/Off can further restrict motion |
| Tool activity | Compact summaries; existing tool visibility remains independently available |
| Event IDs / diagnostic metadata | Inspector by default; optional inline display |
| Reset Nexus appearance | Restores presentation defaults, never deletes annotations or messages |

Switching to Classic must immediately show the complete Classic message set,
clear the effects of Nexus-only collapse/dimming/layout, retain typed drafts
and selected reply targets, and preserve a sensible reading anchor and focus.
Disabling Nexus must not delete saved views, tags, branches, links, messages,
or delivery state. Re-enabling may restore the saved Nexus presentation.
Toggling appearance must never start inference, change trust, mutate transport
configuration, or send a message.

Document preference ownership explicitly. Preserve the existing browser-local
behavior until a migration is implemented. If durable account defaults or
cross-device preferences are added, persist them in SQLite with optional
device overrides; browser storage is only a cache/local presentation choice,
not a new shared configuration authority. Temporary focus and scroll remain
view state. None of these controls is a privacy or authorization boundary.

### Graphics implementation notes

Prefer CSS grid/flex layouts and theme tokens for the reading view, with
feature-detected container queries for adaptive panels. Use a bounded SVG
overlay for selected-message connectors, with text labels and patterned
strokes; decorative edges should not intercept pointer input. The SPA already
includes React Flow for graph editing, so evaluate reuse for the optional
relationship lens before adding another graph engine. Its availability is not
evidence that a large chat graph will be fast or accessible.

Start the map with a proposed cap of 60 nodes/120 edges around the selected
message, an explicit Show more action, and a persistent count of omitted
neighbors. Treat that cap as a tunable design budget to test. Preserve node
positions while streaming; do not rerun a global layout for every token.
Use semantic HTML for message content and the accessible relationship list;
avoid canvas-only text or an animation loop that runs when Nexus is off.

Example of the proposed relationship vocabulary, only when explicit references
exist in stored data; this does not assert relationships in the baseline fixture:

```mermaid
flowchart LR
    R["execlaw draft: not sent"] -->|generated_from| W["WhatsApp input: stable source ID"]
    R -->|generated_from| S["Signal input: stable source ID"]
    R -->|generated_from| T["Tool result: paired call ID"]
```

The three input types retain different identity chips. A draft's originating
channel, evidence sources, and chosen outbound destination are separate fields.
Lines explain a relationship; they neither authorize a send nor prove delivery.

### Nexus enhancement backlog

Priorities and effort use the H-roadmap definitions. NX items refine existing
H work rather than replacing it; for example, authorization remains H026,
delivery truth H011/H024, and accessibility/locales H050/H129.

<a id="nexus-nx01"></a>

**NX01 - Reliable master toggle and complete Classic restoration. P0 / M.**
Gate Nexus-only branch/group hiding on the selected appearance and test all
collapse/filter combinations. Add preview/reset descriptions to the current
setting. **Accept:** switching off while a branch or group is collapsed restores
every eligible Classic message, preserves composer text and reply target, and
works across mounted views/tabs and storage failure. Re-enable without losing
annotations. Fix NXF01 before further visual rollout.

<a id="nexus-nx02"></a>

**NX02 - A cohesive theme and user-controlled visual preferences. P1 / M.**
Implement the token system and subordinate settings above, including density,
source tint, event-ID visibility, inspector, and motion. Define device versus
account ownership and version preference migrations. **Accept:** light/dark,
system theme changes, invalid stored values, reset, and Classic fallback work;
no visual setting changes policy or erases organization data.

<a id="nexus-nx03"></a>

**NX03 - Distinct author, channel, account, and destination identities. P1 / M.**
Replace the overloaded source concept with a readable metadata hierarchy:
who wrote it, where it arrived, which remote conversation/account it belongs
to, and where a draft would go. Use avatars/initials plus labelled source chips.
**Accept:** two Signal accounts, equal WhatsApp/Signal group names, unknown
plugins, tools, and multiple local agents stay distinguishable without color.

<a id="nexus-nx04"></a>

**NX04 - Compact adaptive toolbar with progressive disclosure. P1 / M.**
Keep source lens, search, view switch, and active-filter count visible. Move
saved-view editing, ordering, and secondary facets into an accessible drawer.
**Accept:** at 320/390 px, default controls do not consume roughly a third of
the screen as in the baseline; message/composer space remains useful at zoom.
Every hidden control has a visible, keyboard-operable way to reach it.
Use the live-review default mobile toolbar budget of 96 px below the thread
header, with saved-view editing closed and a compact separately tested error
state. The observed 205-238 px toolbars are the starting baseline.

<a id="nexus-nx05"></a>

**NX05 - Source lenses with explicit filter versus highlight behavior. P1 / M.**
Offer labelled source chips and a choice between highlighting matches in context
and showing only matches. Add optional source swimlanes with a shared time axis,
synchronized selection, and a narrow-screen list fallback; the canonical
timeline retains event order. Display active facets and a single clear action.
**Accept:** an absent source on the newest page remains selected rather than
turning into All sources; counts identify loaded versus full-history scope;
switching between lanes and timeline preserves selection and chronology;
changing a lens never merges threads or changes outbound destination.
Separate incoming transport counts from execlaw/tool roles. Replace ambiguous
`0 / N` chrome with explicit loaded/matching/selected-match labels; a
WhatsApp-only view should prioritize participants and remote-chat identity.

<a id="nexus-nx06"></a>

**NX06 - Readable execlaw response and turn cards. P1 / M.**
Group the final answer, its declared source messages, artifacts, and compact
tool activity into a turn presentation with a clear author and draft state.
Keep chronological event details expandable. **Accept:** responses to several
inputs show their real references; errors and approval waits cannot disappear
inside a collapsed turn; no private chain-of-thought is invented or exposed.
Mark known persisted development/simulation output explicitly. Group compatible
consecutive messages only with stable author/account/chat identity and a bounded
time interval, retaining per-message access and source chronology.

<a id="nexus-nx07"></a>

**NX07 - Focused relationship connectors within the timeline. P1 / M.**
Draw bounded gutter connectors for the selected message and its immediate
parents/replies, synchronized with keyboard focus. Use an inline relation
list where geometry is cramped. **Accept:** connectors follow stable event
identities through resizing, collapse, and virtualization; they never obscure
text or falsely connect adjacent messages because they share a color.

<a id="nexus-nx08"></a>

**NX08 - Optional relationship map with a readable list equivalent. P2 / L.**
Add a stable, bounded neighborhood view, not an always-running force graph.
Support select, zoom-to-selection, fit, and return-to-timeline; load more only
with clear scope. **Accept:** every displayed edge has a recorded source;
keyboard/screen-reader users can inspect the same relationships in a list;
large histories start with a bounded subset and preserve selection across views.
When relationship data fails to load or a successful query has no edges, retain
readable messages and distinguish those states instead of fading the entire view.

<a id="nexus-nx09"></a>

**NX09 - Relationship provenance and direction as a visual grammar. P1 / M.**
Distinguish transport-native/persisted causal replies, operator annotations,
and legacy inferred context using text and line patterns as well as color.
Consolidate duplicate representations. **Accept:** a proximity-derived link
cannot look operator-confirmed; relation type, direction, author/source, and
revision are inspectable. Visual certainty must not exceed the data contract.

<a id="nexus-nx10"></a>

**NX10 - Stable group and causal IDs independent of display labels. P1 / L.**
Provide transport/account/remote-conversation IDs and explicit response/input
references from the backend. Preserve honest legacy fallbacks. **Accept:**
equal group labels stay separate and the same response retains its identity
and causal links across newest, centered, small, and incremental pages.
Native reply relationships and manual organization remain separate fields.

<a id="nexus-nx11"></a>

**NX11 - Follow relationships into unloaded history and return. P1 / M.**
Extend the existing bounded centered-window loader to reply/forward/mention
targets. Show Load source, loading, deleted, unavailable, and denied states
instead of a permanently disabled generic link. **Accept:** loading expands
the necessary branch/group, focuses the target, and offers Back to response;
inaccessible content and its private metadata are not leaked through errors.

<a id="nexus-nx12"></a>

**NX12 - Consistent full-history search and saved-view scope. P1 / M.**
Unify canonical source IDs and supported tag/branch/type/date facets across
local navigation and server search, or visibly identify unsupported scopes.
Version saved-view fields and cancel obsolete requests. **Accept:** late results
cannot overwrite a newly selected view; saved branches either round-trip or
are explicitly transient; results never silently widen the selected scope.
Qualify unavailable/mismatched backend responses: HTTP 200 HTML is not a valid
organization result. Detect content/schema mismatch and retain a useful chat
view with bounded retry and a diagnostic detail surface.

<a id="nexus-nx13"></a>

**NX13 - A source-aware thread sidebar with honest unread state. P1 / M.**
Add optional source/account sections, pinned threads, unread counts, pending
drafts, and running/failed work indicators. Keep thread activation, rename,
and menu controls as distinct accessible targets. **Accept:** a mixed-source
Control thread is labelled as mixed, not duplicated into misleading independent
threads; badges are backed by state and grouping changes no conversation identity.

<a id="nexus-nx14"></a>

**NX14 - Branch summaries that expose context and new activity. P1 / M.**
Refine existing branch collapse into compact summaries with source icons,
loaded/total counts when known, last activity, and pending items. Support
preview/expand without rearranging canonical chronology. **Accept:** collapsing
an interleaved branch leaves unrelated messages in order and does not hide its
unresolved approval/failure indicator; inferred topic suggestions need confirmation.

<a id="nexus-nx15"></a>

**NX15 - Reading-position marker and optional activity overview. P2 / M.**
Add date separators, a reliable unread divider, and an optional timeline
overview showing activity by source/time. Distinguish loaded ranges and gaps.
**Accept:** jumping via the overview loads an authorized bounded window, not
all history; new arrivals update counts without moving a reader; unseen gaps
are not rendered as proof that no messages occurred.

<a id="nexus-nx16"></a>

**NX16 - Event-backed delivery strip with clear next actions. P1 / M.**
Refine the existing draft/requested/accepted/delivered/failed labels into a
compact textual state strip with receipt time and detail on demand. Include
unknown outcomes and cancellation accurately. **Accept:** no delivery receipt
means no delivered checkmark; retry is a consequential action, not a navigation
button; appearance changes do not change transport state. Extends H011/H024.

<a id="nexus-nx17"></a>

**NX17 - Stable streaming and visible work states. P1 / M.**
Give execlaw a consistent response shell while tokens arrive, with concise
model/tool/waiting-for-review/interrupted states derived from runtime events.
Avoid layout jumps as Markdown, tables, and citations settle. **Accept:**
partial output never looks like a finished or sent answer; Stop and queued
corrections remain reachable; announcements summarize changes rather than
reading every token aloud. Extends H035/H043.

<a id="nexus-nx18"></a>

**NX18 - Compact, inspectable tool activity and result previews. P1 / M.**
Replace default walls of raw JSON with bounded typed summaries where schemas
support them; retain a copyable raw view for authorized inspection. Show tool
identity, duration, outcome, and attached artifacts. **Accept:** summaries do
not hide failures or claim effects succeeded prematurely; unknown plugin output
has a safe text fallback; the existing tool-visibility setting still works.

<a id="nexus-nx19"></a>

**NX19 - Evidence-linked attachment and citation cards. P1 / M.**
Use consistent file/image/report cards with filename, size, source, preview,
and authorized download state; support source snippets and page/region links
when real evidence exists. **Accept:** expired access gives a recoverable state,
not an invisible attachment; previews match verified bytes; remote content
cannot introduce scripts or automatic tracking requests. Extends H091/H122.

<a id="nexus-nx20"></a>

**NX20 - One lazy selected-message inspector. P1 / M.**
Move detailed metadata and organization editing out of every message row into
a single responsive panel. Display identity, source event, relationships,
branch/tags, artifacts, and permitted actions. **Accept:** closed inspectors
do not mount all-message selectors per row; selecting another event updates
scope safely; unavailable fields are labelled rather than inferred.
Closed organization UI must contribute zero related-message options; an open
inspector uses bounded target search. The live TrueNAS baseline is 40,800
organization options for 200 rendered messages. Show read-only/unavailable
capability states rather than presenting editing controls that cannot work.

<a id="nexus-nx21"></a>

**NX21 - Searchable relationship editing with revision and undo. P1 / M.**
Replace the all-message dropdown with scoped target search and a preview.
Add saving/saved/failed states, dirty-edit protection, conflict detection,
and audited undo for annotation changes. **Accept:** concurrent tabs cannot
silently overwrite one another; a failed save keeps the draft; undo edits
organization only, never the signed original message or an external send.

<a id="nexus-nx22"></a>

**NX22 - Comfortable, Compact, and Focus reading modes. P1 / S.**
Offer controlled spacing and metadata density presets. Focus emphasizes the
selected turn without fading body text below readable contrast; Compact
reduces repeated chrome while leaving safety states visible. **Accept:**
switching modes preserves message content, order, selection, and composer
draft; long responses remain readable without mandatory truncation.
Use a compact continuation treatment for compatible consecutive same-author
messages, reducing repeated provider labels and organization controls without
merging their event identities. Retain full metadata in the inspector.

<a id="nexus-nx23"></a>

**NX23 - Restrained motion with effective off controls. P1 / S.**
Use short expansion and selection transitions only where they clarify change.
Respect system reduced motion in JavaScript scrolling as well as CSS, and
allow an explicit Nexus motion-off preference. **Accept:** scroll-to-latest,
map movement, drawers, and selection avoid smooth motion when disabled;
streaming produces no repeated pulse or ambient animation requirement.

<a id="nexus-nx24"></a>

**NX24 - Accessible contrast, keyboard routes, and status announcements. P1 / M.**
Keep normal text at a measured 4.5:1 minimum contrast and qualify large text
and meaningful controls under the applicable WCAG criteria. Prefer reducing
decoration over fading entire messages. **Accept:** non-color identity cues,
visible focus, labelled controls, forced-colors mode, logical keyboard order,
and a graph/list equivalent pass automated and manual checks in both themes.

<a id="nexus-nx25"></a>

**NX25 - Bounded rendering and scroll-anchor preservation. P1 / L.**
Precompute relationship/group indexes, lazy-mount editors, and window long
histories while retaining searchable event IDs. Budget layout and streaming
updates on named reference hardware. **Accept:** 100/1,000/10,000-event
fixtures keep bounded mounted rows and responsive selection; appending,
prepending, image loads, and branch expansion preserve the reading anchor.
Measure before and after; do not claim performance from CSS changes alone.
Include the observed 200-message single-transport workload with 40,800 hidden
editor options in the regression corpus; this DOM count is a scaling baseline,
not a measured latency result. Remove the quadratic structure before graph work.

<a id="nexus-nx26"></a>

**NX26 - Mobile-first source navigation and inspector sheets. P1 / M.**
Use compact chips, an accessible filter sheet, and deliberate touch targets
instead of squeezing desktop controls. Keep the keyboard/composer from
covering selected content and provide non-drag alternatives to map actions.
**Accept:** 320/390 px, landscape, zoom, and virtual-keyboard fixtures preserve
essential actions. Target 44 px for primary touch actions while meeting the
applicable minimum-size/spacing rules for smaller secondary controls.
At 320/390 px, target at least 85% viewport width for normal text bubbles by
removing compounded group/rail/alignment indents. Reserve jump-to-latest space
above the composer so the control does not cover message metadata; test virtual
keyboards and safe areas separately from desktop viewport emulation.

<a id="nexus-nx27"></a>

**NX27 - Localized and bidirectional message metadata. P1 / M.**
Move remaining Nexus labels/errors into the translation system; use logical
CSS properties and isolate mixed-direction names, IDs, and timestamps.
**Accept:** long translations, Arabic/Hebrew mixed with Latin identifiers,
CJK text, and locale-specific dates fit without ambiguous identity or action
labels; relation enums and stored source IDs remain locale-independent.
Extends H129 beyond translating only the appearance selector.

<a id="nexus-nx28"></a>

**NX28 - Authorized reads and privacy-safe visual metadata. P0 / M.**
Apply consistent message/conversation authorization to ordinary, paginated,
centered, search, annotation, and graph reads. Scope counts, previews, exports,
and saved-view ownership; show editing only to authorized roles. **Accept:**
denied callers receive neither messages nor private relationship metadata;
unauthorized controls are absent; toggling Classic never bypasses access.
Close NXF10 through H026 before broadening the visualization's data surface.

<a id="nexus-nx29"></a>

**NX29 - A visually explicit draft-review and reply-target panel. P1 / M.**
Before transport sending, show destination account/group, audience, source
message, freshness, and editable draft versus approved revision. Keep the
target visible above the composer. **Accept:** selecting a source lens cannot
reroute a reply; stale messages/audience changes require renewed review;
switching appearance preserves the draft and target without initiating send.
Extends H053/H100/H127.
Place destination/review actions in a compact footer distinct from answer text.
In a web-origin conversation, sending externally is an explicit forwarding
operation, not an inferred reply. Do not offer known synthetic/stub output as
an ordinary ready-to-send response; any conversion to an operator-authored
draft must be deliberate and pass the normal recipient/review gates.

<a id="nexus-nx30"></a>

**NX30 - Visual regression and usability qualification for real state combinations. P1 / M.**
Extend existing fixtures with collapsed-state toggle restoration, search races,
equal-name groups, old replies, non-Controller roles, unknown sources,
delivery failures, reduced motion, and large histories. **Accept:** assertions
check content, focus, scope, and performance alongside screenshots. Run task
studies for locating an origin, following a reply, finding unread messages,
and reviewing a draft; publish results before calling the redesign better.
Add live-derived synthetic cases for a four-message web/stub history, a
200-message WhatsApp-heavy window, 200 HTML instead of organization JSON,
unavailable/empty relationships, short same-author bursts, and crowded mobile
review controls. Preserve privacy by recreating layout characteristics rather
than committing the operator's original transcripts into tests.

### Nexus delivery and release gates

Deliver NX01/NX28 and correctness fixes first; then prioritize
NX04/NX20/NX22/NX25/NX26 for the live-observed reading and rendering problems.
Carry NX02 preferences and NX24/NX27 accessibility/localization through those
slices. Follow with NX03/NX06/NX12/NX16/NX29 for identity, degraded states,
and response review, then richer turn/evidence presentations.
Implement connectors and the bounded map only after stable identities,
relationship provenance, authorized loading, and rendering budgets pass.
Keep NX30 qualification alongside every slice, not as a final screenshot pass.

No Nexus feature is qualified until the Classic-off contract, unread/scroll
continuity, keyboard access, light/dark contrast, localized labels, truthful
delivery state, and authorized data scope pass together. Core fixes apply to
both appearances even if discovered during Nexus work. The review findings
for this section are recorded after F19 under NXF01-NXF11. The
[live deployment review](#nexus-live-review) revises visual priorities using
authenticated desktop/mobile captures and measured loaded-window geometry.

## Review findings and unresolved verification

Findings below describe the reviewed working tree on 2026-09-27. Severity is
reviewer triage, not a CVSS assessment. "Source-verified" means the relevant
implementation and call path were inspected; no attack was executed against
the operator's running service. Feature selection was additionally checked
with `cargo tree -p execlaw -e features -i libsqlite3-sys --offline`. Runtime,
browser, SQLCipher, process-kill, and local-model benchmark suites were not
run for this documentation-only change. Suggested tests are remediation
acceptance criteria, not claimed passing tests. Source line references are
snapshot coordinates and may move.

<a id="finding-f01"></a>

### F01 - Critical: plugin lifecycle endpoints lack authentication

**Evidence:** `crates/server/src/plugins.rs:154`, `:429`, `:457`, and `:497`
define install/enable/disable/uninstall handlers without an authentication
extractor. `plugins_router` at `:610` has no authorization layer;
`crates/server/src/routes.rs:938` merges it directly. The outer layers at
`:1001-1040` provide diagnostics/security headers, not authentication, and
`crates/cli/src/main.rs:2750-2760` serves that router.

**Impact and conditions:** a caller able to reach the API can change plugin
lifecycle state. Uninstall purges sidecars, OAuth/vault rows, artifacts, and
plugin state. Loopback binding reduces remote reachability but does not
authenticate local callers. Provenance checks constrain some installs, not
these destructive lifecycle operations.

**Fix/gate:** require Controller authorization before any mutation and protect
inventory appropriately. Test anonymous, Viewer, Operator, and Controller
requests through the complete router; denied requests must leave filesystem,
DB, runtime, and sidecars unchanged. Roadmap 26.

<a id="finding-f02"></a>

### F02 - Critical: plugin destination traversal and destructive failed upgrades

**Evidence:** `crates/plugin-sdk/src/manifest.rs:715-727` restricts plugin ID
but only requires a nonempty version. `crates/server/src/plugins.rs:181-209`
constructs `{id}-{version}` under the stage root and recursively removes an
existing upgrade target. Provenance authorization occurs later at `:233-246`.

**Impact and conditions:** version separators and parent components can escape
the stage root when the necessary prefix directories exist, exposing writable
host paths to deletion/replacement. ZIP-entry path checks do not protect this
separate destination. Independently, an ordinary same-version upgrade rejected
by provenance has already removed/replaced the old installation. F01 makes
this API reachable without login; fixing F01 alone does not make malicious
Controller-uploaded archives safe.

**Fix/gate:** validate safe version components, enforce destination containment
including symlink/junction handling, and authorize in isolated staging before
atomic promotion. Test traversal variants and rejected/failed upgrades with
outside-root sentinels and old-plugin byte preservation. Also bound entry
count and total expanded bytes: `plugin-sdk/src/zip_stage.rs:46-74` currently
copies archive contents without expansion quotas, permitting disk/resource
exhaustion even when compressed request size is limited. Roadmap 27.

<a id="finding-f03"></a>

### F03 - High: web-fetch SSRF checks occur too late and omit DNS destinations

**Evidence:** `crates/server/src/tool_apis_http.rs:117-123` constructs a client
with automatic redirects; `:263-276` validates the final URL after sending.
The domain branch at `:212-230` does not resolve and validate IP addresses;
`:179-190` omits mapped IPv4 private/loopback addresses. Production callers
include `tool_dispatch.rs:330` and `research/runner.rs:568`.

**Impact and conditions:** influence over a fetched URL, DNS answer, or redirect
can cause requests to reachable private services. Rejecting the final response
does not undo the request; an intermediate private hop may also be hidden by
a final public URL. Existing scheme, body-size, and timeout checks still help.

**Fix/gate:** enforce public-fetch egress before each connection, resolve/pin
addresses, normalize mapped IPs, and manually validate redirects. Test private
DNS, rebinding, and public/private redirect chains; assert zero requests reach
the forbidden endpoint, not merely that the caller receives an error. Roadmap 25.

<a id="finding-f04"></a>

### F04 - High: production SQLCipher release qualification remains open

**Original review evidence (superseded in source):** `crates/core/Cargo.toml:20` defaults to plaintext SQLite;
`crates/cli/Cargo.toml:58` makes SQLCipher opt-in. The build commands in
`scripts/build-mac.sh:44`, `build-linux.sh:73`, and `build-windows.ps1:196`
omit that feature. Offline Cargo feature inspection confirms plain bundled
SQLite for the default CLI build. `crates/core/src/db.rs:128-165` issues
`PRAGMA key` without verifying cipher availability.

**Current status:** all three desktop build scripts now select the CLI
`sqlcipher` feature and packaged `doctor` runs a disposable cipher/recovery
preflight. The original review did not inspect every previously distributed
binary. Native installed-package results and a backed-up migration path for
existing plaintext installations remain outstanding; source changes alone do
not establish release qualification.

**Fix/gate:** explicit production features, fail-closed cipher checks, and
packaged-artifact encrypted-header/wrong-key/restore/rotation tests on each OS.
Provide a backed-up migration path for existing plaintext installations;
changing the build flag alone is not a data migration. Roadmap 28.

<a id="finding-f05"></a>

### F05 - High: password changes leave existing refresh sessions usable

**Evidence:** `crates/server/src/users.rs:356-367` and `:441-451` change password
hashes without revoking refresh records. Logout handlers at
`crates/server/src/routes.rs:755-780` revoke refresh tokens, while
`auth_extract.rs:112-130` validates access JWTs/user existence without checking
session revocation. The default access TTL is 15 minutes (`state.rs:57`).

**Impact and conditions:** an already-stolen refresh credential can survive a
password reset and obtain new access tokens; an access token survives logout
until expiry. Single-use refresh rotation and JWT expiry limit exposure but
do not implement credential-compromise recovery.

**Fix/gate:** revoke refresh sessions on password change/reset and enforce a
durable session/epoch check for access tokens, media, and live connections.
Specify current-session versus logout-all behavior and clear the cookie.
Tests must reject old credentials across restart without logging out unrelated
users. Roadmap 26.

<a id="finding-f06"></a>

### F06 - High: claimed outbox work has no crash-reclamation lease

**Evidence:** `crates/core/src/outbox.rs:124-150` selects only `pending` rows
and changes a claim to `in_flight` without an expiry/owner fence.
`crates/outbox/src/lib.rs:231-244` resolves the claim only after dispatch.
No production reclaim/reset path was found in the reviewed source.

**Impact and conditions:** a process exit after claim strands that row outside
the ready queue. This establishes a flaw in the relay implementation, not that
every production transport currently uses this exact path. A local inbox record
also cannot prove exactly-once acceptance by an independent remote service.

**Fix/gate:** expiring owned leases, recovery scanning, and sink-specific
reconciliation. Kill/reopen before dispatch, after remote acceptance, and
before acknowledgment. Test the actual relay and a stateful mock sink, not
only enqueue or deduplication helpers. Roadmap 23-24; item 9's broad completed
claim needs narrower evidence.

<a id="finding-f07"></a>

### F07 - High: production runner restart does not consume completed checkpoints

**Evidence:** `crates/server/src/chats.rs:2422-2438` begins `model:0` but accepts
only `StepDecision::Execute`; a completed step's `Replay` returns an error.
The round offset starts at zero (`:2372`). `RunStore::list_recoverable` in
`crates/core/src/runs.rs:420` has no callers in the reviewed source.

**Impact and conditions:** persisted primitives and in-process resume support
do not establish production host/runner resume after a completed model step.
A restart can leave work unrecovered or reject an attempted replay.

**Fix/gate:** recover pending runs at startup and reconstruct the remaining
request from durable model/tool checkpoints with correct ordinals. Terminate
real host/runner processes at model completion, tool completion, approval, and
commit; resume the same input once without redispatching completed work.
Roadmap 23. Preserve the distinction between in-process coverage and the
production runner path in item 9 and the architecture docs.

<a id="finding-f08"></a>

### F08 - High: automation dispatch can acknowledge work before it runs

**Evidence:** `crates/server/src/automation_bus.rs:300-307` documents a loss
window that remains in the implementation. At `:331`, it marks an event
dispatched before acquiring worker capacity at `:343` and running the handler.
Recovery skips rows already marked dispatched.

**Impact and conditions:** a crash after the marker, while waiting for a worker,
or during the handler can lose the event's unfinished work. Persisting the bus
event is not sufficient to make handler execution durable.

**Fix/gate:** separate leased/running/completed states and idempotent creation
of downstream automation runs. Crash at all three boundaries and verify the
intended durable run eventually completes exactly once at the local run-record
level; external effects still need their own reconciliation. Roadmap 23-24.

<a id="finding-f09"></a>

### F09 - High: default runner failure logging includes prompts and tool results

**Evidence:** `crates/runner-binary/src/turn_loop.rs:937-975` serializes the
request and logs up to 8,192 characters plus 1,024 characters of generated
text at ERROR level. Stream failures call this at `:255` and `:424`. The
optional diagnostic-directory switch controls additional files, not this log.

**Impact and conditions:** an inference failure can copy sensitive conversation,
memory, and tool-result content into ordinary host/container logs. Truncation
is not redaction. Item 18's Ollama-client sanitization does not cover this path.

**Fix/gate:** default to counts, timings, IDs, and sanitized error categories;
govern optional captures through SQLite settings with explicit retention.
Force both error paths using synthetic secrets in prompts/tool results and
assert they appear in neither logs nor default files. Roadmap 35-36 and 48.

<a id="finding-f10"></a>

### F10 - Medium: inference streaming corrupts UTF-8 and accepts incomplete output

**Evidence:** `crates/inference-api/src/lib.rs:985` decodes each network chunk
with `from_utf8_lossy`; `:987` recognizes only LF/LF frame boundaries. A
multibyte character split across chunks is corrupted, and CRLF-delimited
frames can accumulate into an invalid combined JSON payload. Separately,
`runner-binary/src/turn_loop.rs:319` accepts stream EOF, `:454` defaults the
missing finish reason, and `:523-535` checkpoints the accumulated round.

**Impact and conditions:** ordinary network chunking can alter multilingual
answers or arguments. A clean HTTP close without the protocol's completion
signal can make a truncated answer appear successful.

**Fix/gate:** bounded byte-oriented framing and explicit protocol termination.
Test every byte split, CRLF/LF, partial arguments, and EOF without terminal
state; incomplete calls must not dispatch and incomplete rounds must not
commit as successful. Roadmap 29.

<a id="finding-f11"></a>

### F11 - Medium: cancellation may not wake a stalled tool wait

**Evidence:** `crates/runner-binary/src/turn_loop.rs:661-696` enables a timer
branch with `if cancel.load(...)` inside `tokio::select!`. The guard is sampled
when that select begins, so setting the atomic later does not wake a pending
`recv()` or enable the disabled branch. This follows the documented
[Tokio select lifecycle](https://docs.rs/tokio/latest/tokio/macro.select.html).
Stream opening at `:248` also awaits without a cancellation branch.

**Impact and conditions:** Stop can remain unresponsive while a tool result
or stream-open request is stalled. A later result or timeout may eventually
release the wait; immediate cancellation is not guaranteed.

**Fix/gate:** a waking cancellation token plus propagated deadlines. Test
cancel-after-wait-start with permanently pending futures; require bounded
acknowledgment and paired cancellation audit events. State clearly that an
already-executed remote effect cannot necessarily be undone. Roadmap 23/43.

<a id="finding-f12"></a>

### F12 - Medium: initial context trimming does not bound later tool rounds

**Evidence:** `crates/runner-local/src/turn.rs:589-619` trims then adds a
summary; `:713-720` builds later requests from growing messages and `:1104`
appends tool results. The streaming runner similarly appends results at
`crates/runner-binary/src/turn_loop.rs:687-690` without per-round compaction.
`context-window/src/lib.rs` uses approximate content-size estimates.

**Impact and conditions:** several large results or an oversized summary can
exceed the model window after a valid initial request. Declaration caps and
individual response caps do not bound the combined request.

**Fix/gate:** budget complete requests at every round, preserve tool groups,
and store/retrieve large outputs as scoped artifacts. Test cumulative results,
large arguments/summaries, images, and multilingual content against the actual
serialized/model-tokenized request. Roadmap 31-32.

<a id="finding-f13"></a>

### F13 - Medium: HTTP MCP buffers bodies and waits for SSE connection closure

**Evidence:** `crates/server/src/mcp_http_client.rs:178`, `:202`, `:227`, and
`:256` read full error/SSE/JSON bodies without an application byte cap. SSE
parsing at `:201-219` begins only after `.text().await`; the JSON response
path at `:226-230` does not enforce the requested response ID.

**Impact and conditions:** a configured compromised/faulty MCP server can
consume excessive memory. A matching response on a still-open stream waits
for EOF or timeout. Local endpoint checks and the 60-second timeout constrain
exposure but do not supply a byte budget or correct incremental behavior.

**Fix/gate:** bounded incremental decoding, immediate matching-response
completion, strict version/ID checks, and explicit session lifecycle. Test
oversized success/error bodies, wrong IDs, and an open SSE connection after
the matching result. Roadmap 29/45.

**Closure (2026-09-30):** HTTP JSON and SSE bodies are consumed incrementally
under the response cap; protocol and response IDs are validated, and the
client returns as soon as the matching SSE response arrives. Session expiry
reinitializes without replaying a tool call. Seven HTTP MCP tests and the
15-test stdio client suite passed, including bounded-response, wrong-ID,
session-expiry, open-SSE, and pinned-version fixtures.

<a id="finding-f14"></a>

### F14 - Medium: plugin UI trust is broader than tool capability trust

**Evidence:** `web/src/settings/DynamicPluginPanel.tsx:162-176` imports panel
JavaScript into the host origin; `web/src/plugins/host-bridge.ts:74-79`
provides a raw-token accessor; `web/src/auth/tokens.ts:32-33` persists tokens
in localStorage.

**Impact and conditions:** an installed and loaded malicious/compromised panel
has the operator browser's authority. This is the existing trusted-plugin UI
model, not an independently demonstrated unauthenticated exploit. Tool
capability narrowing and publisher provenance do not isolate that JavaScript.

**Fix/gate:** disclose present authority at installation and add isolated panels
with scoped host RPC. Cookie changes alone do not stop same-origin code from
making authorized requests. Test that a panel cannot obtain bearer credentials,
read another plugin's state, or call unrelated admin mutations. Roadmap 27.

<a id="finding-f15"></a>

### F15 - Closed: a slow STT request no longer holds a shared voice-session lock

**Resolution:** the runtime map mutex now protects only lookup and session
lifecycle; each session owns its STT/TTS state mutex. The regression
`voice_runtime::tests::stalled_stt_flush_does_not_block_another_sessions_interrupt`
holds one session's STT flush and confirms another session's interrupt returns
within 500 ms. H047 remains open for continuous endpointing, incremental STT,
first-turn voice routing, and supported-hardware qualification.

<a id="finding-f16"></a>

### F16 - Medium: release evidence and documentation overstate some guarantees

**Evidence:** `.github/workflows/ci.yml` runs build/tests and SPA checks but no
Criterion baseline comparison or SQLCipher test tier. The policy benchmark
budget in `crates/policy/benches/policy_hot_paths.rs:4` is therefore not an
enforced CI budget. `docs/remaining-improvements-todo.md:19-25` records an
unrun SQLCipher test and seven earlier server-suite failures; those failures
were not reproduced in this review and remain an unresolved evidence item.

The evaluation CLI's mock mode copies expected answers
(`crates/eval-harness/src/main.rs:108-115`), while its live mode judges rubric
prompts rather than running the real harness. Skill evaluation disables tools
and scores substring matches (`server/src/skills_admin.rs:240`, `:266`). These
tests are useful for their stated scope but cannot establish task success or
comparative leadership. Existing browser scripts also need promotion into
real-backend release journeys.

Older strategy statements conflict with newer code: the assertion that HMACs
are only independent row signatures in `execlaw_impr_doc.md` must not be
repeated as a current flaw without accounting for the versioned chaining code;
its durable-run/local-policy/memory TODOs also lag later implementations.
`docs/agent-model.md` says normal chat does not use the step store near line 61,
then documents durable execution in section 3.1. `docs/testing.md:56` describes
an unwired extraction worker although `crates/cli/src/main.rs:2225-2226`
starts it. Treat source and path-specific tests as evidence, not checkboxes.

**Fix/gate:** attach each completed claim to a test path, execution mode,
artifact, and last verified result. Reconcile the historical failures; add
production artifact tests and performance gates; distinguish unit/reopen
coverage from full process recovery, and mock evaluation from task quality.
Update conflicting docs together when the corresponding fixes land.
Roadmap 21/23/28/39/49/50. All findings above remain open until remediation
and the specified verification are completed.

<a id="finding-f17"></a>

### F17 - High: power-loss durability is weaker than process-crash recovery

**Evidence:** `crates/core/src/db.rs::apply_init_pragmas` selects WAL and
`synchronous=NORMAL`. SQLite explicitly distinguishes this from commits
synchronized on each transaction: recent committed transactions can roll
back after power loss or hard reset under that setting. See the
[SQLite WAL performance/durability discussion](https://sqlite.org/wal.html).
This is a verified configuration/contract mismatch, not a reproduced disk
failure or a claim that ordinary process termination always loses data.

**Impact and conditions:** if an externally accepted effect follows a locally
acknowledged transaction that subsequently disappears after a hard reset,
recovery can lack the intent/result needed for reconciliation. Passing
process-kill tests alone cannot establish power-loss durability. Actual
guarantees also depend on the filesystem and storage device honoring sync.

**Fix/gate:** define and enforce an effect-critical persistence boundary before
dispatch, evaluate `FULL` or an equivalent supported design, and measure its
cost. Use disposable storage-fault/hard-reset tests with a separately observed
mock sink; report the guarantee and hardware assumptions accurately. Roadmap
58, alongside 23-24. No durability setting was changed in this review.

<a id="finding-f18"></a>

### F18 - High: automation test runs can perform live effects without a run row

**Evidence:** `crates/server/src/automations_admin.rs:499-504` constructs the
test-run executor with the live database and plugin host. In
`automation_runtime.rs:318-340`, `dry_run` uses the same graph executor with
an in-memory trace sink; it avoids a durable automation-run row, not the
effects themselves. Node dispatch at `:528-530` still invokes `Notify`,
`CallPlugin`, and `HttpFetch`. The SPA exposes this through the Test run drawer
in `web/src/settings/AutomationDetailPage.tsx`.

**Impact and conditions:** an authorized operator testing a graph containing
those nodes can create production alerts and invoke live plugin/network
actions. This finding concerns the unsafe preview boundary and missing
durable run attribution even for intended operator use; the separate
authentication failure is recorded in F19. The current code documents
real model use during a test but does not isolate other effects. No live
workflow was invoked during this review.

**Fix/gate:** use an explicit simulation dispatcher and disposable state that
record proposed effects but cannot reach live effectful services. Keep an
intentional live integration test as a separate clearly labelled operation
with normal policy, approval, outbox, and run-record guarantees. Tests must
assert zero production writes and zero requests at a live mock sink for
dry/shadow execution. Roadmap 126.

<a id="finding-f19"></a>

### F19 - Critical: unauthenticated automation execution grants Controller tool authority

**Evidence:** `crates/server/src/automations_admin.rs:44-73` has no router
authorization layer; `crates/server/src/routes.rs:956` merges it directly.
The create handler at `automations_admin.rs:236-253` and test-run handler at
`:444-512` have no authentication extractor. Test-run supplies the live plugin
host and a caller-selected sample to the graph executor. In
`automation_runtime.rs:529`, `CallPlugin` reaches the call at `:684` with
wildcard capabilities and `Some("Controller")`. In
`crates/plugin-host/src/host.rs:1343-1362`, those values bypass capability
restrictions and satisfy tool trust floors. OpenAPI bearer declarations are
documentation, not an authentication layer.

**Impact and conditions:** a caller able to reach the API can create a graph
and test-run an enabled registered plugin tool with caller-selected arguments
under Controller tool authority. This requires a suitable installed/enabled
tool; plugin-specific validation and available downstream credentials still
constrain successful actions. No available LLM, real bus event, or plugin
installation permission is needed for this path. Returned dry-run traces do
not create a durable automation-run history row. This was verified by tracing
source, not by calling the operator's API.

**Fix/gate:** require Controller authorization before automation mutations,
sensitive reads, and test-run; apply normal capability/effect policy to live
execution; separate effect-disabled simulation as in F18. Complete-router
tests must prove anonymous/lower-role callers cannot create definitions or
dispatch tools, and simulations cannot create external effects. Intentional
live tests need normal approval/outbox/audit guarantees. Roadmap 26/52/126.

<a id="nexus-review-findings"></a>

## Nexus review findings and verification

These are scoped additions to F01-F19, not replacements. Source coordinates
refer to the reviewed implementation. The existing browser fixture and 62
focused Vitest tests passed; a separate temporary browser probe using the
same synthetic fixture reproduced NXF01. That initial review did not access
operator conversations. The subsequent [live deployment review](#nexus-live-review)
used operator-authenticated sessions for captures and read-only response
inspection; it did not probe authentication bypasses or send messages. No
application fix was made in these documentation passes. Security findings
remain open despite successful mocked frontend checks or authenticated visits.

<a id="nexus-finding-nxf01"></a>

### NXF01 - High UX correctness: Classic can retain Nexus-hidden messages

`web/src/chat/MessageStream.tsx:603` skips collapsed branch members without
checking `nexus`; group collapse at `:606` and subsequent rendering also remain
active independently of appearance. The existing browser test expands its
branch before switching to Classic, so it does not exercise this state.

A separate mocked browser probe collapsed the Shipment branch and switched
off Nexus through the existing storage notification: message #2 had **zero
rendered rows in Classic**, where one was expected. This is hidden presentation,
not evidence of deletion. Gate both collapse mechanisms by appearance and
test branch/group/mixed states, drafts, focus, and storage synchronization.
Tracked by NX01/NX30. The fix is not implemented here.

<a id="nexus-finding-nxf02"></a>

### NXF02 - Medium: saved views and full-history filters can disagree

At `MessageStream.tsx:532-539`, applying a saved view changes source/query
without invalidating in-flight search IDs or clearing prior results; it also
retains the branch filter, which the view does not save. At `:225-227`, a
source absent from loaded messages becomes an empty effective source, which
also widens the full-history request at `:328`. Local navigation applies
tag/branch/type and substring matching, while `chats/nexus.rs` search accepts
query/source/before/limit with tokenized matching. Tool actors used as display
sources are not the same source IDs used by the search index.

Reproduce with delayed responses and a saved WhatsApp view whose newest page
contains only Signal. Test that the view retains scope, old results are ignored,
and unsupported facets are labelled or implemented consistently. Use canonical
source IDs distinct from display labels. Source-verified, not a live search
reproduction. Tracked by NX05/NX12/NX30.

<a id="nexus-finding-nxf03"></a>

### NXF03 - Medium: equal display labels can merge distinct transport groups

`crates/server/src/chats.rs::conversation_group_label` at `:5626-5657` returns
a human-readable name/handle. Frontend grouping compares only that string in
`MessageStream.tsx:239-243` and `:615`. Adjacent WhatsApp and Signal groups
with the same label can therefore share a visual group. This does not prove
backend conversation identities are merged; the problem is misleading visual
attribution and collapse scope.

Supply a separate transport/account/remote-conversation identity and use it
for grouping while preserving friendly labels. Test same-name groups across
sources/accounts and renamed groups. Tracked by NX03/NX10/NX13.

<a id="nexus-finding-nxf04"></a>

### NXF04 - Medium: inferred reply context looks authoritative and changes by page

`crates/server/src/chats.rs:5418-5445` selects a page before `:5452-5494`
infers the most recent inbound context and `reply_to_seq`. A one-message page
containing just a response loses the preceding input linkage. This is not a
persisted causal reference, especially with interleaved incoming traffic.
`MessageStream.tsx:1033-1055` renders explicit annotations and legacy reply
context similarly and can show both representations.

Persist actual causal references where available, label legacy context as
inferred, and deduplicate only semantically equivalent representations. Test
identical messages across tiny/centered/default pages and interleaved sources.
Do not build a convincing graph on top of guessed edges. Tracked by NX06-NX10.

<a id="nexus-finding-nxf05"></a>

### NXF05 - Workflow limitation: old relationship targets are not loadable from links

At `MessageStream.tsx:1033-1047`, relationship buttons are disabled when their
targets are outside the loaded set. Search hits already support bounded
centered loading at `:338-346`, but relation navigation does not reuse it.
Consequently a valid stored link may be unusable in a long conversation.

Provide an explicit authorized Load source action and distinct missing,
deleted, denied, and loading states. Test return navigation and expansion of
hidden ancestors. This is an existing capability limit, not fabricated
evidence of data loss. Tracked by NX11/NX25.

<a id="nexus-finding-nxf06"></a>

### NXF06 - Accessibility risk: relationship dimming reduces body-text contrast

`web/src/styles/theme.scss:1124` applies `opacity: 0.48` to the entire unrelated
message. That fades readable text, metadata, and controls, not just decoration.
Compositing the declared light-theme body color against the declared canvas
gives approximately 3:1 after dimming, below the normal-text 4.5:1 target;
muted text is lower. This is a token-based estimate, not a complete rendered
contrast audit across every tinted surface.

Measure actual rendered pairs in both themes, focus/filter states, and forced
colors. Reduce borders/background prominence instead of body-text opacity.
Passing geometry screenshots does not establish accessibility conformance.
Tracked by NX22/NX24/NX30.

<a id="nexus-finding-nxf07"></a>

### NXF07 - Performance: hidden organization editors scale quadratically

Every message mounts a closed organization `<details>` whose target selector
maps almost every loaded message (`MessageStream.tsx:1235`, `:1247-1250`).
With N fully rendered messages this creates N*(N-1) candidate options, before
placeholder and relation options. Repeated index/slice/relationship scans at
`:607-622` and `:658-660` add render work. The synthetic collapsed-branch probe
observed 104 organization option elements with eight rendered messages and
nine loaded messages; large-history timing was not measured.

Lazy-mount one scoped inspector and indexed target search; then qualify bounded
rendering before adding map edges or animations. Test 100/1,000/10,000 events
and record DOM size, input latency, streaming cost, and memory on reference
hardware. Tracked by NX20/NX21/NX25.

<a id="nexus-finding-nxf08"></a>

### NXF08 - Interaction gaps: smooth scrolling and untranslated Nexus controls

`MessageStream.tsx:378` requests smooth scroll-to-latest unconditionally. The
inspected reduced-motion stylesheet rule at `theme.scss:2552-2556` stops
Graphify animation, not that JavaScript scroll. Nexus's type/branch/tag controls,
saved-view labels, errors, and organization UI also contain direct English
strings at `MessageStream.tsx:492-589` and `:1235-1280`, although earlier
navigation controls use translations.

Honor motion settings at the scroll call site and localize the full flow.
Test reduced-motion emulation, long translations, bidirectional names, and
keyboard-only navigation. These are source-verified omissions; no claim of a
complete accessibility or localization audit is made. Tracked by NX23/NX27.

<a id="nexus-finding-nxf09"></a>

### NXF09 - UX mismatch: organization controls do not reflect edit authority

`MessageStream.tsx:665` supplies the save callback regardless of user role,
and `:1235` uses that callback to show the organization editor. Nexus's server
organization/search routes require Controller. A non-Controller can therefore
be offered controls whose requests are rejected. Those routes' checks remain
valuable; this finding is not an annotation authorization bypass.

Render actions from actual permitted capabilities and distinguish read-only
organization from editable state. Persisted annotations are mutable records
without a full author/revision history, so a visual link must not be presented
as a signed original event. Test role-specific controls and conflicting saves.
Tracked by NX09/NX21/NX28.

<a id="nexus-finding-nxf10"></a>

### NXF10 - High security: ordinary message reads do not enforce authentication

`crates/server/src/chats.rs:5337-5353` accepts
`Result<AuthedUser, AuthRejection>` and checks it only when `around` is supplied.
Ordinary GET/limit reads continue after authentication failure.
`crates/server/src/routes.rs:875` mounts the handler without an authorization
layer; the reviewed outer layers provide diagnostics/headers, not identity.
Nexus organization/search Controller checks do not protect this separate read
path.

A caller able to reach the API and knowing a conversation ID can reach ordinary
message reads without login. This is source-verified; no live private data was
requested. Require authorization consistently for every message/history mode
and test anonymous and lower-role callers through the complete router. Denied
requests must return no private content or metadata. Extend H026's release
gate and close this before widening the visualization's data surface. Tracked
by NX28/NX30; a Classic/Nexus toggle is not a security boundary.

<a id="nexus-finding-nxf11"></a>

### NXF11 - Medium: local organization API returns HTML while editing remains offered

The authenticated local capture displays `Organization unavailable`, while
saved-view controls and per-message organization affordances remain visible.
Its conversation organization request returned HTTP 200 with `text/html` and
failed JSON parsing. The same inspection on TrueNAS returned HTTP 200 JSON
with annotations/views arrays. This is live response-contract evidence,
not an assumption that a 200 status means the feature works.

The responsible layer was not isolated: a missing route, proxy/fallback
behavior, or build mismatch could cause the observed contract. Do not label
it an authorization or migration failure without additional evidence. No
annotation save or other mutation was attempted to test the visible controls.

Require typed JSON contracts and explicit unsupported/unavailable states;
unknown API routes should not silently return a successful SPA HTML page.
Keep chat reading available, provide Retry/diagnostic detail, and disable
unavailable organization editing with an explanation. In Relationships mode,
the local capture dims all four loaded rows; retain a readable degraded view
and distinguish unavailable relationships from a successful empty result.

Regression cases: 200 HTML, malformed JSON, unsupported endpoint, authentication
failure, temporary outage, and recovery to valid JSON. Assert that message
reading and draft preservation survive, unavailable edits are not offered as
working, and retries cannot populate stale conversation state. Tracked by
NX12/NX20/NX21/NX28/NX30, with H044's client/server contract work. See
[live captures and measurements](#nexus-live-review). No fix was deployed here.
