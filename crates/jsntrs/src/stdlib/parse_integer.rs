//! `$parseInteger(string, picture)` — parse a formatted integer string back to a number.
//!
//! Port of Go `functions/string_format_integer.go` `fnParseInteger`.

use super::number_words::{
    ordinal_pairs, roman_value, split_picture_modifier, unicode_digit_zero, word_value,
};
use crate::error::{JsonataError, JsonataResult};
use crate::value::Value;

pub fn fn_parse_integer(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.len() < 2 {
        return Err(JsonataError::new(
            "T0410",
            "$parseInteger: requires 2 arguments",
        ));
    }

    if matches!(args[0], Value::Undefined) {
        return Ok(Value::Undefined);
    }

    let s: &str = match &args[0] {
        Value::String(s) => s,
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$parseInteger: argument 1 must be a string",
            ));
        }
    };

    let picture: &str = match &args[1] {
        Value::String(s) => s,
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$parseInteger: argument 2 must be a string",
            ));
        }
    };

    parse_integer_with_picture(s, picture).map(Value::Number)
}

/// The parsed value, as the `f64` `$parseInteger` answers with.
///
/// The three integer paths (Roman, alphabetic, decimal digits) are computed
/// in `i64` and widened here; the word path is `f64` throughout, because
/// "trillion trillion" is 1e24 and no integer type holds it. Until this
/// commit that overflow was signalled by *raising* an error whose code was
/// the invented string `"D3137_FLOAT"` and whose message was the float,
/// caught and re-parsed one frame up. `D3137_FLOAT` is not a code any
/// catalog defines — the JSONata documentation publishes no error-code page
/// at all (see `docs/behaviors.md` § 2.0), so a caller had nothing to match
/// it against — and one escaping through any path but the single `if` that
/// re-parses it would have been unspellable.
fn parse_integer_with_picture(s: &str, picture: &str) -> Result<f64, JsonataError> {
    let (format_token, _modifier) = split_picture_modifier(picture);

    match format_token {
        "w" | "W" | "Ww" => return words_to_float(&s.to_lowercase()),
        "i" | "I" => return from_roman(&s.to_uppercase()).map(|n| n as f64),
        _ => {
            let chars: Vec<char> = format_token.chars().collect();
            if chars.len() == 1 {
                let ch = chars[0];
                if ch.is_ascii_alphabetic() {
                    return from_alphabetic(&s.to_lowercase()).map(|n| n as f64);
                }
            }
        }
    }

    // Decimal digit pattern
    let mut zero_rune = '0';
    let mut has_mandatory = false;
    for c in picture.chars() {
        if c.is_ascii_digit() {
            zero_rune = '0';
            has_mandatory = true;
        } else if let Some(z) = unicode_digit_zero(c) {
            zero_rune = z;
            has_mandatory = true;
        }
    }

    if !has_mandatory {
        return Err(JsonataError::new(
            "D3130",
            "$parseInteger: picture string must contain at least one mandatory digit placeholder",
        ));
    }

    let mut digits = String::new();
    for c in s.chars() {
        if c == '-' || c.is_ascii_digit() {
            digits.push(c);
        } else if zero_rune != '0' {
            let z_u32 = zero_rune as u32;
            let c_u32 = c as u32;
            if c_u32 >= z_u32 && c_u32 <= z_u32 + 9 {
                // Map to ASCII digit
                if let Some(ascii) = char::from_u32('0' as u32 + (c_u32 - z_u32)) {
                    digits.push(ascii);
                }
            }
        }
    }

    let cleaned = digits.trim();
    cleaned.parse::<i64>().map(|n| n as f64).map_err(|_| {
        JsonataError::new(
            "D3137",
            format!("$parseInteger: cannot parse {s:?} as integer"),
        )
    })
}

// ── De-ordinalise ────────────────────────────────────────────────────────────

/// Strip an ordinal ending off the last word: "twenty-first" -> "twenty-one".
///
/// Only the *irregular* ordinals are listed, in
/// [`number_words::ordinal_pairs`]; a word not in it is left alone, which is
/// what makes "twenty-fifth" work (the "twenty" prefix is untouched and only
/// "fifth" is rewritten).
fn de_ordinalise(s: &str) -> String {
    // Find last separator (space or hyphen)
    let bytes = s.as_bytes();
    let mut last_idx: Option<usize> = None;
    let mut last_sep: u8 = b' ';
    for i in (0..bytes.len()).rev() {
        if bytes[i] == b' ' || bytes[i] == b'-' {
            last_idx = Some(i);
            last_sep = bytes[i];
            break;
        }
    }

    let (prefix, last_word) = if let Some(idx) = last_idx {
        (&s[..idx], &s[idx + 1..])
    } else {
        ("", s)
    };

    for (cardinal, ordinal) in ordinal_pairs() {
        if last_word == ordinal {
            if last_idx.is_some() {
                return format!("{}{}{}", prefix, last_sep as char, cardinal);
            }
            return cardinal.to_string();
        }
    }

    s.to_string()
}

