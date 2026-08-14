#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Parser should never panic on any input.
        let _ = jsntrs::Parser::parse(s);
    }
});
