//! `$formatInteger(number, picture)` — XPath 3.1 integer formatting.
//!
//! Port of Go `functions/string_format_integer.go`, function `fnFormatInteger`.

use super::number_words::{
    apply_ordinal_word, int_to_words, is_unicode_digit, ordinal_suffix, split_picture_modifier,
    to_alphabetic, to_roman, unicode_digit_zero,
};
use crate::error::{JsonataError, JsonataResult};
use crate::value::Value;

pub fn fn_format_integer(args: &[Value], _focus: &Value) -> JsonataResult {
    // Largest magnitude admitted for i64 formatting: 2^63 - 1024, the
    // biggest f64 below 2^63 (Go uses the same margin). This also rejects
    // i64::MIN itself — format_integer_with_picture takes unsigned_abs()
    // as i64, which wraps on -2^63 and would recurse without terminating.
    const MAX_I64_F64: f64 = 9_223_372_036_854_774_784.0; // 2^63 - 1024

    if args.len() < 2 {
        return Err(JsonataError::new(
            "T0410",
            "$formatInteger: requires 2 arguments",
        ));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let n = args[0]
        .as_f64()
        .ok_or_else(|| JsonataError::new("T0410", "$formatInteger: argument 1 must be a number"))?;
    let picture: &str = match &args[1] {
        Value::String(s) => s,
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$formatInteger: argument 2 must be a string",
            ));
        }
    };

    // Inf/NaN must never reach the formatters: every non-finite magnitude
    // falls outside the i64 window below, and the words pictures route
    // out-of-range values to `float_to_words`, whose 1e12 normalisation
    // loop cannot terminate for infinity (jsntrs-ecq.3). NaN merely
    // formatted as "zero". JSON input carries no Inf/NaN, but `1/0`,
    // `evaluate_value` and custom-function results all do. Same guard the
    // other string-producing builtin uses ($string, string_funcs.rs).
    if !n.is_finite() {
        return Err(JsonataError::new(
            "D3001",
            "$formatInteger: Number out of range",
        ));
    }

    let truncated = n.trunc();

    if !(-MAX_I64_F64..=MAX_I64_F64).contains(&truncated) {
        let (format_token, modifier) = split_picture_modifier(picture);
        if format_token == "w" || format_token == "W" || format_token == "Ww" {
            return Ok(Value::String(
                format_big_float_words(truncated, format_token, modifier).into(),
            ));
        }
        return Err(JsonataError::new(
            "D3137",
            "$formatInteger: number too large for integer formatting",
        ));
    }

    let result = format_integer_with_picture(truncated as i64, picture)?;
    Ok(Value::String(result.into()))
}

fn format_integer_with_picture(n: i64, picture: &str) -> Result<String, JsonataError> {
    let (format_token, modifier) = split_picture_modifier(picture);
    let negative = n < 0;
    let abs_n = n.unsigned_abs() as i64; // safe: we checked range in caller

    let mut result = match format_token {
        "w" => {
            let words = int_to_words(abs_n);
            if modifier == "o" {
                apply_ordinal_word(&words)
            } else {
                words
            }
        }
        "W" => {
            if modifier == "o" {
                apply_ordinal_word(&int_to_words(abs_n)).to_uppercase()
            } else {
                int_to_words(abs_n).to_uppercase()
            }
        }
        "Ww" => {
            if modifier == "o" {
                to_title_case(&apply_ordinal_word(&int_to_words(abs_n)))
            } else {
                to_title_case(&int_to_words(abs_n))
            }
        }
        "i" => to_roman(abs_n, false),
        "I" => to_roman(abs_n, true),
        _ => {
            let chars: Vec<char> = format_token.chars().collect();
            if chars.len() == 1 {
                let ch = chars[0];
                if ch.is_ascii_lowercase() {
                    let mut r = to_alphabetic(abs_n, 'a');
                    if negative {
                        r.insert(0, '-');
                    }
                    return Ok(r);
                }
                if ch.is_ascii_uppercase() && ch != 'W' && ch != 'I' {
                    let mut r = to_alphabetic(abs_n, 'A');
                    if negative {
                        r.insert(0, '-');
                    }
                    return Ok(r);
                }
            }
            // Must contain at least one digit placeholder
            if !chars
                .iter()
                .any(|&c| c == '#' || c.is_ascii_digit() || is_unicode_digit(c))
            {
                return Err(JsonataError::new(
                    "D3130",
                    format!("$formatInteger: unsupported picture string {format_token:?}"),
                ));
            }
            let mut r = format_integer_decimal(abs_n, format_token)?;
            if modifier == "o" {
                r.push_str(ordinal_suffix(abs_n));
            }
            if negative {
                r.insert(0, '-');
            }
            return Ok(r);
        }
    };

    if negative {
        result.insert(0, '-');
    }
    Ok(result)
}

