#![no_main]

use libfuzzer_sys::fuzz_target;
use postly_core::parse_curl_command;

fuzz_target!(|data: &[u8]| {
    if let Ok(command) = std::str::from_utf8(data) {
        let _ = parse_curl_command(command);
    }
});
