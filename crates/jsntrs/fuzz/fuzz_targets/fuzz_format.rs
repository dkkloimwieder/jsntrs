#![no_main]

//! Fuzz target: `format()` must never panic — on user input, or on its own
//! output.
//!
//! `format` scans the raw bytes for block comments *before* handing the
//! source to the parser, so byte-index handling is the interesting axis: an
//! unterminated `/*` used to leave the cursor mid-character and panic on
//! input as small as `/*€` (jsntrs-ecq.2). Malformed input must come back as
//! a `JsonataError`, never as a crash.
//!
//! The stronger invariants — formatted output re-parses, and formatting is
//! idempotent — are deliberately *not* asserted yet: neither holds today.
//! This target found four unrelated, pre-existing round-trip gaps in its
//! first minutes, each of which needs its own fix:
//!
//! - regex literals print the parser's implicit `g` flag, which the lexer
//!   rejects: `/a/i` → `/a/ig` → S0302;
//! - lambda signatures print doubled angle brackets, because
//!   `Signature::raw` already includes them: `function($x)<n:n>{$x}` →
//!   `function($x)<<n:n>> {…}` → S0402;
//! - `escape_name` backtick-quotes a name that itself contains a backtick
//!   without escaping it, leaving the quoting unterminated: the two-byte
//!   source `0x00 0x60` formats to something that fails with S0105;
//! - the same unescaped backtick can also make the output re-parse into a
//!   *different* expression, i.e. break idempotence.
//!
//! Turn those assertions on as the gaps are closed.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    // Never panics; anything the parser rejects is an Err, not a crash.
    let Ok(once) = jsntrs::format(src) else {
        return;
    };
    // Formatter output is itself user input to the next `format` call
    // (editors reformat on every save), so it must be panic-free too.
    let _ = jsntrs::format(&once);
});
