# execlaw-inference-api

The single internal contract for LLM access: an OpenAI-compatible `/v1/chat/completions`
client. No cloud-vendor SDKs in any form. Endpoints always resolve to local
inference servers (vLLM, OpenArc, llama.cpp server, Ollama) or operator-opted-in
inference-bridge plugins running locally.

Default model: `QuantTrio/Qwen3.5-27B-AWQ` (2026-04-23 locked decision).

Streaming SSE decoding is incremental and bounded, accepts CR/LF/CRLF, and
buffers UTF-8/tool JSON across arbitrary network chunk boundaries. A terminal
`[DONE]` plus a finish reason is required; premature EOF surfaces typed
`IncompleteStream`. Retry wrappers stop once a stream has been returned, so
visible partial output is never replayed. Ollama NDJSON uses its own required
terminal frame.

## Implementation plan

All 154 enhancements are committed implementation scope under the
[implementation plan](../../docs/implementation-plan.md). This commitment is not a
claim that they have shipped: the plan owns implementation status, and the
[roadmap](../../docs/llm-harness-roadmap.md) owns acceptance criteria.

Relevant work for this component:

- [H005: bounded retries](../../docs/llm-harness-roadmap.md#enhancement-005).
- [H029: streaming protocol correctness](../../docs/llm-harness-roadmap.md#enhancement-029).
- [H034: qualified backend capabilities](../../docs/llm-harness-roadmap.md#enhancement-034).
- [H078: hardware tiers](../../docs/llm-harness-roadmap.md#enhancement-078).
- [H110: local-only network verification](../../docs/llm-harness-roadmap.md#enhancement-110).
