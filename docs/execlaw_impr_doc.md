# execlaw Improvement Strategy and AI Technology Radar

Research cutoff: **2026-09-10**

This document is a research-backed improvement roadmap for execlaw. It compares
the current implementation with leading open-source agent harnesses, memory
systems, interoperability standards, and local-inference engines, then converts
those comparisons into changes that fit execlaw's architecture.

It is intentionally not a list of fashionable libraries to add. Execlaw already
has a stronger foundation than many agent frameworks: local-only inference,
SQLite-backed durable state, isolated runners, trust-scoped tool access,
approvals, an outbox, and paired tool events. Improvements should reinforce
those properties rather than replace them with a Python framework or a hosted
control plane.

Related internal references:

- [`architecture.md`](architecture.md)
- [`agent-model.md`](agent-model.md)
- [`operator-decision-rubric.md`](operator-decision-rubric.md)
- [`plugins.md`](plugins.md)
- [`runner-design.md`](runner-design.md)
- [`security.md`](security.md)
- [`testing.md`](testing.md)
- [`TrueNAS_deploy_doc.md`](TrueNAS_deploy_doc.md)

## 1. Product objective

Execlaw should aim to be the best **self-hosted, local-model-first, durable and
security-conscious personal agent platform**, not the framework with the largest
number of loosely connected agent abstractions.

The winning position is:

1. **More trustworthy than cloud-first agents.** Every action is attributable,
   policy-gated, replayable, and inspectable.
2. **More durable than chat-loop agents.** Work survives restarts, approvals,
   inference outages, and long waits without replaying effects.
3. **Better with local models.** Tool calling, context management, memory,
   structured output, and routing account for the strengths and weaknesses of
   quantized local models.
4. **Open without becoming porous.** MCP, A2A, ACP, plugins, skills, and external
   repositories are supported through explicit trust and supply-chain gates.
5. **Measured rather than anecdotal.** New memory, model, and harness techniques
   ship only when repeatable local evaluations demonstrate improvement.

## 2. Non-negotiable constraints

Every proposal in this document must preserve these rules:

- No cloud LLM path, fallback, embedding service, or hosted judge.
- SQLite remains the source of truth for configuration and durable state.
- Effects remain outbox-mediated and idempotent.
- Every `tool_use` has a terminal `tool_result` in the same commit.
- Plugins remain manifest-driven; production host code does not special-case
  plugin IDs.
- Trust-class filtering occurs before retrieval, ranking, or dispatch.
- The model may propose self-modification, memory promotion, and automation, but
  policy or controller approval applies it.
- External descriptions, schemas, Agent Cards, skills, MCP metadata, and graph
  results are untrusted input until verified.
- New engines and protocols are adapters around execlaw's policy model, not
  replacements for it.

## 3. Current strengths

Execlaw already ships capabilities that many surveyed projects only achieve
through additional infrastructure:

| Area | Current execlaw advantage |
|---|---|
| Durability | Append-only event log, replay, durable agents, mailboxes, research jobs, chain runs, cards, and outbox |
| Effects | Framework idempotency keys and outbox relay rather than direct model-side effects |
| Tool correctness | Bounded tool loop and same-commit `tool_use` / `tool_result` pairing |
| Isolation | Per-conversation runner containers with server-mediated tool dispatch |
| Policy | Trust ladder, capability gates, Rule of Two, and sideband human approval |
| Memory | Trust-scoped SQLite memory, HOT/WARM/COLD tiers, skill capture, and proposal review |
| Extensibility | Script and subprocess plugins, sidecars, transports, OAuth, MCP, skills, and UI panels |
| Models | Local OpenAI-compatible endpoints, native Ollama path, backend purposes, and model-family adapters |
| Knowledge | Graphify source graph, Graphiti bridge, research pipeline, and Obsidian lesson workflow |
| Operator UX | SPA, live progress, approvals, agents, routines, automations, settings, and desktop wrappers |

The roadmap should close gaps around these strengths rather than rebuild them.

## 4. Important verified gaps and closed foundations

These are implementation observations, not speculative product ideas.

### 4.1 Event integrity chain and checkpoint

Migration `0021_event_integrity_chain.sql` closes the earlier sequence-integrity
gap. Legacy v1 rows retain independent authentication, while v2 rows bind the
previous tag and key id and a signed per-conversation head authenticates the
terminal state. The retained checkpoint boundary and whole-conversation
deletion limitation are documented in §7.7.

### 4.2 Memory evidence lineage (implemented)

Migration 0020 adds append-only assertions and evidence tied to exact event
sequences, payload paths, quote hashes, extraction run IDs, trust classes,
validity windows, and supersession links. Approved evidence-backed assertions
project into existing memory reads, and trust filtering precedes ranking.
Migration 0024 pins host-derived scope and trust on extraction jobs. Successful
committed turns now enqueue exact ranges for a leased, restart-safe local
Small/Standard worker that validates evidence against replay before insertion.
The production policy leaves candidates pending by default and never projects
procedural candidates as ordinary facts.

### 4.3 Skill capture queue durability (implemented)

`crates/skills/src/capture.rs` now inserts deduplicated `memory_jobs` rows over
committed conversation event ranges. Workers claim or reclaim expiring leases,
persist retries with backoff and terminal status, and resume pending work after
a process restart. Each row records the capture-policy and model-selector hashes
used to define the extraction run.

### 4.4 Graphiti bridge hardening (implemented)

The Controller-only model schema exposes only typed status, ingest, search,
retract, and reconcile actions. The host derives scope, trust, source event,
and evidence identity; SQLite stores the endpoint and vault-secret reference,
while the vault stores the credential. Requests use the shared endpoint policy
and search results fail closed on scope/evidence mismatch. Ingest/reconcile are
deduplicated leased jobs with retry and restart reclaim. Graphiti remains an
optional projection rather than authoritative memory.

### 4.5 Backend behavior is inferred rather than declared

`BackendRow` stores purpose, engine name, endpoint, model specification,
reasoning flag, and lifecycle mode, but there is no normalized capability
contract for context length, modalities, tool calling, strict structured output,
reasoning, cached-token metrics, model residency, or speculation.

Routing should select a backend by required capabilities, not model-name
heuristics or binary hints alone.

### 4.6 Structured-output APIs have moved

The current request type includes `guided_decoding_backend`. Modern serving
engines increasingly expose portable JSON Schema response formats and
engine-specific `structured_outputs` or `format` fields. Execlaw needs a
capability-negotiated structured-output layer rather than one vLLM-era knob.

### 4.7 Durable step recovery foundation is present

Migration 0017 and `RunStore` implement durable model, compute, tool, approval,
outbox, child-run, and artifact step kinds with leases, cursor transitions, and
an explicit recovery decision. Atomic outbox enqueue plus step completion
prevents duplicate effects at that boundary. The remaining gap is adoption by
normal turn execution and the production process-kill/child-join matrix.

### 4.8 Artifact provenance enforcement is present

Migration 0022 and the provenance store enforce digest, publisher, repository,
workflow, offline cosign/SLSA, and SBOM policy for bundled plugin installation.
Subprocess bytes are rechecked at spawn; sidecar and runner launches require
digest-pinned OCI provenance. Controller-enabled local overrides are persisted
and audited. The remaining release gap is publishing the detached provenance
statement and offline cosign bundle consumed by bundled installation. Stdio MCP
commands remain operator-configured executables rather than packaged artifacts.

