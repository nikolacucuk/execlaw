# Voice mode — known follow-ups

The initial voice transport/STT/TTS path exists, with the integration gaps
below. Its presence does not qualify end-to-end agent voice behavior.

## Implementation delivery plan

All H001-H154 are accepted scope in [`implementation-plan.md`](implementation-plan.md).
These former deferrals now feed [H047 local voice delivery](llm-harness-roadmap.md#enhancement-047),
with [H030 inference scheduling](llm-harness-roadmap.md#enhancement-030),
[H043 stop/steering](llm-harness-roadmap.md#enhancement-043),
[H074 resource budgets](llm-harness-roadmap.md#enhancement-074),
[H106 multimodal evidence](llm-harness-roadmap.md#enhancement-106), and
[H110 local-only verification](llm-harness-roadmap.md#enhancement-110).
Voice input now uses the authenticated chat route and sends sentence chunks
from its committed response to local TTS. Playback waits for the chat response
so streamed model text cannot announce an external effect before the turn is
durable. Welcome capture mints the saved conversation before recording;
incognito capture remains an editable draft.
Continuous endpointing, incremental transcription, playback acknowledgment,
and supported-hardware qualification remain open. PCM capture and
per-session cancellation alone do not close those gates.

## 13.E — Server-side WebRTC AEC3 (deferred)

**Status**: deferred to a later phase (13.E-later).

**Why deferred**: WebRTC AEC3 is C++ in upstream WebRTC. Rust bindings are
sparse — `webrtc-audio-processing` lags upstream and Windows builds are
flaky. Honest paths forward are (1) a multi-day FFI yak shave around the
upstream `audio_processing` C++ tree, or (2) a sidecar `service-aec3`
microservice. Neither is justified before voice mode is in active use.

**Current behavior**: browser AEC stays OFF (operator's locked decision in
[`feedback_voice_mode_model`]). The practical implication today is that
**headphones are required** during voice conversations, otherwise the
agent's TTS audio will pick up in the mic and Whisper will transcribe its
own speech. This is acceptable for dogfood; the AEC3 follow-up unlocks
hands-free desktop + speakerphone setups.

**When to revisit**: when phone-bridge (Bluetooth → execlaw) sources land,
because phone audio comes back in the mic by definition and we can't
require headphones on a phone bridge.

## SPA mic capture: PCM16 (completed)

**Status**: `VoiceCaptureButton` captures browser audio samples and converts
them to PCM16 before sending them to the server. Server-side
`voice_runtime::ingest_chunks` accepts the resulting `pcm16le` / `pcm16`
frames.

The former MediaRecorder/Opus compatibility gap is closed. Future codec work
would be an optimization for bandwidth, not a prerequisite for Whisper input.

## Continuous VAD-driven endpointing (deferred)

**Status**: v1 voice mode is push-to-talk. The SPA's mic toggle generates
the `voice_stop` control message; the server's voice_runtime flushes
Whisper on receipt.

**Follow-up**: server-side WebRTC-VAD (or Silero ONNX in the
`voice-pipeline` crate, which already has the `Vad` trait) auto-endpoints
on silence so the operator can speak hands-free. Wire format change is
zero — same `voice_stop` UiEvent, just emitted by the server instead of
the SPA.

## Voice chat routing (implemented for saved threads)

**Status**: the browser reserves a saved conversation ID when voice capture
starts and sends it with `voice_stop`. The server passes the transcript to the
regular Controller chat route. Once that route returns its committed assistant
response, the server chunks the text at sentence boundaries and sends those
chunks to local TTS. Incognito capture sends no saved conversation ID and
leaves its transcript in the composer draft.

**Follow-up**: qualify the real-browser reconnect, failure recovery, and
first-task journey; reconcile conversation title generation for a first voice
turn with the existing typed-send path.

## Streaming TTS feedback to the runner (deferred)

**Status**: chat tokens now drive sentence-level synthesis while the
assistant turn runs. Barge-in cancels the active chat future and fires
`KokoroClient::cancel()` while SPA-side `VoicePlayback.flush()` drops queued
audio, but the runner doesn't know how much was actually heard before the
interrupt.

**Follow-up**: track `played_through_sentence` from acknowledged client
playback, not merely completion of synthesis. On `voice_interrupt`, append a structured
`VoiceInterrupted { played_through_sentence: u32 }` event so future
analytics can distinguish "user barged in immediately" from "user heard
most of it." Keep the generated-message event intact and derive the
actually-heard prefix from playback/interruption events for subsequent context.
Synthesis completion alone does not establish what the operator heard.
