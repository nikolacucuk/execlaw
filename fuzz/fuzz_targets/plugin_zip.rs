#![no_main]
use execlaw_plugin_sdk::stage_zip;
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    if data.len() <= 1024 * 1024 {
        let _ = stage_zip(Cursor::new(data));
    }
});
