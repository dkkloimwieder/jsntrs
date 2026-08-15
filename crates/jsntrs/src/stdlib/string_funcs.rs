//! String functions: $string, $length, $substring, $substringBefore, $substringAfter,
//! $uppercase, $lowercase, $trim, $pad, $contains, $split, $join,
//! $base64encode, $base64decode.

use std::rc::Rc;

use crate::error::{JsonataError, JsonataResult};
use crate::value::Value;
use base64::Engine;

use super::context_arg_code;

pub fn fn_string(args: &[Value], focus: &Value) -> JsonataResult {
    let arg = if args.is_empty() { focus } else { &args[0] };
    if arg.is_undefined() {
        return Ok(Value::Undefined);
    }
    // The Inf/NaN guard (D3001 bare, D1001 nested) lives in `stringify`
    // itself, so `$string(1/0)` and `1/0 & ''` — which the reference defines
    // in terms of the same `string()` — cannot disagree (jsntrs-x0y).
    // Arity enforced by SignedBuiltin signature at call site.
    // When called via HOF, extra args are present — only check arg[1] if it's a bool.
    let prettify = match args.get(1) {
        Some(Value::Bool(b)) => *b,
        _ => false,
    };
    arg.stringify(prettify).map(|s| Value::String(s.into()))
}

pub fn fn_length(args: &[Value], focus: &Value) -> JsonataResult {
    if args.len() > 1 {
        return Err(JsonataError::new("T0410", "$length: expects 1 argument"));
    }
    let from_focus = args.is_empty();
    let arg = if from_focus { focus } else { &args[0] };
    if arg.is_undefined() {
        if from_focus {
            return Err(JsonataError::new("T0411", "$length: argument is required"));
        }
        return Ok(Value::Undefined);
    }
    if let Value::String(s) = arg {
        Ok(Value::Number(s.chars().count() as f64))
    } else {
        Err(JsonataError::new(
            context_arg_code(from_focus),
            "$length: argument must be a string",
        ))
    }
}

pub fn fn_substring(args: &[Value], _focus: &Value) -> JsonataResult {
    // Reference signature `<s-nn?:s>`: `start` is mandatory. A lone string
    // argument leaves it unmatched, so the reference reports T0410 rather
    // than quietly substringing from 0 (jsntrs-p0v.4).
    if args.len() < 2 {
        return Err(JsonataError::new(
            "T0410",
            "$substring: requires at least 2 arguments",
        ));
    }
    if args.len() > 3 {
        return Err(JsonataError::new("T0410", "$substring: too many arguments"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let s: &str = match &args[0] {
        Value::String(s) => s,
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$substring: argument 1 must be a string",
            ));
        }
    };
    // arg2 (start) must be a number; the arity gate above guarantees it exists.
    let start = match args[1].as_f64() {
        Some(n) => n as i64,
        None => {
            return Err(JsonataError::new(
                "T0410",
                "$substring: argument 2 must be a number",
            ));
        }
    };

    let chars: Vec<char> = s.chars().collect();
    let len = chars.len() as i64;

    let actual_start = if start < 0 {
        (len + start).max(0) as usize
    } else {
        start.min(len) as usize
    };

    // arg3 (length) must be a number if provided.
    let result: String = if let Some(length_val) = args.get(2) {
        match length_val.as_f64() {
            Some(n) => {
                let length = n as usize;
                chars[actual_start..].iter().take(length).collect()
            }
            None => {
                return Err(JsonataError::new(
                    "T0410",
                    "$substring: argument 3 must be a number",
                ));
            }
        }
    } else {
        chars[actual_start..].iter().collect()
    };
    Ok(Value::String(result.into()))
}

