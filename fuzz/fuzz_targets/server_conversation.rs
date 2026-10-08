#![no_main]
use execlaw_runner_protocol::{RunnerToServer, ServerToRunner};
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeSet;

fuzz_target!(|data: &[u8]| {
    let mut pending_calls = BTreeSet::new();
    let mut seen_results = BTreeSet::new();
    for frame in data.split(|byte| *byte == b'\n').take(256) {
        if let Ok(RunnerToServer::ToolCallRequest { call_id, .. }) =
            serde_json::from_slice::<RunnerToServer>(frame)
        {
            if call_id.len() <= 256 && !seen_results.contains(&call_id) {
                pending_calls.insert(call_id);
            }
            continue;
        }
        let Ok(message) = serde_json::from_slice::<ServerToRunner>(frame) else {
            continue;
        };
        match message {
            ServerToRunner::Turn(request) => {
                if request.turn_id.len() <= 256 {
                    pending_calls.clear();
                    seen_results.clear();
                }
            }
            ServerToRunner::ToolCallResult(result) => {
                if pending_calls.remove(&result.call_id) {
                    seen_results.insert(result.call_id);
                }
            }
            _ => {}
        }
    }
    debug_assert!(seen_results.iter().all(|call| !pending_calls.contains(call)));
});
