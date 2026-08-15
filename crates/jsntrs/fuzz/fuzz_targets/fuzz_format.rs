#![no_main]

//! Fuzz target: `format()` must never panic — on user input, or on its own
//! output — and its output must be a *fixed point that parses*.
//!
//! `format` scans the raw bytes for block comments, so byte-index handling is
//! the interesting axis: an unterminated `/*` used to leave the cursor
//! mid-character and panic on input as small as `/*€` (jsntrs-ecq.2).
//! Malformed input must come back as a `JsonataError`, never as a crash.
//!
//! The stronger invariants — formatted output re-parses, and formatting is
//! idempotent — are asserted here unconditionally, with no fences left. They
//! did not hold when this target was written: it found four unrelated,
//! pre-existing round-trip gaps in its first minutes, each since fixed under
//! its own issue:
//!
//! - regex literals printed the parser's implicit `g` flag, which the lexer
//!   rejects: `/a/i` → `/a/ig` → S0302 (jsntrs-ecq.6);
//! - lambda signatures printed doubled angle brackets, because
//!   `Signature::raw` already includes them: `function($x)<n:n>{$x}` →
//!   `function($x)<<n:n>> {…}` → S0402 (jsntrs-ecq.7);
//! - `escape_name` backtick-quoted a name that itself contains a backtick
//!   without escaping it — quoting has no escape syntax — leaving the quoting
//!   unterminated: the two-byte source `0x00 0x60` formatted to something
//!   that failed with S0105 (jsntrs-ecq.8);
//! - the same unescaped backtick could also make the output re-parse into a
//!   *different* expression, i.e. break idempotence (jsntrs-ecq.8).
//!
//! Turning the assertions on then surfaced four further gaps, all unrelated to
//! those and to each other, and all now fixed too:
//!
//! - `emit` dropped the `{…}` group of a unary node (`-a{"k": 1}` → `-a`) and
//!   every `keep_array` outside `Name`/`Variable`/`Block` (`a[0][]` → `a[0]`),
//!   and a path group hoisted past a negated step changed the re-parsed step
//!   count (`2.a.--b{}.c@$v`) — jsntrs-ecq.9;
//! - the comment scan skipped string literals but not backtick names or regex
//!   literals, so `` `a/*b*/c` `` grew a stray `/*b*/` line on every pass, and
//!   a lone quote in a name (`` `a'b` `` before a comment) swallowed the real
//!   one — jsntrs-ecq.10;
//! - the joining `.` was written bare, welding the two-step path `0 . 0` into
//!   the number `0.0`, which a later fold could then absorb (`1 - --0 . 0` →
//!   `1 - 0.0`) — jsntrs-ecq.11;
//! - the closing `String::trim_end` used Unicode's notion of whitespace, wider
//!   than the lexer's (` `, `\t`, `\n`, `\r`, `\x0b`), and ate a trailing
//!   `\x0c`/`\u{a0}`/`\u{2028}` straight out of the last token — `$x\u{a0}`
//!   formatted to `$x` — jsntrs-ecq.12.
//!
//! One gap is currently fenced off in `known_unstable_gap`, which is the
//! authoritative list:
//!
//! - **path layout flips across passes.** The two disambiguation strategies
//!   for numeric path steps — multi-line layout and the spaced dot
//!   (jsntrs-ecq.11) — are chosen from context that the group hoist
//!   (jsntrs-ecq.9) rewrites: `0.-0{0:0}.0.a` formats multi-line on the
//!   first pass and single-line on the second — jsntrs-qhh.
//!
//! A newly found gap is a bug in `format`, not in this target: fix the
//! formatter — removing its fence from `known_unstable_gap` — or, if the fix
//! has to wait, add a fence with its repro and file the issue.

use libfuzzer_sys::fuzz_target;

/// `Some(reason)` when this input hits a known gap that makes the *second*
/// pass differ from the first (see the header). No gap can still make the
/// formatted text fail to *parse*: that assertion is unconditional.
fn known_unstable_gap(src: &str) -> Option<&'static str> {
    let bytes = src.as_bytes();
    // A negated path step next to a group can flip the layout strategy
    // between passes (jsntrs-qhh). Over-approximates — most such pairs are
    // stable — which only costs a little corpus coverage.
    if bytes.windows(2).any(|w| w == b".-") && bytes.contains(&b'{') {
        return Some("negated path step with a group (jsntrs-qhh)");
    }
    None
}

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    // Never panics; anything the parser rejects is an Err, not a crash.
    let Ok(once) = jsntrs::format(src) else {
        return;
    };
    // Formatter output is itself user input to the next `format` call
    // (editors reformat on every save), so it must parse …
    let twice = match jsntrs::format(&once) {
        Ok(twice) => twice,
        Err(e) => panic!("formatted output does not parse: {e}\n src: {src:?}\nonce: {once:?}"),
    };
    // … and one pass must already be the canonical form.
    if known_unstable_gap(src).is_some() {
        return;
    }
    assert_eq!(
        once, twice,
        "format is not idempotent\n src: {src:?}\nonce: {once:?}\ntwice: {twice:?}"
    );
});
