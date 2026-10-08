#![no_main]
use execlaw_runner_protocol::{PROTOCOL_VERSION, RegistrationAck};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() <= 64 * 1024 {
        if let Ok(ack) = serde_json::from_slice::<RegistrationAck>(data) {
            if ack.protocol_version != PROTOCOL_VERSION {
                return;
            }
            debug_assert_eq!(ack.protocol_version, PROTOCOL_VERSION);
        }
    }
});