### 4.9 Local endpoint policy (implemented)

Migration 0018 stores approved CIDRs/DNS names and resolution diagnostics. The
shared policy rejects public and mixed DNS answers, redirects, userinfo, and
alternate numeric hosts, pins accepted DNS answers, and is wired to configured
inference, HTTP MCP, Graphiti, and STT/TTS clients. New embedding, reranking, or
judge transports must use the same adapter.

## 5. Competitive landscape

### 5.1 Agent harnesses and coding agents

| Project | Strong pattern | Execlaw response |
|---|---|---|
| [OpenHands](https://github.com/OpenHands/OpenHands) | Agent server separation, multiple backends, Docker workspaces, automation control plane, ACP support | Keep execlaw's stronger event/outbox model; add workspace-oriented coding-agent plugins and optional ACP adapter |
| [mini-SWE-agent](https://github.com/SWE-agent/mini-swe-agent) | Minimal inspectable loop, trajectory files, benchmark discipline, multiple sandbox environments | Keep the core loop small and export deterministic trajectories for evals; avoid framework inflation |
| [Pydantic AI](https://github.com/pydantic/pydantic-ai) | Typed tools/outputs, durable operations, composable capabilities, OpenTelemetry, span-based evals, context tools | Adopt fail-closed schemas, durable step boundaries, local traces, and behavior assertions; do not import Python runtime into core |
| [Letta Code](https://github.com/letta-ai/letta-code) | Editable memory, revision history, long-lived agents, background learning, subagents, skills | Adopt revision/evidence UX and background consolidation; retain approval gates rather than autonomous prompt rewriting |
| [Goose](https://github.com/aaif-goose/goose) | Local-model breadth, resumable/forkable sessions, MCP extension ecosystem | Add conversation/run forks and broader local engine adapters |
| [OpenCode](https://github.com/anomalyco/opencode) | Parent/child session navigation, permissions, compaction, repeated-call safeguards | Add first-class run trees and explicit repeated-identical-call detection |
| [LangGraph](https://github.com/langchain-ai/langgraph) | Graph checkpoints, interrupts, pending writes, time travel, node-level resume | Implement a native SQLite durable step machine; do not make LangGraph a runtime dependency |
| [smolagents](https://github.com/huggingface/smolagents) | Compact model/tool loop and restricted code execution | Preserve simplicity; consider WASI for restricted computation instead of general host code execution |

### 5.2 Memory and knowledge systems

| Project | Strong pattern | Execlaw response |
|---|---|---|
| [Graphiti](https://github.com/getzep/graphiti) | Temporal facts, validity windows, episodes, provenance, hybrid retrieval, contradiction history | Adopt temporal assertion/evidence semantics; keep Graphiti optional because it requires another graph stack |
| [Mem0](https://github.com/mem0ai/mem0) | Simple memory API, multi-signal retrieval, open benchmark harness | Adopt fused lexical/vector/entity ranking and benchmark discipline; do not copy cloud defaults or proprietary score claims |
| [Cognee](https://github.com/topoteretes/cognee) | Evidence records, composable ingestion, graph/vector/code separation, memory visualization | Add evidence/provenance UI and keep deterministic Graphify code graph separate from personal memory |
| [LangMem](https://github.com/langchain-ai/langmem) | Semantic, episodic, procedural memory and foreground/background extraction | Add typed memory; continue routing procedural learning into skills rather than facts |
| [Letta](https://github.com/letta-ai/letta-code) | Core versus external memory, progressive disclosure, memory revision UI | Expose HOT budget and revisions; index WARM memory without injecting all content |

### 5.3 Interoperability standards

| Standard | Relevance | Recommended posture |
|---|---|---|
| [MCP 2026-07-28](https://modelcontextprotocol.io/specification/latest) | Tools, resources, prompts, tasks, skills, apps, elicitation | High priority, dual-version implementation with security hardening |
| [A2A 1.0](https://a2a-protocol.org/latest/specification/) | Agent discovery, asynchronous tasks, streaming, artifacts, authorization states | Add at external delegation boundary after durable steps and SSRF protections |
| [ACP v1](https://agentclientprotocol.com/protocol/overview) | Editor/client sessions, permissions, file and terminal operations | Optional product adapter for IDE use; never replace runner protocol |
| [OpenTelemetry GenAI](https://github.com/open-telemetry/semantic-conventions-genai) | Agent/model/tool/MCP spans and metrics | Emit through an adapter; conventions remain development-stage and must not define canonical DB schema |
| [WASI 0.3](https://wasi.dev/) | Capability-oriented component execution | Prototype as a third plugin tier for restricted local computation |
| [OCI](https://specs.opencontainers.org/) | Signed images and artifacts | Use digests, signatures, referrers, and SBOMs for sidecars/plugins |
| [Sigstore](https://docs.sigstore.dev/) and [SLSA 1.2](https://slsa.dev/spec/v1.2/) | Artifact identity and build provenance | Require for bundled release artifacts; permit audited override for local development |

### 5.4 Local inference engines

| Engine | Best fit |
|---|---|
| [vLLM](https://github.com/vllm-project/vllm) | Primary high-throughput NVIDIA/AMD server; structured output, tool parsers, prefix cache, speculative decoding, metrics |
| [llama.cpp](https://github.com/ggml-org/llama.cpp) | Broadest local hardware support, GGUF, Apple/CPU/Vulkan, hybrid offload, grammars, lightweight server |
| [SGLang](https://github.com/sgl-project/sglang) | High-concurrency and long-prefix NVIDIA/AMD deployments using RadixAttention and advanced speculation |
| [OpenVINO GenAI](https://github.com/openvinotoolkit/openvino.genai) | Intel CPU/GPU/NPU, embeddings, reranking, Whisper, VLM, prefix cache, speculation |
| [Ollama](https://github.com/ollama/ollama) | Best operator onboarding and portable host-native model management |
| TensorRT-LLM | Lab/opt-in NVIDIA path until the selected release and model matrix are stable |

## 6. Priority framework

- **P0:** Correctness, security, or architectural leverage. Start immediately.
- **P1:** Major product differentiation after P0 foundations.
- **P2:** Valuable expansion with measurable demand.
- **Lab:** Track and benchmark; do not promise production support.

Each item must pass four gates:

1. Fits execlaw's architecture.
2. Has an explicit threat model.
3. Has repeatable local acceptance tests.
4. Improves a measured outcome without regressing trust, durability, or latency
   budgets.

### 6.1 Ranked portfolio

| Rank | Enhancement | Priority | Impact | Effort | Earliest horizon | Release gate |
|---:|---|---|---|---|---|---|
| 1 | Durable step machine | P0 | Very high | High | 60 days | Crash matrix shows zero duplicate effects and complete resume |
| 2 | Fail-closed schema contracts | P0 | Very high | Medium | 30 days | Invalid schemas/calls rejected before dispatch in all tool tiers |
| 3 | Typed tool failure and retry protocol | P0 | High | Medium | 30 days | Retry/denial/cancellation conformance suite passes |
| 4 | Local-only endpoint enforcement | P0 | Very high | Medium | 30 days | Public, redirect, IPv6, and rebinding adversarial tests pass |
| 5 | Memory assertions and evidence | P0 | Very high | High | 60 days | Every injected memory resolves to evidence; no trust leakage |
| 6 | Graphiti bridge hardening | P0 | High | Low | 14 days | No model-selected URL/key/raw request; scope tests pass |
| 7 | Integrity chain/checkpoints | P0 | High | High | 60 days | Mutation/deletion/reordering/truncation tests detect tampering |
| 8 | Artifact signatures and provenance | P0 | High | Medium | 90 days | Bundled artifacts verify source/workflow/signature/SBOM |
| 9 | Backend capability contracts | P1 | High | Medium | 60 days | Routing fixtures select only compatible local backends |
| 10 | Modern structured-output adapters | P1 | High | Medium | 60 days | >= 99% executable calls on qualified model matrix |
| 11 | Hybrid temporal memory retrieval | P1 | High | High | 90 days | Held-out recall improves without trust/latency regression |
| 12 | Tiered context engineering | P1 | High | Medium | 60 days | Fewer prompt tokens with no held-out task-success loss |
| 13 | First-class run trees/subagents | P1 | High | High | 90 days | Durable fan-out/join, budget, and cancellation tests pass |
| 14 | Local traces and regression evals | P1 | High | Medium | 60 days | Production trajectory becomes replayable local test |
| 15 | Memory Palace UI | P1 | Medium | Medium | 90 days | Operator can explain, revise, retract, and export every memory |
| 16 | Conversation/run forks | P1 | Medium | Medium | 90 days | Fork cannot inherit or redeliver pending effects |
| 17 | MCP 2026 dual-era support | P2 | High | High | 90+ days | Legacy/current conformance and OAuth/SSRF suites pass |
| 18 | Managed llama-server/OpenVINO | P2 | Medium | Medium | 90+ days | Hardware-specific benchmark beats or complements current path |
| 19 | A2A external delegation | P2 | Medium | High | 6–12 months | Authenticated task/artifact/cancel interoperability suite passes |
| 20 | ACP and WASI adapters | P2/Lab | Medium | High | 6–12 months | Editor or component trial meets isolation and usability gates |

Impact is relative to execlaw's goal, not market popularity. Effort includes
schema migration, security review, tests, UI, operations, and documentation.

## 7. P0: immediate enhancements

### 7.1 Durable step machine

**Problem:** atomic turns are safe, but long-running workflows need finer resume
points.

**Implemented foundation:** migration 0017 and `core::runs::RunStore` provide
the tables below, guarded transitions, leases, idempotent stable definitions,
approval waits, atomic outbox enqueue/completion, cursor advancement, and an
explicit next-safe-action decision. Reopen and expired-lease tests cover the
store. Normal chat/runner execution is not yet driven by `RunStore`, so the
end-to-end process-kill matrix remains open.

Add SQLite-backed:

```text
state_runs
  run_id, conversation_id, parent_run_id, status, cursor,
  input_event_seq, started_at, updated_at, deadline_at

state_run_steps
  run_id, step_id, ordinal, kind, status, attempt,
  input_hash, output_ref, approval_id, outbox_idempotency_key,
  lease_owner, lease_expires_at, started_at, completed_at
```

Step kinds should include:

- `model_request`
- `deterministic_compute`
- `tool_dispatch`
- `approval_wait`
- `outbox_enqueue`
- `child_run_spawn`
- `child_run_join`
- `artifact_publish`

Rules:

- The event log remains canonical conversation history.
- Step tables are durable execution state and can be reconstructed/audited.
- An effectful tool step completes when the effect is durably queued, not when
  the external service eventually responds.
- Same-commit tool pairing remains mandatory.
- Resume never repeats an outbox enqueue with the same idempotency key.

Acceptance:

- Kill the process at every step boundary and resume without duplicate effects.
- Approval waits survive restart indefinitely.
- Child joins resume after either parent or child restart.
- A run can explain its current cursor and next safe action.

Owning surfaces: `crates/core`, `crates/runner-local`, `crates/server`,
`crates/runner-protocol`.

### 7.2 Typed tool failure protocol

The in-process tool path now wraps outcomes in a stable result envelope:

```json
{
  "status": "error",
  "kind": "validation|policy_denied|approval_denied|transient|timeout|cancelled|permanent",
  "code": "stable_machine_code",
  "message": "sanitized explanation",
  "retryable": false,
  "retry_after_ms": null,
  "attempt": 1,
  "guidance": "how the model can correct the request"
}
```

Add:

- Per-tool retry policy and total run retry budget.
- Deterministic exponential backoff persisted in SQLite.
- Repeated-identical-call detection based on tool name plus canonical arguments.
- Circuit breakers for repeatedly failing integrations.
- A bounded correction attempt for schema-invalid model output.

`ToolResultEnvelope`/`ToolFailure` and the in-process executor implement the
failure kinds, retry normalization, three-attempt transient/timeout retry,
bounded schema correction, repeated-identical-call detection, and a 30-second
integration circuit breaker. Retry budgets, backoff, and circuit state are not
yet persisted, and `runner-protocol` still exposes its older string-error
envelope.

Acceptance:

- Invalid arguments never reach tool code.
- Policy denial is never automatically retried.
- Transient errors resume after restart at the scheduled time.
- Identical failing calls terminate before exhausting all tool rounds.

### 7.3 Fail-closed schema contracts

Compile every plugin, MCP, built-in, and structured-output schema at
registration time using JSON Schema Draft 2020-12.

Requirements:

- Reject invalid schemas at install/registration.
- Reject network `$ref` by default.
- Permit bundled references only when path-safe and content-addressed.
- Validate tool input before OAuth injection and dispatch.
- Validate structured tool results before exposing them to the model.
- Record schema hash with each tool invocation and trace.
- Add schema-version compatibility checks during plugin upgrade.

This is the most direct way to improve local-model reliability because weak
models fail most often at structured boundaries.

Plugin input/result schemas and path-safe bundled local references compile at
registration; built-in input and declared-result schemas compile at
registration; MCP input schemas compile at discovery. Dispatch validates before
side effects, successful plugin/built-in results are validated, canonical
bundle hashes reject in-place plugin contract changes, and focused tests cover
the failure paths. Invocation events/traces do not yet persist the schema hash,
and MCP has no advertised result-schema contract to validate.

### 7.4 Enforce local-only inference

Introduce a shared outbound endpoint policy for inference:

- Permit loopback, configured LAN/VPN CIDRs, Unix sockets, and explicitly
  approved local DNS names.
- Reject public IPs and public DNS resolutions by default.
- Re-resolve and verify after redirects.
- Detect IPv4/IPv6 private-range bypasses and DNS rebinding.
- Add an operator-visible endpoint classification and last-resolved address.
- Start managed Ollama with cloud features disabled.
- Apply the policy to embedding, reranking, STT, TTS, and judge endpoints too.

Acceptance: adversarial tests for redirects, mixed DNS answers, IPv6 forms,
userinfo, alternate numeric IP forms, and rebinding.

Migration 0018 and `crates/local-endpoint-policy/` implement this as a shared
policy loaded from SQLite-approved CIDRs/DNS names. DNS answers are validated
once and pinned, redirects are disabled rather than followed, and accepted or
denied resolutions are persisted. Configured inference, HTTP MCP, Graphiti, and
voice STT/TTS use the shared adapter. Future dedicated embedding, reranking, or
judge transports must use the same adapter when introduced.

### 7.5 Memory assertion and evidence model

Do not replace `memory_entries` immediately. Introduce an append-only assertion
layer and project approved current values into existing reads.

```text
memory_assertions
  assertion_id, scope, trust_class, kind,
  subject, predicate, object_json,
  confidence, status,
  observed_at, valid_from, valid_until,
  supersedes_id, extraction_run_id,
  created_event_seq

memory_evidence
  assertion_id, conversation_id, event_seq,
  payload_path, quote_hash, evidence_kind

memory_jobs
  job_id, conversation_id, from_seq, to_seq,
  extraction_policy_version, model_config_hash,
  status, attempts, lease_owner, lease_expires_at
```

Memory kinds:

- `profile`: stable operator/contact preferences and identity facts.
- `semantic`: general facts and relationships.
- `episodic`: events and experiences tied to time.
- `procedural`: candidate reusable workflow; project into skill proposals.
- `summary`: lossy digest with source range.
- `decision`: explicit choice plus rationale and alternatives.

Retrieval must filter trust in SQL before ranking.

Acceptance:

- Every injected memory resolves to at least one surviving evidence record.
- Corrections supersede rather than overwrite.
- Historical queries return the correct validity window.
- Forced termination loses no memory jobs.
- Cross-trust leakage remains zero in adversarial tests.

Migration 0020 and `core::memory_assertions` implement append-only assertions
and evidence, validity windows, supersession, evidence-gated projection into
existing memory reads, trust-before-ranking queries, and leased retryable jobs.
Skill capture persists `skill_capture` jobs before waking its worker. Migration
0024 and `server::memory_extract_runtime` add the production `memory_extract`
producer and worker with pinned host authority, policy/model hashes, bounded
local inference, replay-validated evidence, retry/backoff, and pending-by-default
approval.

### 7.6 Graphiti hardening

Make Graphiti an optional projection consumer, never the source of truth.

Immediate changes:

- Remove `base_url`, `api_key`, and `raw_request` from model-callable arguments.
- Store endpoint in SQLite and API key in the vault.
- Restrict actions to typed status, ingest, search, retract, and reconcile.
- Derive Graphiti group IDs from local scope/trust policy.
- Route ingestion through a durable job/outbox path.
- Attach source event/evidence IDs to episodes.
- Validate returned scope and evidence before prompt injection.
- Disable Graphiti telemetry in the supported local deployment.
- Add endpoint SSRF and redirect policy.

Implemented: the model sees only typed actions; scope, trust, source event, and
evidence identity are host-derived. Endpoint configuration is in SQLite, the
credential reference resolves through the vault, requests use the shared
local-endpoint policy, search results fail closed on scope/evidence mismatch,
and ingest/reconcile run as deduplicated leased jobs with retry and restart
reclaim. Graphiti remains optional and is not authoritative memory.

### 7.7 Integrity checkpoints

Implemented by migration `0021_event_integrity_chain.sql` as a
per-conversation HMAC chain plus a signed terminal head:

- Existing rows are `integrity_version = 1`; their tags retain the original
  independent-row canonical encoding.
- `EventLog::establish_v2_checkpoint` (or the all-conversation backfill helper)
  creates a v2 genesis HMAC over the exact legacy prefix without rewriting it.
  The next event chains from that anchor.
- V2 event tags bind the unchanged v1 canonical row, signing `key_id`, and
  `prev_tag`. `state_event_integrity_heads` signs the chain start, terminal
  sequence, terminal tag, genesis key id, and checkpoint key id.
- Event rows and the terminal head update in one SQLite transaction. Replay
  verifies the full conversation before returning a suffix and keyed hydration
  does not trust snapshots that omit integrity metadata.
- Rotation changes the current signing key id only. Prior event ranges and
  checkpoints continue to verify through retained key-ring entries; no
  destructive re-signing occurs.

Detection is guaranteed through the latest retained signed head. As with any
same-database checkpoint, deleting the entire conversation, all of its v2 rows,
and its head is outside this local proof; backup manifests or an external
anchor are still required to prove that a whole conversation once existed.

Acceptance:

- Mutation, deletion, insertion, reordering, and truncation are detected within
  the promised checkpoint boundary.
- Backup verification can run without model or plugin code.
- Key rotation keeps prior ranges verifiable.

### 7.8 Supply-chain verification

For plugin ZIPs, subprocess binaries, sidecars, installers, and runner images:

- Persist SHA-256 digest, publisher identity, source repository, source commit,
  workflow identity, signature, and attestation result.
- Require verified provenance for bundled production artifacts.
- Allow unsigned local development only with an explicit Controller override and
  audit event.
- Pin sidecars by OCI digest, not floating tag.
- Generate CycloneDX or SPDX SBOMs per plugin/image/release.
- Verify Sigstore signatures and SLSA provenance against an allowlisted source
  and workflow.
- Surface dependency/VEX status without treating an SBOM as a vulnerability
  verdict.

Migration 0022 and `core::artifact_provenance` implement SQLite allowlists,
SHA-256 checks, offline cosign SLSA verification, persisted provenance, and
audited local overrides. Bundled plugin installation verifies ZIP and SBOM;
subprocess spawn rechecks the derived executable digest; sidecar and runner
launch require digest-pinned OCI plus persisted provenance. Packaging emits
SPDX 2.3 plugin sidecars and release workflows create GitHub attestations. The
remaining P0 release task is to export/publish the detached provenance statement
and offline cosign bundle in the format bundled installation consumes.

## 8. P1: product-defining enhancements

### 8.1 Hybrid, temporal memory retrieval

Fuse independent signals after trust filtering:

- FTS5/BM25 lexical score.
- Local embedding similarity.
- Entity overlap.
- Temporal relevance.
- Evidence quality.
- Confidence.
- Recency and usage.
- User pin/approval weight.

Use reciprocal-rank fusion first because it is robust across incomparable score
scales. Embeddings are a rebuildable derivative index keyed by assertion ID and
embedding-model hash.

Do not require a graph database for normal recall. Use Graphiti only when graph
and temporal traversal demonstrate measurable value.

Target metrics:

- Recall@5 and Recall@10.
- MRR and nDCG.
- Evidence sufficiency.
- Temporal correction accuracy.
- Abstention accuracy.
- p50/p95 retrieval latency.
- Useful-token ratio in the final prompt.

### 8.2 Memory Palace UI

Build operator-facing memory inspection around evidence and revision, inspired
by Letta's memory UX without autonomous mutation.

Views:

- HOT budget and exact prompt contribution.
- Current assertions grouped by profile/semantic/episodic/procedural.
- Evidence turn and quoted source.
- Extraction model, prompt/config hash, timestamp, and confidence.
- Revision/supersession timeline.
- Contradictions and unresolved candidates.
- Promote, demote, retract, merge, split, pin, and set TTL.
- “Why was this recalled?” score decomposition.
- Export/delete across projections.

### 8.3 First-class run trees and subagents

Extend current durable child agents into navigable run trees:

- Parent/child run IDs.
- Separate context and narrowed capability tokens.
- Per-child model, token, time, tool-round, and concurrency budgets.
- Fan-out/join with durable partial completion.
- Cancellation propagation.
- Background completion notifications.
- Artifact handoff rather than full transcript copying.
- UI navigation across parent and child traces.
- Plugin- or SQLite-declared roles; no hardcoded role names.

A child must never inherit broader trust or tools than its parent.

### 8.4 Tiered context engineering

Apply context reduction in this order:

1. Remove superseded duplicate reads.
2. Store large results in content-addressed blobs and retain bounded previews.
3. Replace old tool payloads with typed receipts while preserving tool pairs.
4. Retrieve only relevant WARM memories and skills.
5. Preserve pinned task state, approvals, and unresolved errors.
6. Summarize the oldest safe range last.

Persist a compaction receipt containing:

- Source event range.
- Strategy version.
- Model/config hash if a model was used.
- Preserved anchors.
- Dropped blob references.
- Estimated tokens before/after.

Expose context use in the SPA by source: system, tools, history, memory, skills,
attachments, and reserved output.

### 8.5 Backend capability contracts

Add a normalized SQLite-backed descriptor:

```text
protocol: openai_chat | openai_responses | ollama_chat | llama_server | custom
context_tokens
modalities: [text, image, audio, video]
tool_calls: none | serial | parallel
tool_choice: boolean
structured_output: none | json_object | json_schema | grammar
reasoning: none | toggle | effort
usage_metrics: [prompt, cached_prompt, completion, queue, ttft, tpot]
residency: fixed | keep_alive | on_demand
speculation: [ngram, suffix, draft, mtp, eagle]
health_surface
metrics_surface
```

Resolve requests by purpose plus required capability. Probe and cache verified
capabilities, but require operator confirmation before trusting self-reported
remote metadata.

### 8.6 Modern structured output and tool calling

Create one internal contract and map it per engine:

- OpenAI-compatible `response_format`.
- vLLM `structured_outputs` with XGrammar/Guidance.
- Ollama native `format` JSON Schema.
- llama.cpp grammar/JSON Schema.
- SGLang XGrammar.
- OpenVINO XGrammar where supported.

Qualify each model/parser/template combination with a deterministic tool-call
matrix. Do not infer parser compatibility from model family alone.

Hard gate: no malformed tool arguments may reach dispatch.

### 8.7 Local inference observability and adaptive routing

Collect:

- Time to first token.
- Inter-token latency and TPOT.
- Queue time.
- Prefill and decode throughput.
- Cached and uncached prompt tokens.
- KV-cache use and evictions.
- Preemptions.
- Model load/swap time.
- Speculative accepted/rejected tokens.
- Tool-schema compilation time.
- Peak RAM/VRAM.

Use these metrics for policy-based routing:

- Small model for classification/extraction when schema quality is sufficient.
- Standard model for general turns.
- Reasoning mode only after complexity or retry thresholds.
- Vision model only when media requires it.
- Fallback means another approved local backend, never cloud.

### 8.8 Trace projection and local observability

Add stable IDs for run, step, parent run, model request, tool call, approval, and
outbox effect. Project event/step data into OpenTelemetry-compatible spans.

Rules:

- SQLite/event log remains canonical.
- OTLP export is optional and local-only by default.
- Prompt, tool arguments, results, and resource URIs are redacted/off by default.
- OTel GenAI naming is an adapter because those conventions are still evolving.
- Provide a local trace browser with critical-path and retry visualization.

### 8.9 Production failure to regression workflow

Turn redacted production trajectories into versioned local eval datasets.

Assertions should include:

- No forbidden tool was offered or called.
- Approval happened before outbox enqueue.
- Every tool call paired.
- Correct trust partition used.
- No duplicate effect after retry/resume.
- Expected artifact/evidence produced.
- Tool rounds, latency, and token budgets respected.
- Model abstained when evidence was insufficient.

Support model matrices across approved local endpoints and deterministic mock
tools. Keep extractor, embedder, answerer, and judge versions fixed when
comparing memory systems.

### 8.10 Conversation and run forks

Allow an operator to fork from an event sequence or checkpoint:

- Immutable ancestry pointer.
- New conversation/run IDs.
- New idempotency namespace.
- Explicit choice of inherited memories, attachments, and approvals.
- No inherited pending outbox effect.
- Side-by-side outcome comparison.

This supports debugging, alternative planning, and automatic eval-case creation.

## 9. P2: ecosystem and capability expansion

### 9.1 MCP 2026 dual-era support

Do not replace the current implementation with a constant bump. Build protocol
fixtures for both current legacy support and MCP 2026-07-28.

Add:

- Per-request version/capability metadata.
- Modern discovery and response typing.
- Tasks, progress, cancellation, subscriptions, and elicitation where useful.
- Resource and prompt surfaces, not tools only.
- Structured output validation.
- RFC 9728/OAuth discovery for HTTP servers.
- Strict HTTPS, issuer, audience, resource, redirect, and SSRF checks.
- Sandboxed stdio execution.

Execlaw trust and capability gates remain authoritative; MCP annotations do not
grant authority.

### 9.2 A2A external agent delegation

Implement A2A only after durable steps and endpoint security are in place.

Map:

- Execlaw agent definition -> signed Agent Card.
- Execlaw run/card -> A2A Task.
- Attachment/artifact -> A2A Artifact.
- Approval/auth wait -> `INPUT_REQUIRED` or `AUTH_REQUIRED` plus local policy
  record.
- Event bus -> SSE task updates.
- Outbox -> authenticated, idempotent push notifications.

Start with authenticated HTTP+JSON, polling, cancellation, and artifacts. Add
streaming next. Add push only after URL allowlists, DNS-rebinding protection,
unique callback credentials, and idempotent receipt storage.

Never pass user/provider credentials through an agent chain.

### 9.3 ACP editor adapter

Expose execlaw as an optional ACP agent for VS Code and compatible editors:

- Session create/load maps to conversations/forks.
- Permission requests map to approvals.
- Plans/tool updates map to cards and trace events.
- File and terminal capabilities map to a workspace plugin with restricted
  roots and capabilities.
- Absolute path requests remain inside an approved workspace mapping.

ACP is a client adapter, not the internal runner protocol.

### 9.4 WASI component plugin tier

Prototype a third runtime tier between Rhai and subprocess:

- Wasmtime component model/WASI 0.3.
- Explicit host capabilities for HTTP, vault aliases, artifacts, clocks, and
  randomness.
- No ambient filesystem or network.
- Fuel, epoch, memory, and wall-clock limits.
- Signed component artifacts and WIT interface versioning.

Use for parsers, transforms, deterministic utilities, and lightweight native
extensions. Keep container sidecars for heavyweight browsers, databases, and
GPU services.

### 9.5 Managed llama-server backend

Add llama.cpp's server as a managed backend for:

- Apple Silicon.
- CPU/Vulkan hosts.
- GGUF quantized models.
- Partial CPU/GPU offload.
- Low-memory installations.

Compare direct llama-server model routing with llama-swap before adding another
configuration layer. Any runtime config must be generated from SQLite.

### 9.6 OpenVINO GenAI and SGLang presets

- Promote OpenVINO GenAI to a first-class Intel CPU/GPU/NPU path with explicit
  LLM, embedding, reranking, Whisper, and VLM capabilities.
- Add SGLang as an advanced preset for high-concurrency NVIDIA/AMD servers when
  RadixAttention or speculation wins the execlaw benchmark.
- Keep TensorRT-LLM an opt-in lab backend until a stable release/model matrix
  passes soak tests.

### 9.7 Measured speculative decoding

Start with methods that require no draft model:

- n-gram or suffix speculation.
- prompt lookup.

Then test MTP/EAGLE only on qualified model/hardware pairs.

Ship only when:

- Accepted tokens per target step >= 1.5.
- p95 TPOT improves >= 20%.
- Throughput regression at target concurrency <= 5%.
- Greedy output remains equivalent where expected.
- Tool-call and schema accuracy do not regress.

### 9.8 Coding-agent workspace plugin

Execlaw can become a strong coding harness without turning host core into an IDE.
Build a first-party plugin or capability bundle that provides:

- Workspace-rooted file access.
- Semantic/Graphify navigation.
- Allowlisted shell and task execution.
- Git diff, status, history, and patch application.
- Test discovery and focused validation.
- Checkpointed plans and child review agents.
- Patch artifacts and approval before destructive commands.
- Optional ACP frontend.

Use per-project containers/volumes and narrowed capability tokens. Never mount
all operator files or the Docker socket into a coding runner.

## 10. Graphify strategy

Graphify and runtime memory solve different problems.

### 10.1 Keep Graphify deterministic and developer-facing

Graphify should remain the source/code topology graph. Improve it with:

- Incremental graph freshness status in the SPA.
- Source commit and parser-version metadata.
- Changed-file impact queries.
- Symbol-to-test and symbol-to-doc relationships.
- Dependency risk and ownership overlays.
- Query result evidence pointing to exact source locations.
- Graph diff between commits.
- A compact API for scoped subgraphs rather than shipping the full graph.

### 10.2 Connect, do not merge

A memory assertion may reference a Graphify symbol as evidence or context, but
personal/temporal memory must not be merged into the source graph.

Example:

```text
procedural assertion: "Plugin webhooks authenticate before dispatch"
evidence:
  event: controller correction in conversation X
  symbol: crates/server/src/plugin_webhook_routes.rs::dispatch
  source_commit: abc123
```

When the symbol changes, mark the procedural memory for revalidation rather than
silently deleting or trusting it.

### 10.3 Graph quality gates

- No stale source commit in a model-visible graph result.
- Every node has a stable source identifier.
- Every semantic label records the extraction model/config hash.
- AST edges remain deterministic and rebuildable without an LLM.
- Semantic labels never override source truth.
- Graph queries are bounded by node/edge/result budgets.

## 11. Self-improvement without loss of control

“Self-improving agent” should mean evidence-driven proposals, not autonomous
mutation of its own authority.

Allowed loop:

1. Observe a successful or failed trajectory.
2. Extract a candidate memory, skill, prompt patch, test, or automation.
3. Attach evidence and expected benefit.
4. Run local evaluation against current and candidate versions.
5. Scan for secrets, policy changes, and expanded capabilities.
6. Present a diff and metrics to the Controller.
7. Approve, reject, or edit.
8. Apply with versioning and rollback.
9. Monitor outcome and automatically propose rollback on regression.

Never auto-apply:

- Trust policy changes.
- New external endpoints.
- Wider tool access.
- Secret handling changes.
- Plugin/subprocess installation.
- Changes to system invariants.
- A prompt or skill that failed held-out regression tests.

## 12. Testing and evaluation program

### 12.1 Harness correctness

Add fault injection at every durable boundary:

- Before/after event append.
- Before/after tool dispatch.
- Before/after outbox enqueue.
- During approval wait.
- During child-agent fan-out/join.
- During memory extraction and projection.
- During model stream and cancellation.

Verify restart, idempotency, and exact event/step state.

### 12.2 Tool-calling matrix

For every supported model/engine/template/parser combination, run:

- Required/optional/nested schema fields.
- Parallel tool calls.
- Malformed and truncated arguments.
- Tool result error correction.
- No-tool and forced-tool choices.
- Long tool catalogs.
- Repeated identical calls.
- Reasoning enabled/disabled.
- Context near maximum length.

Track executable-call rate, schema validity, correction success, and average
rounds.

### 12.3 Memory benchmarks

Use open benchmark datasets such as LoCoMo, LongMemEval, and BEAM where licenses
and local execution permit. Add execlaw-specific sets for:

- Trust leakage.
- Temporal corrections.
- Contact identity ambiguity.
- Prompt-injection persistence poisoning.
- Skill extraction quality.
- Evidence sufficiency and abstention.
- Deletion/export completeness.

Do not compare vendor scores unless extractor, embedding, retrieval, answer, and
judge configurations match.

### 12.4 Inference benchmark

Use at least:

1. 8K-input/256-output chat.
2. Three-round tool loop with full catalog.
3. Repeated-prefix workload.
4. Concurrent conversations at 1/4/8 clients.
5. Vision request.
6. STT and TTS workloads where configured.

Report cold/warm TTFT, p50/p95 TPOT, throughput, cache ratio, RAM/VRAM, model
load time, schema validity, cancellation latency, WER, and first-audio latency.

### 12.5 Security tests

- Cross-trust memory retrieval.
- SSRF and DNS rebinding for every configurable URL.
- Malicious plugin ZIP/schema/UI asset.
- Unsigned or mismatched provenance.
- MCP/A2A metadata prompt injection.
- Symlink/path escape.
- Sidecar privilege and egress policy.
- Credential redaction in events, traces, logs, and artifacts.
- Event deletion/reordering integrity.

## 13. Operational hardening

### 13.1 Container defaults

For runners and sidecars where compatible:

- Non-root user.
- Read-only root filesystem.
- Drop all Linux capabilities.
- `no-new-privileges`.
- Seccomp/AppArmor profile.
- PID, memory, CPU, file-size, and process limits.
- Explicit writable mounts only.
- Isolated network by default.
- Manifest-declared egress domains/CIDRs.
- No host Docker socket in child containers.

### 13.2 Backup and deletion semantics

A complete export/delete operation must cover:

- Event rows and integrity checkpoints.
- Memory assertions/evidence/projections.
- Embedding indexes.
- Graphiti projections.
- Skills and proposals.
- Attachments/artifacts.
- Agent mailboxes/checkpoints.
- Outbox/inbox rows.
- Sidecar state where applicable.

Deletion should emit a tombstone/audit record without retaining deleted
sensitive content.

### 13.3 Offline update channel

Support signed offline bundles containing:

- Control-plane binary/image.
- Runner image.
- Plugin ZIPs.
- SBOMs and provenance.
- Migration manifest.
- Compatibility matrix.

This preserves local-only operation for disconnected installations.

## 14. AI technology radar

### Adopt now

- Durable SQLite step checkpoints.
- Typed tool errors and repeated-call detection.
- Fail-closed JSON Schema validation.
- Evidence-backed temporal memory.
- Local endpoint enforcement.
- Plugin/image signatures, provenance, and SBOMs.
- Local trace projection and trajectory evals.
- Capability-driven backend selection.

### Trial

- Hybrid FTS5 plus local embedding retrieval.
- MCP 2026 dual-version adapter.
- Managed llama-server.
- First-class OpenVINO GenAI preset.
- OTel GenAI export adapter.
- WASI component plugin tier.
- A2A client for explicitly configured remote/local peer agents.
- ACP adapter for editor clients.

### Assess

- SGLang for larger GPU deployments.
- EAGLE/MTP and advanced speculation.
- Graphiti as an optional temporal projection.
- MCP Apps and Skills over MCP.
- A2A push notifications.
- Multimodal unified models for audio understanding.
- Distributed KV cache and prefill/decode disaggregation.

### Hold or reject

- Cloud LLM or embedding fallback.
- Mandatory external graph/vector database for ordinary memory.
- Autonomous, unreviewed prompt/skill/policy rewriting.
- Hosted-only tracing or evaluation.
- Floating sidecar/model image tags in production.
- Treating namespaces, group IDs, MCP annotations, or Agent Cards as authority.
- Importing a general Python agent framework into Rust host core.
- Benchmark-driven adoption without reproducing results on operator hardware.

## 15. Delivery roadmap

### First two weeks

1. Correct integrity documentation or specify real chain/checkpoint migration.
2. Harden Graphiti arguments, configuration, trust floor, and endpoint policy.
3. Design durable `state_runs` / `state_run_steps` and failure envelope.
4. Add fail-closed schema validation spike for one built-in and one plugin tool.
5. Add durable memory/skill extraction job design.
6. Establish baseline tool-call and trust-leakage eval datasets.
7. Add backend capability descriptor schema and probe contract.

Deliverable: approved design records, migrations, adversarial tests, and baseline
numbers before broad feature implementation.

### 30 days

- Typed tool errors and repeated-call detection shipped.
- Schema validation at plugin install and dispatch shipped.
- Local inference endpoint enforcement shipped.
- Graphiti model-controlled URL/key/raw request removed.
- Durable extraction queue shipped.
- Trace IDs and local run/step browser MVP shipped.
- Plugin artifact digest and provenance fields persisted.

### 60 days

- Durable step resume for model/tool/approval/outbox boundaries.
- Temporal memory assertions and evidence records.
- Memory Palace MVP.
- Hybrid FTS5 plus one approved local embedding backend.
- Context source-budget UI and compaction receipts.
- Backend capability-driven routing.

### 90 days

- First-class run trees and fan-out/join.
- Production-trajectory regression datasets.
- Conversation/run forks.
- MCP dual-version conformance suite.
- Managed llama-server and full Ollama-native multimodal/structured parity.
- Signed bundled plugins and SBOMs in release workflow.

### Six to twelve months

- A2A external delegation.
- ACP editor integration.
- WASI plugin tier if the trial meets security and performance gates.
- OpenVINO GenAI production preset and SGLang advanced preset.
- Graphiti projection reconciliation and temporal graph UI.
- Signed offline update bundles.
- Measured speculation presets.
- Complete production voice path with real agent replies, endpointing, AEC
  strategy, and latency acceptance.

## 16. Success metrics

### Trust and correctness

- Zero unauthorized memory/tool rows in 10,000+ adversarial cases.
- 100% paired tool events.
- Zero duplicate external effects during injected crash/retry tests.
- 100% model-injected memory with resolvable evidence.
- 100% bundled production plugins/images with verified provenance.

### Agent quality

- >= 99% executable automatic tool calls on the supported model matrix.
- Measurable reduction in repeated-call loops and max-round cancellations.
- Improved task success on held-out local trajectory evals.
- Memory Recall@5/10 and temporal correction targets established per dataset.
- Abstention improves without unacceptable recall loss.

### Performance

- p95 tool-free turn overhead outside inference remains bounded.
- Warm repeated-prefix tests show >= 80% cached prompt tokens where supported.
- Warm p95 TTFT improves >= 40% on qualified prefix-cache workloads.
- Context compaction reduces prompt tokens without reducing held-out task success.
- Memory retrieval p95 remains within the interactive budget.

### Operations

- Every failed run identifies its current step and safe resume action.
- Recovery from control-plane restart loses zero durable jobs.
- Backup verification detects corruption and missing integrity ranges.
- Operator can explain why a tool, memory, model, or approval was selected.

## 17. Biweekly AI review process

AI changes too quickly for an annual roadmap. Run this process every two weeks.

### Inputs

Monitor official releases and specifications for:

- vLLM, Ollama, llama.cpp, SGLang, OpenVINO GenAI, TensorRT-LLM.
- MCP, A2A, ACP, OTel GenAI, WASI, OCI, Sigstore, SLSA.
- OpenHands, mini-SWE-agent, Pydantic AI/Harness, Letta, Goose, OpenCode.
- Graphiti, Mem0, Cognee, LangMem.
- Open model releases relevant to tool calling, reasoning, vision, audio,
  embedding, and reranking.

### Triage template

For each announcement record:

```text
Upstream + version/commit:
Official source:
Capability claimed:
Stable / experimental / benchmark-only:
Local-only compatible:
Required hardware:
Security implications:
Execlaw owning boundary:
Existing equivalent:
Expected measurable benefit:
Smallest experiment:
Acceptance threshold:
Rollback path:
Decision: adopt / trial / assess / reject
Review date:
```

### Rules

- Use official repository, release notes, spec, or paper as primary evidence.
- Treat vendor benchmark numbers as hypotheses.
- Pin exact versions in experiments.
- Reproduce on supported operator hardware and local models.
- Never merge an experiment directly into a production preset.
- Require a threat-model delta and rollback path.
- Update this radar and compatibility matrix when a trial changes state.
- Retire stale integrations rather than accumulating permanent compatibility
  burden.

### Experiment budget

Reserve a bounded engineering budget each cycle:

- One low-risk compatibility experiment.
- One quality/performance benchmark.
- One security or reliability hardening item.

If the experiment has no measurable acceptance criterion, it is not ready to
enter the backlog.

## 18. Recommended first implementation program

The highest-leverage program is **Durable, Evidence-Grounded Local Agents**:

1. Durable run/step checkpoints.
2. Typed schema-validated tool boundaries.
3. Temporal memory assertions with event evidence.
4. Local trace/eval projection.
5. Capability-driven local inference.
6. Signed extension supply chain.

This program closes the largest gaps with LangGraph, Pydantic AI Harness,
Letta, Graphiti, OpenHands, and modern inference servers while preserving what
makes execlaw distinctive.

Do not start with A2A, another orchestration framework, or a larger graph
service. Those become much safer and more useful after durable steps, evidence,
schema enforcement, endpoint policy, and evals are in place.

## 19. Sources

### Execlaw

- [`architecture.md`](architecture.md)
- [`agent-model.md`](agent-model.md)
- [`plugins.md`](plugins.md)
- [`security.md`](security.md)
- [`testing.md`](testing.md)

### Agent harnesses

- [OpenHands Agent Canvas](https://github.com/OpenHands/OpenHands)
- [OpenHands Software Agent SDK](https://github.com/OpenHands/software-agent-sdk)
- [mini-SWE-agent](https://github.com/SWE-agent/mini-swe-agent)
- [Pydantic AI](https://github.com/pydantic/pydantic-ai)
- [Pydantic AI durable execution](https://pydantic.dev/docs/ai/capabilities/durable_execution/overview/)
- [Letta Code](https://github.com/letta-ai/letta-code)
- [Goose](https://github.com/aaif-goose/goose)
- [OpenCode](https://github.com/anomalyco/opencode)
- [LangGraph](https://github.com/langchain-ai/langgraph)
- [smolagents](https://github.com/huggingface/smolagents)

### Memory and knowledge

- [Graphiti](https://github.com/getzep/graphiti)
- [Mem0](https://github.com/mem0ai/mem0)
- [Mem0 memory benchmarks](https://github.com/mem0ai/memory-benchmarks)
- [Cognee](https://github.com/topoteretes/cognee)
- [LangMem](https://github.com/langchain-ai/langmem)
- [Letta memory documentation](https://docs.letta.com/configuration/memory)

### Protocols and supply chain

- [MCP specification](https://modelcontextprotocol.io/specification/latest)
- [A2A 1.0 specification](https://a2a-protocol.org/latest/specification/)
- [Agent Client Protocol](https://agentclientprotocol.com/protocol/overview)
- [OpenTelemetry GenAI conventions](https://github.com/open-telemetry/semantic-conventions-genai)
- [WASI](https://wasi.dev/)
- [WebAssembly Component Model](https://component-model.bytecodealliance.org/)
- [OCI specifications](https://specs.opencontainers.org/)
- [Sigstore](https://docs.sigstore.dev/)
- [SLSA 1.2](https://slsa.dev/spec/v1.2/)
- [CycloneDX](https://cyclonedx.org/specification/overview/)
- [SPDX](https://spdx.github.io/spdx-spec/)

### Local inference

- [vLLM](https://github.com/vllm-project/vllm)
- [Ollama](https://github.com/ollama/ollama)
- [llama.cpp](https://github.com/ggml-org/llama.cpp)
- [SGLang](https://github.com/sgl-project/sglang)
- [OpenVINO GenAI](https://github.com/openvinotoolkit/openvino.genai)
- [TensorRT-LLM](https://github.com/NVIDIA/TensorRT-LLM)

## 20. Research limitations

- This is a point-in-time review. Fast-moving projects can change protocol,
  licensing, architecture, or maintenance status quickly.
- GitHub popularity is not evidence of correctness or fit.
- Upstream benchmark claims were not reproduced during this documentation task.
- Some capabilities exist only in managed editions even when an OSS repository
  demonstrates the concept.
- Official sources confirm advertised behavior, not its suitability for
  execlaw's threat model.
- Every adoption decision still requires a pinned proof of concept, local
  benchmark, security review, and rollback plan.

## 21. Implementation TODO

Status updated: **2026-09-12**. A checked item means the code and focused tests
exist in this repository; it does not imply that the broader ranked enhancement
is complete unless the text says so.

### Completed in this implementation pass

- [x] Compile declared plugin tool schemas as JSON Schema Draft 2020-12 before
  registration.
- [x] Reject missing, malformed, non-object, stage-escaping, or externally
  referencing declared schemas without partially registering the plugin.
- [x] Validate plugin tool arguments against the compiled schema before OAuth
  token lookup or runtime dispatch.
- [x] Add focused tests for valid schema loading, missing-schema rejection,
  external-reference and path-traversal rejection, atomic failure, and
  pre-dispatch argument rejection.
- [x] Remove model-controlled Graphiti URL, API key, HTTP method, path, body,
  group ID, source, and `raw_request` action.
- [x] Restrict the Graphiti tool to Controller trust, mark it sensitive, derive
  model-call scope from the conversation, reject unknown arguments, disable
  redirects, bound result counts, reject unscoped admin search/ingestion, and
  restrict the compatibility endpoint to a root HTTP loopback URL.
- [x] Correct security documentation for versioned integrity: legacy v1 rows
  authenticate independently; v2 rows chain to a signed terminal checkpoint.
- [x] Move Graphiti endpoint configuration into SQLite and credentials into
  the vault; add scope-bound evidence, durable leased ingest/reconcile jobs,
  retry/restart recovery, and fail-closed search-result validation.

### P0 implementation status

- [x] Compile and validate plugin input/result schemas, bundled path-safe local
  `$ref` documents, built-in input/declared-result schemas, and MCP input
  schemas; hash registered contracts and reject plugin upgrades whose hashes
  change in place.
- [x] Persist input/result schema hashes on durable tool-invocation traces;
  validate MCP's standard result shape and any advertised `outputSchema` before
  exposing structured content to the model.
- [x] Add the SQLite-backed `RunStore` with stable step definitions, leases,
  approval waits, atomic outbox enqueue/completion, cursor advancement, and a
  `next_safe_action` recovery decision. Reopen and lease-expiry tests exist.
- [x] Drive normal runner/server turns through `RunStore`, replay completed
  model/tool checkpoints after reopen, and preserve atomic event pairing and
  outbox idempotency. Durable child fan-out/join remains part of the P1 run-tree
  enhancement below because normal turns do not create child runs today.
- [x] Add the typed in-process `ToolResultEnvelope`/`ToolFailure` contract,
  bounded schema correction, transient retry/backoff, repeated-call detection,
  and per-integration circuit breaking.
- [x] Persist retry budgets, backoff, repeated-call fingerprints, schema hashes,
  and circuit state; carry the typed failure envelope through
  `runner-protocol` with legacy `{status,message}` deserialization.
- [x] Add the shared local-endpoint policy with SQLite-approved CIDRs/DNS names,
  public/mixed-answer rejection, DNS pinning, redirect denial, IPv4/IPv6 and
  alternate-numeric-host tests, and persisted resolution diagnostics. It is
  wired to configured inference, HTTP MCP, Graphiti, and STT/TTS clients.
- [x] Add append-only memory assertions/evidence, validity and supersession,
  evidence-gated projection into normal memory reads, durable leased jobs, and
  trust-first retrieval tests.
- [x] Wire the production `memory_extract` producer and leased worker at server
  bootstrap, with exact committed ranges, pinned host authority and policy/model
  hashes, bounded local inference, replay-validated evidence, retry/backoff,
  conservative approval, and procedural candidates retained as proposals.
- [x] Replace skill capture's volatile work queue with deduplicated leased
  SQLite `memory_jobs`; wake notifications are only an optimization.
- [x] Implement event-integrity v2 chaining/checkpoints with frozen legacy-v1
  verification, truncation detection, and non-destructive key rotation.
- [x] Enforce persisted provenance policy for bundled plugin ZIPs, subprocess
  executables, digest-pinned sidecar/runner OCI images, and audited Controller
  local-development overrides; generate SPDX 2.3 plugin sidecars.
- [x] Generate detached runtime provenance statements and offline cosign SLSA
  bundles before desktop packaging; embed and publish them beside each exact
  ZIP/checksum/SPDX artifact on Linux, macOS, and Windows.

### Outstanding P1/P2 and lab work

- [ ] Add backend capability contracts and modern structured-output adapters.
- [ ] Add hybrid temporal memory retrieval and the Memory Palace UI.
- [ ] Add tiered context receipts, source budgets, and held-out quality gates.
- [ ] Add durable run trees/subagents, cancellation, fan-out/join, and artifact
  handoff.
- [ ] Add local trace projection, trajectory regression datasets, and run
  forks with isolated effect namespaces.
- [ ] Implement MCP dual-era conformance and hardened OAuth/SSRF handling.
- [ ] Trial managed llama-server, OpenVINO GenAI, SGLang, and measured
  speculative decoding against the documented benchmark gates.
- [ ] Implement A2A, ACP, and WASI adapters only after their P0 dependencies
  and isolation gates pass.
- [ ] Build the workspace-rooted coding-agent plugin with narrowed capability
  tokens and destructive-operation approval.
- [ ] Complete container hardening, backup/deletion coverage, signed offline
  updates, Graphify freshness/impact features, and production voice gates.