// ── Decimal formatting ───────────────────────────────────────────────────────

fn format_integer_decimal(n: i64, picture: &str) -> Result<String, JsonataError> {
    let runes: Vec<char> = picture.chars().collect();

    // Determine digit family (zero rune)
    let mut zero_rune = '0';
    let mut found_family = false;
    for &c in &runes {
        if c.is_ascii_digit() {
            if found_family && zero_rune != '0' {
                return Err(JsonataError::new(
                    "D3131",
                    "$formatInteger: mixed digit families in picture",
                ));
            }
            zero_rune = '0';
            found_family = true;
            continue;
        }
        if let Some(z) = unicode_digit_zero(c) {
            if found_family && zero_rune != z {
                return Err(JsonataError::new(
                    "D3131",
                    "$formatInteger: mixed digit families in picture",
                ));
            }
            if found_family && zero_rune == '0' {
                return Err(JsonataError::new(
                    "D3131",
                    "$formatInteger: mixed digit families in picture",
                ));
            }
            zero_rune = z;
            found_family = true;
        }
    }

    // Count mandatory and total digit positions
    let mut mandatory_count = 0usize;
    let mut total_digits = 0usize;
    for &c in &runes {
        if c == '#' {
            total_digits += 1;
        } else if c.is_ascii_digit() || is_unicode_digit(c) {
            mandatory_count += 1;
            total_digits += 1;
        }
    }
    if total_digits == 0 {
        return Err(JsonataError::new(
            "D3131",
            "$formatInteger: no digit placeholders in picture",
        ));
    }

    // Collect grouping info (separator char + position from right)
    let mut grp_infos: Vec<(char, usize)> = Vec::new();
    let mut digit_from_right = 0usize;
    for &c in runes.iter().rev() {
        if c == '#' || c.is_ascii_digit() || is_unicode_digit(c) {
            digit_from_right += 1;
        } else if digit_from_right > 0 {
            grp_infos.push((c, digit_from_right));
        }
    }

    // Build digit string with zero-padding
    let mut digits = format!("{n}");
    while digits.len() < mandatory_count {
        digits.insert(0, '0');
    }

    // Apply grouping
    if !grp_infos.is_empty() && !digits.is_empty() {
        digits = apply_integer_grouping(&digits, &grp_infos);
    }

    // Apply digit family translation
    if zero_rune != '0' {
        digits = apply_digit_family_rune(&digits, zero_rune);
    }

    Ok(digits)
}

/// Apply grouping separators to a digit string.
/// `grps` is a list of (separator_char, position_from_right).
fn apply_integer_grouping(digits: &str, grps: &[(char, usize)]) -> String {
    if grps.is_empty() {
        return digits.to_string();
    }

    let mut pos_set = std::collections::HashMap::new();
    for &(sep, pos_r) in grps {
        pos_set.insert(pos_r, sep);
    }

    let all_same_sep = grps.iter().all(|g| g.0 == grps[0].0);

    let is_regular = if grps.len() > 1 {
        let gap0 = grps[0].1;
        grps.windows(2).all(|w| w[1].1 - w[0].1 == gap0)
    } else {
        true
    };

    if all_same_sep && is_regular {
        let interval = grps[0].1;
        let rightmost_sep = grps[0].0;
        let max_len = digits.len() + 1;
        let mut pos = interval;
        while pos <= max_len {
            pos_set.entry(pos).or_insert(rightmost_sep);
            pos += interval;
        }
    }

    let runes: Vec<char> = digits.chars().collect();
    let mut result = Vec::new();
    for (i, &ch) in runes.iter().enumerate() {
        let pos_from_right = runes.len() - i;
        if i > 0
            && let Some(&sep) = pos_set.get(&pos_from_right)
        {
            result.push(sep);
        }
        result.push(ch);
    }
    result.into_iter().collect()
}

