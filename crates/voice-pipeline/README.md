# execlaw-voice-pipeline

Two-lane Tokio graph for the streaming STT → LLM → TTS voice pipeline
(§2.13.2). Phase 0 ships the frame-vocabulary sketch; full implementation
is Phase 4.

## Implementation plan

All 130 enhancements are committed implementation scope under the
[implementation plan](../../docs/implementation-plan.md). This commitment is not a
claim that they have shipped: the plan owns implementation status, and the
[roadmap](../../docs/llm-harness-roadmap.md) owns acceptance criteria.

Relevant work for this component:

- [H047: cancellable voice](../../docs/llm-harness-roadmap.md#enhancement-047).
- [H074: bounded audio and process resources](../../docs/llm-harness-roadmap.md#enhancement-074).
- [H106: multimodal grounding](../../docs/llm-harness-roadmap.md#enhancement-106).
- [H129: multilingual qualification](../../docs/llm-harness-roadmap.md#enhancement-129).
