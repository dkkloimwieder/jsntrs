//! Regex support: $match, $replace, and regex compilation helpers.
//!
//! Port of Go `functions/string_match_replace.go` and `internal/evaluator/eval_regex.go`.

use std::rc::Rc;

// Backend selection: `regex` wins when both features are enabled.
#[cfg(feature = "regex")]
use regex::{Captures, Match, Regex, escape as regex_escape};

#[cfg(all(feature = "regex-lite", not(feature = "regex")))]
use regex_lite::{Captures, Match, Regex, escape as regex_escape};

#[cfg(not(any(feature = "regex", feature = "regex-lite")))]
compile_error!(
    "jsntrs requires a regex backend: enable feature \"regex\" (default) or \"regex-lite\""
);

use crate::error::{JsonataError, JsonataResult};
use crate::evaluator::{Environment, FunctionValue, call_function};
use crate::parser::AstArena;
use crate::value::Value;

/// Compiled regexes cached per thread, keyed by `"<flags>\0<pattern>"`
/// (flags first — they're short and `'\0'` can never appear in them).
/// Go caches compiled regexes in a `sync.Map`; `Value` is `!Send`, so a
/// thread_local suffices here. Bounded: cleared wholesale when full so
/// dynamically generated patterns can't grow it without bound.
const REGEX_CACHE_CAP: usize = 256;

/// Flags component marking an escaped string-literal pattern
/// (`$match(s, "a.b")` matches the literal text). Never a real flag
/// character, so these entries can't collide with `<pattern>` + flags.
const LITERAL_MARKER: char = '\u{1}';

thread_local! {
    static REGEX_CACHE: std::cell::RefCell<
        std::collections::HashMap<compact_str::CompactString, Rc<Regex>, foldhash::fast::RandomState>,
    > = std::cell::RefCell::new(std::collections::HashMap::default());
}

fn cache_key(flags_component: &str, pattern: &str) -> compact_str::CompactString {
    let mut key =
        compact_str::CompactString::with_capacity(flags_component.len() + 1 + pattern.len());
    key.push_str(flags_component);
    key.push('\0');
    key.push_str(pattern);
    key
}

/// Return the cached regex for `key`, compiling (and caching) on miss.
fn cached_or_compile(
    key: compact_str::CompactString,
    compile: impl FnOnce() -> Result<Regex, JsonataError>,
) -> Result<Rc<Regex>, JsonataError> {
    REGEX_CACHE.with(|cache| {
        if let Some(re) = cache.borrow().get(key.as_str()) {
            return Ok(Rc::clone(re));
        }
        let re = Rc::new(compile()?);
        let mut map = cache.borrow_mut();
        if map.len() >= REGEX_CACHE_CAP {
            map.clear();
        }
        map.insert(key, Rc::clone(&re));
        Ok(re)
    })
}

/// Compile a regex from a pattern string and flags, with per-thread caching.
///
/// # Errors
/// Returns `D3137` if the pattern is not a valid regular expression.
pub fn compile_regex(pattern: &str, flags: &str) -> Result<Rc<Regex>, JsonataError> {
    cached_or_compile(cache_key(flags, pattern), || {
        compile_regex_uncached(pattern, flags)
    })
}

fn compile_regex_uncached(pattern: &str, flags: &str) -> Result<Regex, JsonataError> {
    let mut inline = String::new();
    if flags.contains('i') {
        inline.push('i');
    }
    if flags.contains('m') {
        inline.push('m');
    }
    if flags.contains('s') {
        inline.push('s');
    }
    let full = if inline.is_empty() {
        pattern.to_string()
    } else {
        format!("(?{inline}){pattern}")
    };
    Regex::new(&full).map_err(|e| JsonataError::new("D3137", format!("invalid regex: {e}")))
}