pub fn fn_substring_before(args: &[Value], focus: &Value) -> JsonataResult {
    // Go semantics: len(args)==0 → T0411; len(args)==1 → use focus as str, arg as sep;
    // len(args)==2 → str=args[0], sep=args[1]; len(args)>2 → T0410.
    if args.len() > 2 {
        return Err(JsonataError::new(
            "T0410",
            "$substringBefore: too many arguments",
        ));
    }
    let (str_arg, sep_arg, from_context) = if args.len() == 2 {
        (&args[0], &args[1], false)
    } else if args.len() == 1 {
        (focus, &args[0], true)
    } else {
        return Err(JsonataError::new(
            "T0411",
            "$substringBefore: requires 2 arguments",
        ));
    };
    if str_arg.is_undefined() {
        return Ok(Value::Undefined);
    }
    let s: &str = if let Value::String(s) = str_arg {
        s
    } else {
        // When using focus as context and it's not a string → T0411
        return Err(JsonataError::new(
            context_arg_code(from_context),
            "$substringBefore: argument 1 must be a string",
        ));
    };
    let sep: &str = match sep_arg {
        Value::String(s) => s,
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$substringBefore: argument 2 must be a string",
            ));
        }
    };
    match s.find(sep) {
        Some(idx) => Ok(Value::String(s[..idx].into())),
        None => Ok(Value::String(s.into())),
    }
}

pub fn fn_substring_after(args: &[Value], focus: &Value) -> JsonataResult {
    // Go semantics: len(args)==0 → T0411; len(args)==1 → use focus as str, arg as sep;
    // len(args)==2 → str=args[0], sep=args[1]; len(args)>2 → T0410.
    if args.len() > 2 {
        return Err(JsonataError::new(
            "T0410",
            "$substringAfter: too many arguments",
        ));
    }
    let (str_arg, sep_arg, from_context) = if args.len() == 2 {
        (&args[0], &args[1], false)
    } else if args.len() == 1 {
        (focus, &args[0], true)
    } else {
        return Err(JsonataError::new(
            "T0411",
            "$substringAfter: requires 2 arguments",
        ));
    };
    if str_arg.is_undefined() {
        return Ok(Value::Undefined);
    }
    let s: &str = if let Value::String(s) = str_arg {
        s
    } else {
        return Err(JsonataError::new(
            context_arg_code(from_context),
            "$substringAfter: argument 1 must be a string",
        ));
    };
    let sep: &str = match sep_arg {
        Value::String(s) => s,
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$substringAfter: argument 2 must be a string",
            ));
        }
    };
    match s.find(sep) {
        Some(idx) => Ok(Value::String(s[idx + sep.len()..].into())),
        None => Ok(Value::String(s.into())),
    }
}

pub fn fn_uppercase(args: &[Value], focus: &Value) -> JsonataResult {
    let from_focus = args.is_empty();
    let arg = if from_focus { focus } else { &args[0] };
    if arg.is_undefined() {
        return Ok(Value::Undefined);
    }
    match arg {
        Value::String(s) => Ok(Value::String(s.to_uppercase())),
        _ => Err(JsonataError::new(
            context_arg_code(from_focus),
            "$uppercase: argument must be a string",
        )),
    }
}

pub fn fn_lowercase(args: &[Value], focus: &Value) -> JsonataResult {
    let from_focus = args.is_empty();
    let arg = if from_focus { focus } else { &args[0] };
    if arg.is_undefined() {
        return Ok(Value::Undefined);
    }
    match arg {
        Value::String(s) => Ok(Value::String(s.to_lowercase())),
        _ => Err(JsonataError::new(
            context_arg_code(from_focus),
            "$lowercase: argument must be a string",
        )),
    }
}

pub fn fn_trim(args: &[Value], focus: &Value) -> JsonataResult {
    let from_focus = args.is_empty();
    let arg = if from_focus { focus } else { &args[0] };
    if arg.is_undefined() {
        return Ok(Value::Undefined);
    }
    match arg {
        Value::String(s) => {
            // JSONata $trim: strip leading/trailing whitespace AND collapse internal whitespace.
            let trimmed = s.trim();
            let mut result = String::with_capacity(trimmed.len());
            let mut prev_space = false;
            for c in trimmed.chars() {
                if c.is_whitespace() {
                    if !prev_space {
                        result.push(' ');
                    }
                    prev_space = true;
                } else {
                    result.push(c);
                    prev_space = false;
                }
            }
            Ok(Value::String(result.into()))
        }
        _ => Err(JsonataError::new(
            context_arg_code(from_focus),
            "$trim: argument must be a string",
        )),
    }
}

