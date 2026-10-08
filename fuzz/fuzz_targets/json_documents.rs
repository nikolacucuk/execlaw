#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() <= 1024 * 1024 {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) {
            let _ = jsonschema::validator_for(&value);
        }
        if let Ok(text) = std::str::from_utf8(data) {
            let _ = toml::from_str::<toml::Value>(text);
        }
    }
});