/// Compile a regex from a Value (string or regex object {pattern, flags}).
fn compile_regex_arg(v: &Value) -> Result<Rc<Regex>, JsonataError> {
    match v {
        Value::String(s) => {
            let mut key = compact_str::CompactString::with_capacity(s.len() + 2);
            key.push(LITERAL_MARKER);
            key.push('\0');
            key.push_str(s);
            cached_or_compile(key, || {
                let escaped = regex_escape(s);
                Regex::new(&escaped)
                    .map_err(|e| JsonataError::new("D3137", format!("regex error: {e}")))
            })
        }
        Value::Object(obj) => {
            let pattern: &str = match obj.get("pattern") {
                Some(Value::String(s)) => s,
                _ => "",
            };
            let flags: &str = match obj.get("flags") {
                Some(Value::String(s)) => s,
                _ => "",
            };
            compile_regex(pattern, flags)
        }
        _ => Err(JsonataError::new(
            "T0410",
            "expected a string or regex pattern",
        )),
    }
}

/// Running (byte offset, char count) cursor over a subject string. Matches
/// arrive in ascending order, so each char-index computation advances from
/// the previous position instead of recounting from the start of the string
/// — which made many-match `$match`/`$replace` quadratic.
struct CharCursor {
    byte: usize,
    chars: usize,
}

impl CharCursor {
    fn new() -> Self {
        Self { byte: 0, chars: 0 }
    }

    /// Char index of `byte_pos`, which must not precede the previous call's.
    fn char_index(&mut self, s: &str, byte_pos: usize) -> usize {
        self.chars += s[self.byte..byte_pos].chars().count();
        self.byte = byte_pos;
        self.chars
    }
}

/// Build a match result object from a regex match.
fn build_match_object(s: &str, caps: &Captures, m: &Match, cursor: &mut CharCursor) -> Value {
    let match_str: compact_str::CompactString = m.as_str().into();
    let start = cursor.char_index(s, m.start()) as f64;
    let end = cursor.char_index(s, m.end()) as f64;

    let mut groups = Vec::new();
    for i in 1..caps.len() {
        match caps.get(i) {
            Some(g) => groups.push(Value::String(g.as_str().into())),
            None => groups.push(Value::String("".into())),
        }
    }

    let mut obj = crate::value::ObjectMap::default();
    obj.insert("match".into(), Value::String(match_str));
    obj.insert("start".into(), Value::Number(start));
    obj.insert("end".into(), Value::Number(end));
    obj.insert("groups".into(), Value::Array(Rc::from(groups)));
    Value::Object(Rc::new(obj))
}

/// `$match(str, pattern, limit?)`
///
/// # Errors
/// Returns `T0410` for type mismatches and `D3137` for invalid regex patterns.
pub fn fn_match(
    args: &[Value],
    _focus: &Value,
    env: &Rc<Environment>,
    arena: &AstArena,
) -> JsonataResult {
    if args.len() < 2 {
        return Err(JsonataError::new(
            "T0410",
            "$match: requires at least 2 arguments",
        ));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let s: &str = match &args[0] {
        Value::String(s) => s,
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$match: argument 1 must be a string",
            ));
        }
    };

    // Go: absent/undefined or negative -> unlimited; non-number -> T0410;
    // fractional truncates.
    let limit: Option<usize> = match args.get(2) {
        None | Some(Value::Undefined) => None,
        Some(Value::Number(n)) => {
            if *n < 0.0 {
                None
            } else {
                Some(*n as usize)
            }
        }
        Some(_) => {
            return Err(JsonataError::new(
                "T0410",
                "$match: argument 3 must be a number",
            ));
        }
    };

    // If the second argument is a function, use custom matcher protocol.
    if let Value::Function(func) = &args[1] {
        return match_with_custom_matcher(s, func, limit, env, arena);
    }

    let re = compile_regex_arg(&args[1])?;

    let mut result = Vec::new();
    let mut cursor = CharCursor::new();
    for caps in re.captures_iter(s) {
        if let Some(lim) = limit
            && result.len() >= lim
        {
            break;
        }
        if let Some(m) = caps.get(0) {
            result.push(build_match_object(s, &caps, &m, &mut cursor));
        }
    }

    Ok(match_sequence(result))
}