/// Widths beyond this are rejected with D3010: unbounded width would reserve
/// `width` bytes up front (alloc abort on absurd values). Matches the Go cap.
const MAX_PAD_WIDTH: i64 = 10_000;

pub fn fn_pad(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.len() < 2 {
        return Err(JsonataError::new(
            "T0410",
            "$pad: requires at least 2 arguments",
        ));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let s: compact_str::CompactString = match &args[0] {
        Value::String(s) => s.clone(),
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$pad: first argument must be a string",
            ));
        }
    };
    let width = args[1]
        .as_f64()
        .ok_or_else(|| JsonataError::new("T0410", "$pad: width must be a number"))?
        as i64;
    if !(-MAX_PAD_WIDTH..=MAX_PAD_WIDTH).contains(&width) {
        return Err(JsonataError::new(
            "D3010",
            format!("$pad: width argument exceeds maximum of {MAX_PAD_WIDTH}"),
        ));
    }
    let pad_str: compact_str::CompactString = if args.len() >= 3 {
        match &args[2] {
            Value::String(c) if !c.is_empty() => c.clone(),
            _ => " ".into(),
        }
    } else {
        " ".into()
    };

    let char_count = s.chars().count() as i64;
    let needed = width.unsigned_abs() as usize;
    if char_count >= needed as i64 {
        return Ok(Value::String(s));
    }
    let pad_count = needed - char_count as usize;
    let padding: String = pad_str.chars().cycle().take(pad_count).collect();

    if width > 0 {
        Ok(Value::String(format!("{s}{padding}").into()))
    } else {
        Ok(Value::String(format!("{padding}{s}").into()))
    }
}

pub fn fn_contains(args: &[Value], focus: &Value) -> JsonataResult {
    // When called with 1 arg in path context, use focus as the string.
    let (str_arg, pattern_arg, from_focus) = if args.len() >= 2 {
        (&args[0], &args[1], false)
    } else if args.len() == 1 {
        (focus, &args[0], true)
    } else {
        return Err(JsonataError::new(
            "T0410",
            "$contains: requires 2 arguments",
        ));
    };
    if str_arg.is_undefined() {
        return Ok(Value::Undefined);
    }
    let s: &str = match str_arg {
        Value::String(s) => s,
        _ => {
            return Err(JsonataError::new(
                context_arg_code(from_focus),
                "$contains: first argument must be a string",
            ));
        }
    };
    match pattern_arg {
        Value::String(sub) => Ok(Value::Bool(s.contains(&**sub))),
        Value::Object(obj) if obj.contains_key("pattern") => {
            // Regex object — use compile_regex to properly handle flags.
            if let Some(Value::String(pat)) = obj.get("pattern") {
                let flags: &str = match obj.get("flags") {
                    Some(Value::String(f)) => f,
                    _ => "",
                };
                let re = crate::stdlib::regex::compile_regex(pat, flags).map_err(|e| {
                    JsonataError::new("D3010", format!("$contains: invalid regex: {}", e.message))
                })?;
                Ok(Value::Bool(re.is_match(s)))
            } else {
                Ok(Value::Bool(false))
            }
        }
        _ => Err(JsonataError::new(
            "T0410",
            "$contains: second argument must be a string or regex",
        )),
    }
}