// ── Words to number ──────────────────────────────────────────────────────────

fn words_to_float(s: &str) -> Result<f64, JsonataError> {
    let s = de_ordinalise(s);

    let s = s.replace(['-', ','], " ");
    let words: Vec<&str> = s.split_whitespace().collect();

    let mut total: f64 = 0.0;
    let mut current: f64 = 0.0;

    for w in &words {
        if *w == "and" {
            continue;
        }
        let Some(val) = word_value(w) else {
            return Err(JsonataError::new(
                "D3137",
                format!("$parseInteger: unknown word {w:?}"),
            ));
        };

        if val == 100.0 {
            if current == 0.0 {
                current = 1.0;
            }
            current *= 100.0;
        } else if val >= 1000.0 {
            if current == 0.0 {
                if total == 0.0 {
                    total = 1.0;
                }
                total *= val;
            } else {
                total += current * val;
                current = 0.0;
            }
        } else {
            current += val;
        }
    }

    Ok(total + current)
}

// ── Roman numerals ───────────────────────────────────────────────────────────

fn from_roman(s: &str) -> Result<i64, JsonataError> {
    let chars: Vec<char> = s.chars().collect();
    let mut total: i64 = 0;

    for (i, &c) in chars.iter().enumerate() {
        let v = roman_value(c).ok_or_else(|| {
            JsonataError::new(
                "D3137",
                format!("$parseInteger: invalid Roman numeral {c:?}"),
            )
        })?;

        if i + 1 < chars.len()
            && let Some(next) = roman_value(chars[i + 1])
            && next > v
        {
            total -= v;
            continue;
        }
        total += v;
    }

    Ok(total)
}

// ── Alphabetic (spreadsheet column) ──────────────────────────────────────────

fn from_alphabetic(s: &str) -> Result<i64, JsonataError> {
    let mut result: i64 = 0;
    for c in s.chars() {
        if !c.is_ascii_lowercase() {
            return Err(JsonataError::new(
                "D3137",
                format!("$parseInteger: invalid alphabetic character {c:?}"),
            ));
        }
        // Go wraps silently past i64; a clean range error beats garbage.
        result = result
            .checked_mul(26)
            .and_then(|r| r.checked_add(c as i64 - 'a' as i64 + 1))
            .ok_or_else(|| {
                JsonataError::new("D3137", "$parseInteger: alphabetic value out of range")
            })?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str, picture: &str) -> f64 {
        match fn_parse_integer(
            &[Value::String(s.into()), Value::String(picture.into())],
            &Value::Undefined,
        ) {
            Ok(Value::Number(n)) => n,
            other => panic!("expected number, got {other:?}"),
        }
    }

    /// $parseInteger inverts $formatInteger for the same picture
    /// ("12,345,678" example from the JSONata documentation).
    #[test]
    fn parse_integer_inverts_format_integer() {
        assert_eq!(parse("twelve", "w"), 12.0);
        assert_eq!(parse("MCMXCIX", "I"), 1999.0);
        assert_eq!(parse("mcmxcix", "i"), 1999.0);
        assert_eq!(parse("12,345,678", "#,##0"), 12_345_678.0);
        assert_eq!(parse("0123", "0000"), 123.0);
    }

    /// Word values are accumulated as `f64` and answered as they stand, so
    /// magnitudes no integer type holds survive intact (Go-verified:
    /// "trillion trillion" -> 1e24, "nine hundred trillion" -> 9e14).
    #[test]
    fn words_range_guard_boundary() {
        assert_eq!(parse("nine hundred trillion", "w"), 9e14);
        assert_eq!(parse("trillion trillion", "w"), 1e24);
    }

    /// Alphabetic overflow errors cleanly. Deliberate Go divergence:
    /// Go wraps int64 silently ("zzzzzzzzzzzzzz" -> -6.69e18 garbage).
    #[test]
    fn alphabetic_overflow_is_clean_error() {
        assert_eq!(parse("zz", "a"), 702.0); // Go-verified
        let err = fn_parse_integer(
            &[
                Value::String("zzzzzzzzzzzzzz".into()),
                Value::String("a".into()),
            ],
            &Value::Undefined,
        )
        .unwrap_err();
        assert_eq!(err.code, "D3137");
    }
}