/// Wrap `$match` results in the internal sequence the reference builds with
/// `createSequence()`: the singleton collapse belongs to the consumer of the
/// call, which is what lets `$match(s, r)[]` keep a single match wrapped
/// (jsntrs-e8l, jsntrs-p0v.6).
fn match_sequence(matches: Vec<Value>) -> Value {
    Value::Sequence(Box::new(crate::value::Sequence::with_items(matches)))
}

/// Custom matcher: call a function that returns {match, start, groups, next} objects.
fn match_with_custom_matcher(
    s: &str,
    matcher_fn: &FunctionValue,
    limit: Option<usize>,
    env: &Rc<Environment>,
    arena: &AstArena,
) -> JsonataResult {
    let mut result = Vec::new();

    // Initial call: matcher_fn(str, 0)
    let mut res = call_function(
        matcher_fn,
        &[Value::String(s.into()), Value::Number(0.0)],
        &Value::Undefined,
        env,
        arena,
    )?;

    while let Value::Object(obj) = &res {
        let match_val = obj.get("match").cloned().unwrap_or(Value::Undefined);
        let start_val = obj.get("start").cloned().unwrap_or(Value::Undefined);
        let groups_val = obj
            .get("groups")
            .cloned()
            .unwrap_or(Value::Array(Rc::from(vec![])));

        let mut match_obj = crate::value::ObjectMap::default();
        match_obj.insert("match".into(), match_val);
        match_obj.insert("index".into(), start_val);
        match_obj.insert("groups".into(), groups_val);
        result.push(Value::Object(Rc::new(match_obj)));

        if let Some(lim) = limit
            && result.len() >= lim
        {
            break;
        }

        // Get the next function and call it.
        let next_fn = match obj.get("next") {
            Some(Value::Function(f)) => f.clone(),
            _ => break,
        };
        res = call_function(&next_fn, &[], &Value::Undefined, env, arena)?;
    }

    Ok(match_sequence(result))
}

/// `$replace(str, pattern, replacement, limit?)`
///
/// # Errors
/// Returns `T0410` for type mismatches, `D3137` for invalid regex, and `D1004` on match failure.
pub fn fn_replace(
    args: &[Value],
    _focus: &Value,
    env: &Rc<Environment>,
    arena: &AstArena,
) -> JsonataResult {
    if args.is_empty() || args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let s: compact_str::CompactString = match &args[0] {
        Value::String(s) => s.clone(),
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$replace: argument 1 must be a string",
            ));
        }
    };
    if args.len() < 3 {
        return Err(JsonataError::new(
            "T0410",
            "$replace: argument 3 (replacement) is required",
        ));
    }

    // Reference signature `<s-(sf)(sf)n?:s>`: the limit parameter accepts a
    // number, `undefined`, or nothing at all. Every other type — null, string,
    // boolean, array, object, function — fails signature validation with
    // T0410 before the D3011 range check gets a say (jsntrs-p0v.4).
    let limit: Option<usize> = match args.get(3) {
        None | Some(Value::Undefined) => None,
        Some(Value::Number(n)) => {
            if *n < 0.0 {
                return Err(JsonataError::new(
                    "D3011",
                    "$replace: fourth argument must not be negative",
                ));
            }
            Some(*n as usize)
        }
        Some(_) => {
            return Err(JsonataError::new(
                "T0410",
                "$replace: fourth argument must be a number",
            ));
        }
    };

    // String pattern with string replacement — simple case.
    if let (Value::String(pattern), Value::String(replacement)) = (&args[1], &args[2]) {
        if pattern.is_empty() {
            return Err(JsonataError::new(
                "D3010",
                "$replace: pattern cannot be an empty string",
            ));
        }
        return Ok(Value::String(
            replace_n_literal(&s, pattern, replacement, limit).into(),
        ));
    }

    // Regex pattern.
    let re = compile_regex_arg(&args[1])?;

    match &args[2] {
        Value::String(replacement) => {
            replace_regex_string(&s, &re, replacement, limit).map(|s| Value::String(s.into()))
        }
        Value::Function(func) => {
            replace_with_fn(&s, &re, func, limit, env, arena).map(|s| Value::String(s.into()))
        }
        _ => Err(JsonataError::new(
            "T0410",
            "$replace: argument 3 must be a string or function",
        )),
    }
}

