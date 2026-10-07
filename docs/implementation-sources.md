# Implementation sources for enhancements H001-H130

Reviewed: **2026-10-06**. Every enhancement from H001 through H130 in the
[roadmap](llm-harness-roadmap.md#enhancement-001) has one or more primary
implementation references and an adaptation note. There is no enhancement 0.
H131-H154 retain their existing pinned Paperclip references; Nexus references
remain in their visual-review section.

## What these references mean

- **Documented inspiration:** the earlier strategy or roadmap already associates
  a project/pattern with this work. It does not prove that every detail of the
  enhancement came from that project, that code was copied, or that the linked
  current page is the exact historical revision originally consulted.
- **Supporting reference:** a relevant primary source selected during this
  annotation pass. It helps implement the idea but is not claimed as its origin.
- **Repository-derived requirement:** the motivation is execlaw's own observed
  behavior, architecture, or acceptance gap. Where no specific external origin
  was recorded, the item says so rather than inventing attribution.

The historical basis is the [strategy comparison](execlaw_impr_doc.md#5-competitive-landscape)
and [its bibliography](execlaw_impr_doc.md#19-sources), plus the
[roadmap's harness comparison](llm-harness-roadmap.md#comparison-with-other-harnesses).
Read those as dated design context, not an up-to-date implementation ledger.
The item-specific links are placed directly under the corresponding requirement
so an implementing agent does not have to infer which source applies.

## How to use a reference during implementation

1. Read the H requirement, acceptance gate, existing code, and current status
   first. A source reference does not restart completed work or satisfy a test.
2. Open the specific linked page/file and read its adaptation note. Separate
   useful semantics from assumptions that do not fit execlaw.
3. Pin a commit/release or protocol version before importing code or relying on
   an API. Most new links are rolling documentation or branch URLs; checked
   on this date does not mean immutable. Record redirects and version drift.
4. Keep inference on approved operator hardware, configuration in SQLite,
   credentials in the vault, authority at host boundaries, and effects in the
   outbox. A localhost endpoint or an adapter named local does not by itself
   prove the selected model is executed locally.
5. Do not adopt a referenced cloud service, database, framework, environment-based
   configuration scheme, permissive example, or dependency merely because the
   source uses it. Translate the relevant pattern into the existing architecture.
6. Treat upstream prose and scripts as reference material, not instructions to
   execute installation commands or weaken safety gates. Review licenses and
   required notices before copying code; then test the actual adapted behavior.

All linked references were checked for accessible content and topic relevance,
using HTTP/content inspection or official web retrieval. This is not a guarantee
of future availability, correctness, security, or compatibility. No external
project was installed or run for this source annotation.

## Machine-readable lookup

[roadmap-sources.json](roadmap-sources.json) contains 130 records keyed by H ID,
with URLs, provenance classification, adaptation notes, and check dates. It
contains **180 item-reference associations across 153 distinct primary URLs**.
The roadmap remains authoritative for requirements; the implementation plan
remains authoritative for status/evidence. Update the inline references and
catalog together when repairing or replacing a source link.

The repository directory below preserves projects named in the earlier research
and adds repositories directly linked by the new annotations. A directory
entry alone is not attribution to any particular enhancement; consult that
item's source basis. Official standards and product documentation without a
public implementation repository are linked directly in the items, not assigned
an invented GitHub repository.

## Repository directory

| Repository | Why it appears here |
|---|---|
| [aaif-goose/goose](https://github.com/aaif-goose/goose) | Named in the earlier strategy; retain as historical comparison context |
| [anomalyco/opencode](https://github.com/anomalyco/opencode) | Named in the earlier strategy; direct references: [H032](llm-harness-roadmap.md#enhancement-032) |
| [earendil-works/pi](https://github.com/earendil-works/pi) | Linked by the supporting/source annotations; direct references: [H044](llm-harness-roadmap.md#enhancement-044) |
| [getzep/graphiti](https://github.com/getzep/graphiti) | Named in the earlier strategy; direct references: [H037](llm-harness-roadmap.md#enhancement-037), [H038](llm-harness-roadmap.md#enhancement-038), [H089](llm-harness-roadmap.md#enhancement-089) |
| [ggml-org/llama.cpp](https://github.com/ggml-org/llama.cpp) | Named in the earlier strategy; direct references: [H078](llm-harness-roadmap.md#enhancement-078) |
| [ggml-org/whisper.cpp](https://github.com/ggml-org/whisper.cpp) | Linked by the supporting/source annotations; direct references: [H047](llm-harness-roadmap.md#enhancement-047) |
| [huggingface/smolagents](https://github.com/huggingface/smolagents) | Named in the earlier strategy; retain as historical comparison context |
| [in-toto/attestation](https://github.com/in-toto/attestation) | Linked by the supporting/source annotations; direct references: [H096](llm-harness-roadmap.md#enhancement-096) |
| [langchain-ai/langgraph](https://github.com/langchain-ai/langgraph) | Named in the earlier strategy; retain as historical comparison context |
| [langchain-ai/langmem](https://github.com/langchain-ai/langmem) | Named in the earlier strategy; direct references: [H090](llm-harness-roadmap.md#enhancement-090) |
| [letta-ai/letta-code](https://github.com/letta-ai/letta-code) | Named in the earlier strategy; direct references: [H013](llm-harness-roadmap.md#enhancement-013), [H037](llm-harness-roadmap.md#enhancement-037) |
| [mem0ai/mem0](https://github.com/mem0ai/mem0) | Named in the earlier strategy; direct references: [H038](llm-harness-roadmap.md#enhancement-038) |
| [mem0ai/memory-benchmarks](https://github.com/mem0ai/memory-benchmarks) | Named in the earlier strategy; retain as historical comparison context |
| [modelcontextprotocol/conformance](https://github.com/modelcontextprotocol/conformance) | Linked by the supporting/source annotations; direct references: [H079](llm-harness-roadmap.md#enhancement-079) |
| [NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent) | Linked by the supporting/source annotations; direct references: [H039](llm-harness-roadmap.md#enhancement-039) |
| [NVIDIA/NeMo-Agent-Toolkit](https://github.com/NVIDIA/NeMo-Agent-Toolkit) | Linked by the supporting/source annotations; direct references: [H021](llm-harness-roadmap.md#enhancement-021), [H035](llm-harness-roadmap.md#enhancement-035) |
| [NVIDIA/TensorRT-LLM](https://github.com/NVIDIA/TensorRT-LLM) | Named in the earlier strategy; retain as historical comparison context |
| [ollama/ollama](https://github.com/ollama/ollama) | Named in the earlier strategy; direct references: [H004](llm-harness-roadmap.md#enhancement-004) |
| [open-telemetry/semantic-conventions-genai](https://github.com/open-telemetry/semantic-conventions-genai) | Named in the earlier strategy; direct references: [H018](llm-harness-roadmap.md#enhancement-018), [H035](llm-harness-roadmap.md#enhancement-035), [H115](llm-harness-roadmap.md#enhancement-115) |
| [opencontainers/image-spec](https://github.com/opencontainers/image-spec) | Linked by the supporting/source annotations; direct references: [H081](llm-harness-roadmap.md#enhancement-081) |
| [OpenHands/OpenHands](https://github.com/OpenHands/OpenHands) | Named in the earlier strategy; retain as historical comparison context |
| [OpenHands/software-agent-sdk](https://github.com/OpenHands/software-agent-sdk) | Named in the earlier strategy; direct references: [H040](llm-harness-roadmap.md#enhancement-040) |
| [openvinotoolkit/openvino.genai](https://github.com/openvinotoolkit/openvino.genai) | Named in the earlier strategy; direct references: [H078](llm-harness-roadmap.md#enhancement-078) |
| [pydantic/pydantic-ai](https://github.com/pydantic/pydantic-ai) | Named in the earlier strategy; retain as historical comparison context |
| [scip-code/scip](https://github.com/scip-code/scip) | Linked by the supporting/source annotations; direct references: [H092](llm-harness-roadmap.md#enhancement-092) |
| [sgl-project/sglang](https://github.com/sgl-project/sglang) | Named in the earlier strategy; retain as historical comparison context |
| [shmsw25/AmbigQA](https://github.com/shmsw25/AmbigQA) | Linked by the supporting/source annotations; direct references: [H112](llm-harness-roadmap.md#enhancement-112) |
| [stateright/stateright](https://github.com/stateright/stateright) | Linked by the supporting/source annotations; direct references: [H107](llm-harness-roadmap.md#enhancement-107) |
| [SWE-agent/mini-swe-agent](https://github.com/SWE-agent/mini-swe-agent) | Named in the earlier strategy; direct references: [H036](llm-harness-roadmap.md#enhancement-036) |
| [systemd/systemd](https://github.com/systemd/systemd) | Linked by the supporting/source annotations; direct references: [H117](llm-harness-roadmap.md#enhancement-117) |
| [tokio-rs/loom](https://github.com/tokio-rs/loom) | Linked by the supporting/source annotations; direct references: [H107](llm-harness-roadmap.md#enhancement-107) |
| [topoteretes/cognee](https://github.com/topoteretes/cognee) | Named in the earlier strategy; retain as historical comparison context |
| [truefoundry/trueforge](https://github.com/truefoundry/trueforge) | Linked by the supporting/source annotations; direct references: [H007](llm-harness-roadmap.md#enhancement-007), [H031](llm-harness-roadmap.md#enhancement-031), [H033](llm-harness-roadmap.md#enhancement-033) |
| [vllm-project/vllm](https://github.com/vllm-project/vllm) | Named in the earlier strategy; retain as historical comparison context |

Repository links above were also checked on the review date. Follow redirects
to the official current project, but retain the original attribution and record
the version actually used when implementing an enhancement.