pub fn fn_split(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new(
            "T0410",
            "$split: requires at least 2 arguments",
        ));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    // Non-string first arg → undefined
    let s: &str = match &args[0] {
        Value::String(s) => s,
        _ => return Ok(Value::Undefined),
    };
    if args.len() < 2 {
        return Err(JsonataError::new(
            "T0410",
            "$split: requires at least 2 arguments",
        ));
    }
    // Check limit arg before using it
    if let Some(limit_arg) = args.get(2)
        && !limit_arg.is_undefined()
    {
        match limit_arg.as_f64() {
            Some(n) if n < 0.0 => {
                return Err(JsonataError::new(
                    "D3020",
                    "$split: third argument must not be negative",
                ));
            }
            Some(_) => {} // valid number
            None => {
                return Err(JsonataError::new(
                    "T0410",
                    "$split: third argument must be a number",
                ));
            }
        }
    }
    let limit = args
        .get(2)
        .and_then(super::super::value::Value::as_f64)
        .map(|n| n as usize);

    let parts: Vec<Value> = match &args[1] {
        Value::String(sep) => {
            let splits: Vec<&str> = if sep.is_empty() {
                // Empty separator: split into individual characters.
                s.char_indices()
                    .map(|(i, c)| &s[i..i + c.len_utf8()])
                    .collect()
            } else {
                s.split(&**sep).collect()
            };
            let mut result: Vec<Value> = splits
                .into_iter()
                .map(|p| Value::String(p.into()))
                .collect();
            // Apply limit: return at most N items.
            if let Some(lim) = limit {
                result.truncate(lim);
            }
            result
        }
        Value::Object(obj) if obj.contains_key("pattern") => {
            if let Some(Value::String(pat)) = obj.get("pattern") {
                let flags: &str = match obj.get("flags") {
                    Some(Value::String(f)) => f,
                    _ => "",
                };
                let re = crate::stdlib::regex::compile_regex(pat, flags).map_err(|e| {
                    JsonataError::new("D3010", format!("$split: invalid regex: {}", e.message))
                })?;
                let splits: Vec<&str> = re.split(s).collect();
                let mut result: Vec<Value> = splits
                    .into_iter()
                    .map(|p| Value::String(p.into()))
                    .collect();
                if let Some(lim) = limit {
                    result.truncate(lim);
                }
                result
            } else {
                vec![Value::String(s.into())]
            }
        }
        Value::Function(_) => {
            return Err(JsonataError::new(
                "T1010",
                "$split: second argument must be a string or regex",
            ));
        }
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$split: separator must be a string or regex",
            ));
        }
    };
    Ok(Value::Array(Rc::from(parts)))
}

pub fn fn_join(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$join: argument is required"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let arr = match &args[0] {
        Value::Array(a) => a,
        Value::String(s) => return Ok(Value::String(s.clone())),
        _ => {
            return Err(JsonataError::new(
                "T0412",
                "$join: argument must be an array of strings",
            ));
        }
    };
    let sep: &str = match args.get(1) {
        Some(Value::String(s)) => s,
        None | Some(Value::Undefined) => "",
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$join: separator must be a string",
            ));
        }
    };
    // Build result directly into a single buffer.
    let mut buf = String::new();
    for (i, v) in arr.iter().enumerate() {
        match v {
            Value::String(s) => {
                if i > 0 && !sep.is_empty() {
                    buf.push_str(sep);
                }
                buf.push_str(s);
            }
            _ => {
                return Err(JsonataError::new(
                    "T0412",
                    "$join: array must contain only strings",
                ));
            }
        }
    }
    Ok(Value::String(buf.into()))
}

pub fn fn_base64_encode(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() || args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    match &args[0] {
        Value::String(s) => {
            let encoded = base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
            Ok(Value::String(encoded.into()))
        }
        _ => Err(JsonataError::new(
            "T0410",
            "$base64encode: argument must be a string",
        )),
    }
}

pub fn fn_base64_decode(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() || args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    match &args[0] {
        Value::String(s) => {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(s.as_bytes())
                .map_err(|e| JsonataError::new("D3010", format!("$base64decode: {e}")))?;
            let result = String::from_utf8(decoded)
                .map_err(|e| JsonataError::new("D3010", format!("$base64decode: {e}")))?;
            Ok(Value::String(result.into()))
        }
        _ => Err(JsonataError::new(
            "T0410",
            "$base64decode: argument must be a string",
        )),
    }
}

pub fn fn_encode_url(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() || args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    match &args[0] {
        Value::String(s) => {
            // encodeUrl preserves URI-safe characters.
            let encoded =
                percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC)
                    .to_string();
            // Restore URI-safe chars that shouldn't be encoded.
            let encoded = encoded
                .replace("%2F", "/")
                .replace("%3A", ":")
                .replace("%40", "@")
                .replace("%21", "!")
                .replace("%24", "$")
                .replace("%26", "&")
                .replace("%27", "'")
                .replace("%28", "(")
                .replace("%29", ")")
                .replace("%2A", "*")
                .replace("%2B", "+")
                .replace("%2C", ",")
                .replace("%3B", ";")
                .replace("%3D", "=")
                .replace("%3F", "?")
                .replace("%23", "#")
                .replace("%5B", "[")
                .replace("%5D", "]")
                .replace("%2D", "-")
                .replace("%2E", ".")
                .replace("%5F", "_")
                .replace("%7E", "~");
            Ok(Value::String(encoded.into()))
        }
        _ => Err(JsonataError::new(
            "T0410",
            "$encodeUrl: argument must be a string",
        )),
    }
}