fn replace_n_literal(s: &str, old: &str, replacement: &str, limit: Option<usize>) -> String {
    match limit {
        None => s.replace(old, replacement),
        Some(0) => s.to_string(),
        Some(lim) => {
            let mut result = String::new();
            let mut remaining = s;
            let mut count = 0;
            while count < lim {
                if let Some(idx) = remaining.find(old) {
                    result.push_str(&remaining[..idx]);
                    result.push_str(replacement);
                    remaining = &remaining[idx + old.len()..];
                    count += 1;
                } else {
                    break;
                }
            }
            result.push_str(remaining);
            result
        }
    }
}

fn replace_regex_string(
    s: &str,
    re: &Regex,
    repl: &str,
    limit: Option<usize>,
) -> Result<String, JsonataError> {
    let mut result = String::new();
    let mut prev = 0;
    for (count, caps) in re.captures_iter(s).enumerate() {
        if let Some(lim) = limit
            && count >= lim
        {
            break;
        }
        let m = caps
            .get(0)
            .ok_or_else(|| JsonataError::new("D1004", "$replace: failed to get regex match"))?;
        if m.as_str().is_empty() {
            return Err(JsonataError::new(
                "D1004",
                "$replace: the regex matched a zero-length string",
            ));
        }
        result.push_str(&s[prev..m.start()]);

        // Expand replacement template with back-references.
        let groups: Vec<&str> = (1..caps.len())
            .map(|i| caps.get(i).map_or("", |g| g.as_str()))
            .collect();
        result.push_str(&expand_replacement(repl, m.as_str(), &groups));

        prev = m.end();
    }
    result.push_str(&s[prev..]);
    Ok(result)
}

fn replace_with_fn(
    s: &str,
    re: &Regex,
    func: &FunctionValue,
    limit: Option<usize>,
    env: &Rc<Environment>,
    arena: &AstArena,
) -> Result<String, JsonataError> {
    let mut result = String::new();
    let mut prev = 0;
    let mut cursor = CharCursor::new();

    for (count, caps) in re.captures_iter(s).enumerate() {
        if let Some(lim) = limit
            && count >= lim
        {
            break;
        }
        let m = caps
            .get(0)
            .ok_or_else(|| JsonataError::new("D1004", "$replace: failed to get regex match"))?;
        if m.as_str().is_empty() {
            return Err(JsonataError::new(
                "D1004",
                "$replace: the regex matched a zero-length string",
            ));
        }
        result.push_str(&s[prev..m.start()]);

        let match_obj = build_match_object(s, &caps, &m, &mut cursor);
        let val = call_function(func, &[match_obj], &Value::Undefined, env, arena)?;
        match val {
            Value::String(sv) => result.push_str(&sv),
            _ => {
                return Err(JsonataError::new(
                    "D3012",
                    "$replace: replacement function must return a string",
                ));
            }
        }

        prev = m.end();
    }
    result.push_str(&s[prev..]);
    Ok(result)
}