fn apply_digit_family_rune(s: &str, zero: char) -> String {
    let mut result = String::with_capacity(s.len() * 4);
    for c in s.chars() {
        if c.is_ascii_digit() {
            result.push(char::from_u32(zero as u32 + c as u32 - '0' as u32).unwrap_or(c));
        } else {
            result.push(c);
        }
    }
    result
}

// ── Float to words (for very large numbers) ──────────────────────────────────

/// Words for magnitudes past the i64 window, in units of trillions.
///
/// The caller must have rejected Inf/NaN: the normalisation loop below
/// divides until the value drops under 1e12, which never happens for
/// infinity (jsntrs-ecq.3).
fn float_to_words(f: f64) -> String {
    debug_assert!(f.is_finite(), "float_to_words requires a finite magnitude");
    if f == 0.0 {
        return "zero".to_string();
    }
    let mut f = f;
    let trillion: f64 = 1e12;
    let mut trillion_count = 0usize;
    while f >= trillion {
        f /= trillion;
        trillion_count += 1;
    }
    let mut result = int_to_words(f.round() as i64);
    for _ in 0..trillion_count {
        result.push_str(" trillion");
    }
    result
}

fn format_big_float_words(f: f64, format_token: &str, modifier: &str) -> String {
    let negative = f < 0.0;
    let f = f.abs();
    let mut words = match format_token {
        "w" => {
            if modifier == "o" {
                apply_ordinal_word(&float_to_words(f))
            } else {
                float_to_words(f)
            }
        }
        "W" => {
            if modifier == "o" {
                apply_ordinal_word(&float_to_words(f)).to_uppercase()
            } else {
                float_to_words(f).to_uppercase()
            }
        }
        "Ww" => {
            if modifier == "o" {
                to_title_case(&apply_ordinal_word(&float_to_words(f)))
            } else {
                to_title_case(&float_to_words(f))
            }
        }
        _ => float_to_words(f),
    };
    if negative {
        words.insert_str(0, "minus ");
    }
    words
}

// ── Title case ───────────────────────────────────────────────────────────────

