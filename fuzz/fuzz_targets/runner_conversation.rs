#![no_main]
use execlaw_runner_protocol::RunnerToServer;
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeMap;

fuzz_target!(|data: &[u8]| {
    // Newline-delimited frames model reordered, duplicated, and version-skewed
    // conversations without opening a socket or invoking a dispatcher.
    let mut turn_state: BTreeMap<String, bool> = BTreeMap::new();
    for frame in data.split(|byte| *byte == b'\n').take(256) {
        let Ok(message) = serde_json::from_slice::<RunnerToServer>(frame) else {
            continue;
        };
        let (turn_id, terminal) = match message {
            RunnerToServer::TokenDelta { turn_id, .. }
            | RunnerToServer::Phase { turn_id, .. }
            | RunnerToServer::InferenceRetry { turn_id, .. }
            | RunnerToServer::ModelRoundCheckpoint { turn_id, .. }
            | RunnerToServer::ToolCallRequest { turn_id, .. }
            | RunnerToServer::EventLogAppend { turn_id, .. }
            | RunnerToServer::ControlAcknowledged { turn_id, .. } => (turn_id, false),
            RunnerToServer::TurnComplete { turn_id, .. }
            | RunnerToServer::Error { turn_id, .. } => (turn_id, true),
            RunnerToServer::HeartbeatAck { .. } => continue,
        };
        let closed = turn_state.entry(turn_id).or_default();
        // The receiver contract is terminal-once. Repeated/late frames must
        // never reopen a completed turn; they are ignored at dispatch.
        if !*closed && terminal {
            *closed = true;
        }
    }
});