/// Expand a JSONata replacement template with back-references ($0, $1, etc.)
fn expand_replacement(repl: &str, full_match: &str, groups: &[&str]) -> String {
    let mut result = String::new();
    let bytes = repl.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'$' {
            // Copy the literal run whole: pushing `bytes[i] as char` would
            // Latin-1-widen each byte of a multi-byte UTF-8 character. A `$`
            // byte is always a char boundary, so the slice is valid.
            let start = i;
            while i < bytes.len() && bytes[i] != b'$' {
                i += 1;
            }
            result.push_str(&repl[start..i]);
            continue;
        }
        i += 1; // skip $
        if i >= bytes.len() {
            result.push('$');
            break;
        }
        if bytes[i] == b'$' {
            result.push('$');
            i += 1;
            continue;
        }
        if !bytes[i].is_ascii_digit() {
            // `$` not followed by a digit stays literal; the next iteration's
            // literal-run copy picks up the following character intact.
            result.push('$');
            continue;
        }
        // Collect digit run.
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let num_str = &repl[start..i];
        let n: usize = num_str.parse().unwrap_or(0);
        if n == 0 {
            result.push_str(full_match);
        } else if n <= groups.len() {
            result.push_str(groups[n - 1]);
        } else {
            // Try shorter prefixes.
            let mut found = false;
            for plen in (1..num_str.len()).rev() {
                let p: usize = num_str[..plen].parse().unwrap_or(0);
                if p == 0 {
                    result.push_str(full_match);
                    result.push_str(&num_str[plen..]);
                    found = true;
                    break;
                }
                if p <= groups.len() {
                    result.push_str(groups[p - 1]);
                    result.push_str(&num_str[plen..]);
                    found = true;
                    break;
                }
            }
            if !found && num_str.len() > 1 {
                result.push_str(&num_str[1..]);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::eval;
    use crate::parser::{Parser, process_ast};

    /// Helper: parse, process, and evaluate a full expression.
    fn eval_expr(src: &str) -> Value {
        let (mut arena, root) = Parser::parse(src).expect("parse failed");
        let root = process_ast(&mut arena, root).expect("process failed");
        let mut env = Environment::new();
        crate::stdlib::register_all(&mut env);
        let env = Rc::new(env);
        eval(&arena, root, &Value::Undefined, &env).expect("eval failed")
    }

    #[test]
    fn compile_regex_applies_inline_flags() {
        assert!(compile_regex("abc", "i").unwrap().is_match("xABCy"));
        assert!(!compile_regex("abc", "").unwrap().is_match("xABCy"));
        assert_eq!(compile_regex("(", "").unwrap_err().code, "D3137");
    }

    /// Char positions across multiple matches: the running cursor must
    /// agree with a from-scratch count even over multibyte chars.
    #[test]
    fn match_positions_stay_correct_across_many_matches() {
        let m = eval_expr(r#"$match("aéxaéxaéx", /x/).start"#);
        let expected = eval_expr("[2, 5, 8]");
        assert!(m.deep_equal(&expected), "got {m:?}");
        let m = eval_expr(r#"$match("aéxaéxaéx", /x/).end"#);
        let expected = eval_expr("[3, 6, 9]");
        assert!(m.deep_equal(&expected), "got {m:?}");
    }

    #[test]
    fn cache_returns_shared_instance() {
        let a = compile_regex("cache_probe_[0-9]+", "i").unwrap();
        let b = compile_regex("cache_probe_[0-9]+", "i").unwrap();
        assert!(Rc::ptr_eq(&a, &b), "second compile should hit the cache");
        // Different flags are a different entry.
        let c = compile_regex("cache_probe_[0-9]+", "").unwrap();
        assert!(!Rc::ptr_eq(&a, &c));
    }

    /// A string argument matches literally; a regex with the same source
    /// text keeps its metacharacters. The two must not share a cache slot.
    #[test]
    fn escaped_literal_does_not_collide_with_pattern() {
        let pattern = compile_regex("a.b", "").unwrap();
        let literal = compile_regex_arg(&Value::String("a.b".into())).unwrap();
        assert!(pattern.is_match("axb"));
        assert!(!literal.is_match("axb"));
        assert!(literal.is_match("a.b"));
    }

    #[test]
    fn cache_clears_when_full_without_dropping_correctness() {
        for i in 0..(2 * REGEX_CACHE_CAP + 10) {
            let re = compile_regex(&format!("full_probe_{i}_[a-z]"), "").unwrap();
            assert!(re.is_match(&format!("full_probe_{i}_x")));
        }
    }

    /// Match positions are character indices, not byte offsets, and
    /// capture groups are reported in order.
    #[test]
    fn match_reports_char_indices_and_groups() {
        let m = eval_expr(r#"$match("héllo world", /(l+)o/)"#);
        let expected = eval_expr(r#"{"match": "llo", "start": 2, "end": 5, "groups": ["ll"]}"#);
        assert!(m.deep_equal(&expected), "got {m:?}");
    }

    /// $match limit semantics, Go-verified 2026-08-07: negative means
    /// unlimited, zero means no matches, fractional truncates, and a
    /// non-numeric limit is T0410 (gnata-nuo.7).
    #[test]
    fn match_limit_edge_cases() {
        assert_eq!(
            eval_expr(r#"$count($match("ababab", /ab/, -1))"#),
            Value::Number(3.0)
        );
        assert_eq!(
            eval_expr(r#"$count($match("ababab", /ab/, 0))"#),
            Value::Number(0.0)
        );
        assert_eq!(
            eval_expr(r#"$count($match("ababab", /ab/, 2))"#),
            Value::Number(2.0)
        );
        assert_eq!(
            eval_expr(r#"$count($match("ababab", /ab/, 1.9))"#),
            Value::Number(1.0)
        );
        let (mut arena, root) = Parser::parse(r#"$match("ababab", /ab/, "x")"#).unwrap();
        let root = process_ast(&mut arena, root).unwrap();
        let mut env = Environment::new();
        crate::stdlib::register_all(&mut env);
        let env = Rc::new(env);
        let err = eval(&arena, root, &Value::Undefined, &env).unwrap_err();
        assert_eq!(err.code, "T0410");
    }

    /// $replace supports $N group references (JSONata documentation
    /// example).
    #[test]
    fn replace_supports_group_references() {
        let r = eval_expr(r#"$replace("John Smith", /(\w+)\s(\w+)/, "$2 $1")"#);
        assert!(
            r.deep_equal(&Value::String("Smith John".into())),
            "got {r:?}"
        );
    }

    /// Non-ASCII replacement text must survive template expansion intact
    /// (the byte-wise expansion used to Latin-1-widen multi-byte UTF-8).
    #[test]
    fn replace_preserves_non_ascii_replacement() {
        let r = eval_expr(r#"$replace("hello", /l/, "ü")"#);
        assert!(r.deep_equal(&Value::String("heüüo".into())), "got {r:?}");
        // `$` followed by a non-ASCII char: `$` stays literal, char intact.
        let r = eval_expr(r#"$replace("ab", /b/, "$€")"#);
        assert!(r.deep_equal(&Value::String("a$€".into())), "got {r:?}");
        // Group references mixed with non-ASCII literals.
        let r = eval_expr(r#"$replace("John Smith", /(\w+)\s(\w+)/, "«$2» — «$1»")"#);
        assert!(
            r.deep_equal(&Value::String("«Smith» — «John»".into())),
            "got {r:?}"
        );
    }

    #[test]
    fn split_by_regex() {
        let r = eval_expr(r#"$split("a1b22c", /\d+/)"#);
        let expected = eval_expr(r#"["a", "b", "c"]"#);
        assert!(r.deep_equal(&expected), "got {r:?}");
    }

    #[test]
    fn contains_accepts_regex_patterns() {
        assert!(eval_expr(r#"$contains("abracadabra", /a.*a/)"#).deep_equal(&Value::Bool(true)));
        assert!(eval_expr(r#"$contains("abc", /\d/)"#).deep_equal(&Value::Bool(false)));
    }
}
