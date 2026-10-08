#![no_main]
use execlaw_plugin_sdk::PluginManifest;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() <= 256 * 1024 {
        if let Ok(text) = std::str::from_utf8(data) {
            let _ = PluginManifest::parse(text);
        }
    }
});
