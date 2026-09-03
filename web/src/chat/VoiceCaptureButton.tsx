// Phase 13.A — voice mic button + MediaRecorder capture.
//
// Click → request mic permission → capture PCM16 frames → stream
// them upstream as binary WebSocket frames. Click again → stop the
// capture graph and release the mic.
//
// Scope of 13.A: just the audio-capture-and-send wire. Server-side
// processing (VAD, STT, LLM, TTS) lands in 13.B–13.E. We send chunks
// every ~250ms so a future end-of-utterance heuristic on the server
// has fine-enough granularity to detect short pauses.
//
import { useCallback, useEffect, useRef, useState } from "react";
import Button from "react-bootstrap/Button";
import OverlayTrigger from "react-bootstrap/OverlayTrigger";
import Tooltip from "react-bootstrap/Tooltip";
import { ErrorBanner } from "../components/ErrorBanner";
import { VoiceSession } from "./VoiceSession";

interface Props {
    /**
     * Called for every captured audio chunk. Returns true when the
     * underlying WebSocket queued the bytes; false drops the chunk
     * silently. The voice pipeline's VAD tolerates short audio gaps,
     * so dropping is safer than buffering across reconnects.
     *
     * `null` when the WebSocket isn't connected at all (e.g. the
     * settings shell). The button still renders in that case so
     * the operator sees the voice affordance, but it's muted with
     * a "voice unavailable" tooltip.
     */
    sendBinary: ((bytes: ArrayBuffer) => boolean) | null;
    /**
     * Phase 13.C — fire a control message upstream. Carries
     * voice_stop on mic-off (server flushes STT + runs the agent
     * reply path). Returns false silently when the WS is offline.
     */
    sendControl?: (payload: object) => boolean;
    /**
     * Disabled when the chat shell is busy with another action
     * (e.g. composer mid-submit) or when the WebSocket is offline.
     */
    disabled?: boolean;
    /**
     * Phase 14.D — voice backend readiness. When `ready: false`
     * the button renders with a muted-mic icon (line through) +
     * a tooltip explaining what's missing (STT not configured,
     * TTS warming up, etc.). Operator clicks become no-ops.
     *
     * Optional so callers that already know voice can run can
     * skip the readiness probe entirely (tests, in-memory mocks).
     */
    readiness?: { ready: boolean; tooltip: string; loading: boolean } | null;
    /// How often the recorder slices the audio stream into chunks.
    /// Defaults to 250ms — small enough that endpointer latency
    /// stays under the 300ms budget on the server side.
    timesliceMs?: number;
}

const DEFAULT_TIMESLICE_MS = 250;