pub fn fn_encode_url_component(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() || args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    match &args[0] {
        Value::String(s) => {
            let encoded =
                percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC)
                    .to_string()
                    .replace("%21", "!")
                    .replace("%27", "'")
                    .replace("%28", "(")
                    .replace("%29", ")")
                    .replace("%2A", "*")
                    .replace("%2D", "-")
                    .replace("%2E", ".")
                    .replace("%5F", "_")
                    .replace("%7E", "~");
            Ok(Value::String(encoded.into()))
        }
        _ => Err(JsonataError::new(
            "T0410",
            "$encodeUrlComponent: argument must be a string",
        )),
    }
}

pub fn fn_decode_url(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() || args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    match &args[0] {
        Value::String(s) => {
            let decoded = percent_encoding::percent_decode_str(s)
                .decode_utf8()
                .map_err(|e| JsonataError::new("D3140", format!("$decodeUrl: {e}")))?
                .to_string();
            Ok(Value::String(decoded.into()))
        }
        _ => Err(JsonataError::new(
            "T0410",
            "$decodeUrl: argument must be a string",
        )),
    }
}

pub fn fn_decode_url_component(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() || args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    match &args[0] {
        Value::String(s) => {
            let decoded = percent_encoding::percent_decode_str(s)
                .decode_utf8()
                .map_err(|e| JsonataError::new("D3140", format!("$decodeUrlComponent: {e}")))?
                .to_string();
            Ok(Value::String(decoded.into()))
        }
        _ => Err(JsonataError::new(
            "T0410",
            "$decodeUrlComponent: argument must be a string",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const U: &Value = &Value::Undefined;

    fn s(v: &str) -> Value {
        Value::String(v.into())
    }
    fn n(x: f64) -> Value {
        Value::Number(x)
    }
    fn text(r: JsonataResult) -> String {
        match r {
            Ok(Value::String(v)) => v.to_string(),
            other => panic!("expected string, got {other:?}"),
        }
    }
    fn strings(r: JsonataResult) -> Vec<String> {
        match r {
            Ok(Value::Array(items)) => items
                .iter()
                .map(|v| match v {
                    Value::String(x) => x.to_string(),
                    other => panic!("expected string element, got {other:?}"),
                })
                .collect(),
            other => panic!("expected array, got {other:?}"),
        }
    }
    fn code(r: JsonataResult) -> &'static str {
        match r {
            Err(e) => e.code,
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// $substring operates on characters, not bytes (spec.md §5.2.3).
    #[test]
    fn substring_is_unicode_aware_with_negative_start() {
        assert_eq!(
            text(fn_substring(&[s("hello world"), n(0.0), n(5.0)], U)),
            "hello"
        );
        // Negative start counts from the end, clamped to 0.
        assert_eq!(text(fn_substring(&[s("hello"), n(-2.0)], U)), "lo");
        assert_eq!(text(fn_substring(&[s("hello"), n(-99.0), n(2.0)], U)), "he");
        // Start past the end → empty.
        assert_eq!(text(fn_substring(&[s("hello"), n(9.0)], U)), "");
        // Character-based, not byte-based.
        assert_eq!(text(fn_substring(&[s("héllo"), n(1.0), n(2.0)], U)), "él");
    }

    /// Separator not found → original string unchanged (spec.md §5.2.4).
    #[test]
    fn substring_before_after_split_on_first_separator() {
        assert_eq!(text(fn_substring_before(&[s("a-b-c"), s("-")], U)), "a");
        assert_eq!(text(fn_substring_after(&[s("a-b-c"), s("-")], U)), "b-c");
        assert_eq!(text(fn_substring_before(&[s("abc"), s("|")], U)), "abc");
        assert_eq!(text(fn_substring_after(&[s("abc"), s("|")], U)), "abc");
    }

    /// $trim collapses runs of internal whitespace to a single space.
    #[test]
    fn trim_collapses_internal_whitespace() {
        assert_eq!(text(fn_trim(&[s("  a \t\n b  ")], U)), "a b");
    }

    /// $pad direction follows the width's sign; width counts characters.
    #[test]
    fn pad_pads_by_sign_and_counts_chars() {
        assert_eq!(text(fn_pad(&[s("abc"), n(5.0)], U)), "abc  ");
        assert_eq!(text(fn_pad(&[s("abc"), n(-5.0)], U)), "  abc");
        assert_eq!(text(fn_pad(&[s("abc"), n(5.0), s("-")], U)), "abc--");
        assert_eq!(text(fn_pad(&[s("abc"), n(2.0)], U)), "abc");
        assert_eq!(text(fn_pad(&[s("éé"), n(3.0)], U)), "éé ");
    }

    /// Width beyond ±10,000 → D3010; unguarded it reserved `width` bytes
    /// (1e18 aborted the process via handle_alloc_error).
    #[test]
    fn pad_rejects_width_beyond_cap() {
        assert_eq!(code(fn_pad(&[s("a"), n(10_001.0)], U)), "D3010");
        assert_eq!(code(fn_pad(&[s("a"), n(-10_001.0)], U)), "D3010");
        assert_eq!(code(fn_pad(&[s("a"), n(1e18)], U)), "D3010");
        assert_eq!(code(fn_pad(&[s("a"), n(1e19)], U)), "D3010");
        // The boundary itself is allowed.
        assert_eq!(text(fn_pad(&[s("a"), n(10_000.0)], U)).len(), 10_000);
    }

    #[test]
    fn split_supports_limits_and_char_mode() {
        assert_eq!(strings(fn_split(&[s("a,b,c"), s(",")], U)), ["a", "b", "c"]);
        assert_eq!(
            strings(fn_split(&[s("a,b,c"), s(","), n(2.0)], U)),
            ["a", "b"]
        );
        // Empty separator splits into characters.
        assert_eq!(
            strings(fn_split(&[s("héllo"), s("")], U)),
            ["h", "é", "l", "l", "o"]
        );
        // Negative limit is an error.
        assert_eq!(code(fn_split(&[s("a,b"), s(","), n(-1.0)], U)), "D3020");
    }

    #[test]
    fn join_concatenates_with_separator() {
        let items = Value::Array(Rc::from(vec![s("a"), s("b"), s("c")]));
        assert_eq!(text(fn_join(&[items, s("-")], U)), "a-b-c");
    }

    #[test]
    fn base64_round_trips() {
        assert_eq!(text(fn_base64_encode(&[s("hello")], U)), "aGVsbG8=");
        assert_eq!(text(fn_base64_decode(&[s("aGVsbG8=")], U)), "hello");
    }

    /// $encodeUrl keeps URL structure characters; the component variant
    /// encodes them (ECMAScript encodeURI vs encodeURIComponent).
    #[test]
    fn url_component_encoding_is_stricter_than_url_encoding() {
        assert_eq!(
            text(fn_encode_url(&[s("http://x.com/a b?q=1")], U)),
            "http://x.com/a%20b?q=1"
        );
        assert_eq!(
            text(fn_encode_url_component(&[s("a b&c=d")], U)),
            "a%20b%26c%3Dd"
        );
        assert_eq!(text(fn_decode_url_component(&[s("a%20b%26c")], U)), "a b&c");
    }

    /// $string uses ECMAScript Number.toString() (behavioral invariant #9).
    #[test]
    fn string_formats_numbers_like_ecmascript() {
        assert_eq!(text(fn_string(&[n(100.0)], U)), "100");
        assert_eq!(text(fn_string(&[n(1e21)], U)), "1e+21");
        assert_eq!(text(fn_string(&[Value::Bool(true)], U)), "true");
        assert_eq!(code(fn_string(&[n(f64::INFINITY)], U)), "D3001");
    }

    /// $length counts characters (spec.md §5.2.2).
    #[test]
    fn length_counts_chars() {
        assert!(matches!(fn_length(&[s("héllo")], U), Ok(Value::Number(x)) if x == 5.0));
    }
}
