#![no_main]

//! Fuzz target: `format()` must never panic — on user input, or on its own
//! output — and its output must be a *fixed point that parses*.
//!
//! `format` scans the raw bytes for block comments *before* handing the
//! source to the parser, so byte-index handling is the interesting axis: an
//! unterminated `/*` used to leave the cursor mid-character and panic on
//! input as small as `/*€` (jsntrs-ecq.2). Malformed input must come back as
//! a `JsonataError`, never as a crash.
//!
//! The stronger invariants — formatted output re-parses, and formatting is
//! idempotent — are asserted here now. They did not hold when this target was
//! written: it found four unrelated, pre-existing round-trip gaps in its first
//! minutes, each since fixed under its own issue:
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
//! those and to each other. One is fixed — `emit` dropped the `{…}` group of a
//! unary node (`-a{"k": 1}` → `-a`) and every `keep_array` outside
//! `Name`/`Variable`/`Block` (`a[0][]` → `a[0]`), and a path group hoisted past
//! a negated step changed the re-parsed step count (jsntrs-ecq.9). The rest are
//! left for their own issues and fenced off in `known_unstable_gap` below,
//! which is the authoritative list:
//!
//! - **comment scan vs. names and regexes.** `extract_comments` skips string
//!   literals, but not backtick-quoted names and not regex literals, so a
//!   comment marker inside either is mistaken for a comment: `` `a/*b*/c` ``
//!   formats to itself plus a stray `/*b*/` line, and every further pass
//!   appends another copy (`λ/*/6*/*/\/*\x08*/` is the regex spelling of the
//!   same thing). A lone quote inside a name is the other half of it —
//!   `` `a'b` `` followed by `/*c*/` silently *drops* the real comment,
//!   because the scan then treats the rest of the source as an unclosed
//!   string literal. (`highlight.rs` already skips backtick names; the
//!   formatter's own scanner does not.) Broad enough that any comment in the
//!   text is exempt from idempotence; comments never broke *parsing*, so
//!   that assertion still applies to them.
//! - **numeric path steps welded by the joining dot.** Steps are joined with
//!   a bare `.`, so the two-step path `0 . 0` formats to `0.0`, which lexes
//!   back as one number. Invisible until a fold depends on it: `1 - --0 . 0`
//!   formats to `1 - --0.0` and then to `1 - 0.0`, because the parser folds
//!   unary minus into a number literal but not into a path.
//! - **`trim_end` eats token text.** `format` finishes with
//!   `String::trim_end`, whose notion of whitespace is Unicode's and so wider
//!   than the lexer's (` `, `\t`, `\n`, `\r`, `\x0b`). A trailing `\x0c`,
//!   `\u{a0}`, `\u{2028}`, … belongs to the last *token* — a field name, say
//!   — and is eaten: the expression changes and, being shorter, may lay out
//!   differently on the next pass.
//!
//! A newly found gap is a bug in `format`, not in this target: fix the
//! formatter, or — if the fix has to wait — add a fence to `known_gap` with
//! its repro, and file the issue.

use libfuzzer_sys::fuzz_target;

/// Whitespace to Rust but not to the JSONata lexer: such a character belongs
/// to whatever token it sits in, yet `trim_end` treats it as padding.
fn is_alien_space(c: char) -> bool {
    c.is_whitespace() && !matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{b}')
}

/// `Some(reason)` when this pair hits a known gap that makes the *second*
/// pass differ from the first (see the header). No gap can still make the
/// formatted text fail to *parse*: that assertion is unconditional.
fn known_unstable_gap(src: &str, once: &str) -> Option<&'static str> {
    // The comment scan is blind to backtick names and regex literals, so a
    // comment marker anywhere in the text may be dropped or duplicated.
    if src.contains("/*") || once.contains("/*") {
        return Some("comment placement");
    }
    // A path dot with whitespace beside it may separate steps that the
    // formatter welds together (`0 . 0` → the number `0.0`).
    let bytes = src.as_bytes();
    let lexer_space = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0B);
    if bytes.iter().enumerate().any(|(i, &c)| {
        c == b'.'
            && (i > 0 && lexer_space(bytes[i - 1])
                || i + 1 < bytes.len() && lexer_space(bytes[i + 1]))
    }) {
        return Some("spaced path dot");
    }
    // The closing `trim_end` eats whitespace the lexer does not recognise
    // straight out of the last token.
    if src.chars().any(is_alien_space) {
        return Some("whitespace the lexer does not skip");
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
    if known_unstable_gap(src, &once).is_some() {
        return;
    }
    // … and one pass must already be the canonical form.
    assert_eq!(
        once, twice,
        "format is not idempotent\n src: {src:?}\nonce: {once:?}\ntwice: {twice:?}"
    );
});