export function VoiceCaptureButton({
    sendBinary,
    sendControl,
    disabled,
    readiness,
    timesliceMs = DEFAULT_TIMESLICE_MS,
}: Props) {
    void timesliceMs;
    const [recording, setRecording] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const audioContextRef = useRef<AudioContext | null>(null);
    const processorRef = useRef<ScriptProcessorNode | null>(null);
    const sourceRef = useRef<MediaStreamAudioSourceNode | null>(null);
    const streamRef = useRef<MediaStream | null>(null);
    /// Phase 13.A closure — voice-session lifecycle. A fresh
    /// session id + seq counter is minted on every recording start;
    /// torn down on stop. `framePayload` wraps each chunk in the
    /// `[u32 header_len][JSON header][opus payload]` wire format
    /// the server-side `voice_frame::parse_frame` consumes.
    const sessionRef = useRef<VoiceSession | null>(null);

    // Releasing the mic on unmount avoids a "tab is using your mic"
    // browser indicator persisting after the component is gone.
    useEffect(() => {
        return () => {
            processorRef.current?.disconnect();
            sourceRef.current?.disconnect();
            void audioContextRef.current?.close();
            processorRef.current = null;
            sourceRef.current = null;
            audioContextRef.current = null;
            if (streamRef.current) {
                for (const track of streamRef.current.getTracks()) {
                    track.stop();
                }
                streamRef.current = null;
            }
        };
    }, []);

    const stopRecording = useCallback(() => {
        processorRef.current?.disconnect();
        sourceRef.current?.disconnect();
        void audioContextRef.current?.close();
        processorRef.current = null;
        sourceRef.current = null;
        audioContextRef.current = null;
        if (streamRef.current) {
            for (const track of streamRef.current.getTracks()) {
                track.stop();
            }
            streamRef.current = null;
        }
        // Phase 13.C — tell the server to flush STT + run the agent
        // reply path. The server's `voice_stop` handler also closes
        // the registry session and emits VoiceSessionEnded.
        const sess = sessionRef.current;
        if (sess && sendControl) {
            sendControl({ op: "voice_stop", session: sess.sessionId });
        }
        sessionRef.current = null;
        setRecording(false);
    }, [sendControl]);

    const startRecording = useCallback(async () => {
        setError(null);
        if (typeof navigator === "undefined" || !navigator.mediaDevices?.getUserMedia) {
            setError("This browser doesn't support microphone capture.");
            return;
        }
        let stream: MediaStream;
        try {
            // Browser-side echo cancellation OFF — operator decision.
            // The plan's WebRTC AEC3 (Phase 13.E) needs the browser to
            // pass through the raw mic signal so the server can reason
            // about both the mic and the speaker. Browser-built-in AEC
            // would mangle the signal first. Until 13.E lands, the
            // limitation is "agent's TTS may pick up in the mic if
            // speakers are loud" — acceptable for pre-13.E dogfood.
            // NS + AGC stay on; they don't interfere with downstream
            // AEC the way browser AEC does.
            stream = await navigator.mediaDevices.getUserMedia({
                audio: {
                    echoCancellation: false,
                    noiseSuppression: true,
                    autoGainControl: true,
                },
            });
        } catch (e) {
            setError(
                e instanceof Error
                    ? `Mic permission denied: ${e.message}`
                    : "Mic permission denied.",
            );
            return;
        }
        streamRef.current = stream;
        let audioContext: AudioContext;
        try {
            audioContext = new AudioContext();
        } catch (e) {
            // Tear down the freshly-acquired mic on construction
            // failure so the indicator dot vanishes.
            for (const t of stream.getTracks()) t.stop();
            streamRef.current = null;
            setError(
                e instanceof Error
                    ? `Couldn't start audio capture: ${e.message}`
                    : "Couldn't start audio capture.",
            );
            return;
        }
        audioContextRef.current = audioContext;
        const source = audioContext.createMediaStreamSource(stream);
        const processor = audioContext.createScriptProcessor(4096, 1, 1);
        sourceRef.current = source;
        processorRef.current = processor;
        // The server accepts PCM16 only. MediaRecorder's Opus/WebM
        // output cannot be decoded by the current voice pipeline.
        const session = new VoiceSession({
            codec: "pcm16",
            sampleRate: audioContext.sampleRate,
        });
        sessionRef.current = session;
        processor.onaudioprocess = (event) => {
            const input = event.inputBuffer.getChannelData(0);
            const pcm = new ArrayBuffer(input.length * 2);
            const view = new DataView(pcm);
            for (let i = 0; i < input.length; i += 1) {
                const sample = Math.max(-1, Math.min(1, input[i]));
                view.setInt16(i * 2, sample < 0 ? sample * 0x8000 : sample * 0x7fff, true);
            }
            const sess = sessionRef.current;
            if (sess && sendBinary !== null) sendBinary(sess.framePayload(pcm));
        };
        try {
            const silentOutput = audioContext.createGain();
            silentOutput.gain.value = 0;
            source.connect(processor);
            processor.connect(silentOutput);
            silentOutput.connect(audioContext.destination);
            setRecording(true);
        } catch (e) {
            setError(
                e instanceof Error
                    ? `Couldn't start audio capture: ${e.message}`
                    : "Couldn't start audio capture.",
            );
        }
    }, [sendBinary]);

    const onClick = useCallback(() => {
        if (recording) {
            stopRecording();
        } else {
            void startRecording();
        }
    }, [recording, startRecording, stopRecording]);

    // Determine the operator-visible state. Three exclusive cases:
    //   * `recording`  → red mic-fill, click stops.
    //   * `voiceUnavailable` (no WS) OR `!readiness.ready` (STT/TTS
    //     missing or warming up) → muted-mic icon (line through),
    //     click is a no-op, hover surfaces the tooltip explaining
    //     the setup state.
    //   * default → empty mic, click starts capture.
    const voiceUnavailable = sendBinary === null;
    const readinessKnown = readiness !== undefined && readiness !== null;
    // When `readiness` is omitted the caller is opting out of the
    // probe (tests, mocks); treat that as ready=true so the
    // button behaves as the pre-Phase-14.D version did.
    const readinessReady = readinessKnown ? readiness!.ready : true;
    const muted = voiceUnavailable || !readinessReady;
    const tooltipText = (() => {
        if (recording) return "Click to stop voice capture";
        if (voiceUnavailable)
            return "Voice unavailable — the chat WebSocket isn't connected.";
        if (readinessKnown && readiness!.loading)
            return "Checking voice backend status…";
        if (readinessKnown && !readiness!.ready) return readiness!.tooltip;
        return "Click to start voice. Speak when the indicator turns red.";
    })();

    const iconClass = recording
        ? "bi bi-mic-fill"
        : muted
        ? "bi bi-mic-mute"
        : "bi bi-mic";

    const button = (
        <Button
            type="button"
            variant={
                recording
                    ? "danger"
                    : muted
                    ? "outline-secondary"
                    : "outline-secondary"
            }
            onClick={muted ? undefined : onClick}
            disabled={disabled || muted}
            data-testid="composer-voice"
            data-mic-state={
                recording ? "recording" : muted ? "muted" : "ready"
            }
            aria-label={
                recording
                    ? "Stop voice capture"
                    : muted
                    ? `Voice unavailable: ${tooltipText}`
                    : "Start voice capture"
            }
            aria-pressed={recording}
        >
            <i className={iconClass} aria-hidden />
        </Button>
    );

    return (
        <>
            <OverlayTrigger
                placement="top"
                overlay={
                    <Tooltip id="composer-voice-tooltip">{tooltipText}</Tooltip>
                }
            >
                {button}
            </OverlayTrigger>
            <ErrorBanner
                message={error}
                onDismiss={() => setError(null)}
                className="mt-2"
                testId="composer-voice-error"
            />
        </>
    );
}