fn to_title_case(s: &str) -> String {
    let lowercase: &[&str] = &["and", "or", "of", "the"];
    let mut result = String::with_capacity(s.len());
    let mut capitalize_next = true;
    let mut word_buf = String::new();

    let flush = |word_buf: &mut String, result: &mut String, capitalize_next: &mut bool| {
        if word_buf.is_empty() {
            return;
        }
        let lower = word_buf.to_lowercase();
        if *capitalize_next || !lowercase.contains(&lower.as_str()) {
            if !lower.is_empty() {
                let mut chars = lower.chars();
                if let Some(first) = chars.next() {
                    result.extend(first.to_uppercase());
                    result.push_str(chars.as_str());
                }
            }
        } else {
            result.push_str(&lower);
        }
        *capitalize_next = false;
        word_buf.clear();
    };

    for c in s.chars() {
        if c == ' ' || c == ',' || c == '-' {
            flush(&mut word_buf, &mut result, &mut capitalize_next);
            result.push(c);
            capitalize_next = c == '-';
        } else {
            word_buf.push(c);
        }
    }
    flush(&mut word_buf, &mut result, &mut capitalize_next);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(n: f64, picture: &str) -> String {
        match fn_format_integer(
            &[Value::Number(n), Value::String(picture.into())],
            &Value::Undefined,
        ) {
            Ok(Value::String(s)) => s.to_string(),
            other => panic!("expected string, got {other:?}"),
        }
    }

    /// 'w' and roman-numeral expectations come from the JSONata
    /// documentation examples for $formatInteger.
    #[test]
    fn format_integer_pictures() {
        assert_eq!(
            fmt(2789.0, "w"),
            "two thousand, seven hundred and eighty-nine"
        );
        assert_eq!(fmt(1999.0, "I"), "MCMXCIX");
        assert_eq!(fmt(1999.0, "i"), "mcmxcix");
        assert_eq!(fmt(123.0, "0000"), "0123");
    }

    /// The ';o' modifier produces ordinals (spec.md §5.2.22).
    #[test]
    fn ordinal_modifier() {
        assert_eq!(fmt(12.0, "w;o"), "twelfth");
        assert_eq!(fmt(12.0, "1;o"), "12th");
        assert_eq!(fmt(2.0, "1;o"), "2nd");
    }

    /// i64::MIN (-2^63) must be rejected, not wrapped through unsigned_abs
    /// (which previously recursed without terminating in int_to_words).
    #[test]
    fn i64_min_errors_cleanly() {
        let min = i64::MIN as f64; // exactly -2^63
        for picture in ["0", "i", "a"] {
            let err = match fn_format_integer(
                &[Value::Number(min), Value::String((*picture).into())],
                &Value::Undefined,
            ) {
                Err(e) => e,
                other => panic!("expected D3137 for {picture:?}, got {other:?}"),
            };
            assert_eq!(err.code, "D3137");
        }
        // Words pictures route out-of-range magnitudes to the big-float
        // words formatter instead of erroring.
        let words = fmt(min, "w");
        assert!(words.starts_with("minus "), "got {words:?}");
        // The margin bound itself still formats normally.
        assert!(fmt(-9_223_372_036_854_774_784.0, "0").starts_with('-'));
    }

    /// Non-finite input is rejected at entry for *every* picture family
    /// (jsntrs-ecq.3). Before the guard, `+/-Inf` with a words picture
    /// spun forever in `float_to_words` (the `f /= 1e12` loop never drops
    /// infinity below the threshold) and NaN silently formatted as
    /// "zero"; the other families reported D3137 "too large", which is
    /// the wrong diagnosis for Inf/NaN.
    #[test]
    fn non_finite_errors_for_every_picture() {
        let pictures = [
            "w", "W", "Ww", "w;o", "W;o", "Ww;o", "i", "I", "a", "A", "0", "#,##0", "1;o",
        ];
        for picture in pictures {
            for n in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
                let err = match fn_format_integer(
                    &[Value::Number(n), Value::String(picture.into())],
                    &Value::Undefined,
                ) {
                    Err(e) => e,
                    other => panic!("expected D3001 for {picture:?} / {n}, got {other:?}"),
                };
                assert_eq!(err.code, "D3001", "picture {picture:?}, input {n}");
            }
        }
    }

    /// The same guard through the public API: `evaluate_value` accepts a
    /// `Value` built in Rust, so Inf/NaN reach the builtin without going
    /// through JSON (which cannot represent them). `1/0` and `0/0` inside
    /// an expression are the other route — division deliberately lets
    /// non-finite results propagate.
    #[test]
    fn non_finite_errors_through_evaluate_value() {
        for picture in ["w", "W;o", "i", "0"] {
            let src = format!("$formatInteger($, '{picture}')");
            let expr = crate::expression::Expression::compile(&src).unwrap();
            for n in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
                let err = expr.evaluate_value(&Value::Number(n)).unwrap_err();
                assert_eq!(err.code, "D3001", "picture {picture:?}, input {n}");
            }

            let from_div = crate::expression::Expression::compile(&format!(
                "$formatInteger(1/0, '{picture}')"
            ))
            .unwrap();
            assert_eq!(
                from_div.evaluate_value(&Value::Undefined).unwrap_err().code,
                "D3001",
                "1/0 with picture {picture:?}"
            );
        }
    }
}
