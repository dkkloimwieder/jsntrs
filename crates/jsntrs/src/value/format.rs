//! Number formatting matching JavaScript's `Number.toString()` behavior.
//!
//! Go equivalent: `FormatFloat` in `eval_helpers.go`, `FormatNumber` in `eval_helpers.go`.
//! Uses `ryu-js` crate for exact ECMAScript formatting.

/// Format an f64 to match Go's `FormatFloat` in `eval_helpers.go`.
///
/// Algorithm (mirrors Go exactly):
/// 1. `s = FormatFloat(n, 'g', 15, 64)` — 15 significant digits
/// 2. If `abs ∉ [5e-7, 1e21)` — scientific with shortest repr, cleaned exponent
/// 3. Else if `s` contains `e`/`E` — `FormatFloat(n, 'f', -1, 64)` full decimal
/// 4. Else — return `s`
///
/// Step 3 splits on integrality — see [`round_to_15_significant`] and the
/// comment at its call site.
///
/// - NaN/Inf → "null"
pub fn format_float(n: f64) -> String {
    if n.is_nan() || n.is_infinite() {
        return "null".into();
    }

    // Negative zero prints unsigned. JavaScript agrees at both layers
    // (`String(-0)` and `JSON.stringify(-0)` are both "0") and so does the
    // JSON side here (`ryu-js`), so the two number-formatting layers must
    // not disagree about the sign of zero (jsntrs-p0v.5). Go's
    // FormatFloat('g') did print "-0"; that is the one place this function
    // does not mirror it.
    if n == 0.0 {
        return "0".into();
    }

    let abs = n.abs();

    // Fast path: integral values below 1e15 print as their plain digits —
    // identical to the 'g'15 pipeline output (≤15 significant digits, exact
    // in f64), without its intermediate strings. Integers dominate the
    // string-concat and $string hot paths.
    if abs < 1e15 && n.fract() == 0.0 {
        return format!("{}", n as i64);
    }

    // Step 1: 15 significant digits (Go's 'g', 15).
    // Rust's format!("{:.14e}") gives 15 sig digits in scientific form.
    let s = format_g15(n);

    // Step 2: very small or very large → scientific with shortest repr
    if abs != 0.0 && !(5e-7..1e21).contains(&abs) {
        // Use ryu-js for shortest representation (like Go's 'e', -1)
        let mut buf = ryu_js::Buffer::new();
        let ryu = buf.format(n).to_owned();
        if ryu.contains('e') || ryu.contains('E') {
            return clean_exponent(&ryu);
        }
        // Fallback: Rust scientific notation
        let sci = format!("{n:e}");
        return clean_exponent(&sci);
    }

    // Step 3: if 'g',15 produced scientific, use full decimal (Go's 'f', -1)
    if s.contains('e') || s.contains('E') {
        // Go's 'f',-1 is the *shortest round-tripping* digits, which keeps
        // everything past the 15th that step 1 had just rounded away. That
        // matches jsonata-js only for integers: its stringify replacer
        // passes those to JSON.stringify untouched
        // (`!Number.isInteger(val)` guards the cast), which is what pins
        // $string(5890840712243076) to its own digits in
        // function-string/case030. A non-integer goes through
        // `Number(val.toPrecision(15))` first, so the extra digits are gone
        // before serialization — $string(1234567890123456.7) is
        // "1234567890123460", not the "1234567890123456.8" this used to
        // print (jsntrs-p0v.24).
        //
        // Only the large end: 'g'15 also goes scientific below 1e-6, and
        // that band is deliberately Go-shaped (see
        // `format_small_band_keeps_full_precision`). A positive exponent
        // here means 15 significant digits reach at least 1e15, so this
        // covers the non-integral doubles from 999999999999999.5 up to
        // 2^52, above which every double is a whole number anyway.
        if n.fract() != 0.0 && s.contains("e+") {
            let mut buf = ryu_js::Buffer::new();
            return buf.format_finite(round_to_15_significant(n)).to_owned();
        }
        // ryu-js may produce scientific notation. We need full decimal.
        // Use ryu-js first; if it's decimal, return it. Otherwise convert.
        let mut buf = ryu_js::Buffer::new();
        let ryu = buf.format(n).to_owned();
        if !ryu.contains('e') && !ryu.contains('E') {
            return ryu;
        }
        // ryu-js gave scientific; manually convert to decimal.
        return scientific_to_decimal(n);
    }

    // Step 4: return the 15-sig-digit result
    s
}

/// Round `n` to 15 significant decimal digits and return the nearest double
/// — ECMAScript `Number(n.toPrecision(15))`, the cast jsonata-js's `$string`
/// applies to a non-integral number before serializing it.
///
/// `{:.14e}` is Rust's correctly-rounded 15-significant-digit form, and
/// parsing it back lands on the same double `Number(…)` would. The two rules
/// disagree only on an exact tie — a value whose exact decimal expansion is
/// 16 significant digits ending in 5 — which Rust breaks to even and
/// ECMAScript breaks away from zero.
///
/// The one caller reaches this with a non-integral double whose 15 digits
/// span 1e15 or more, i.e. from 999999999999999.5 up to 2^52. Above 1e15
/// such a value has at least 17 significant digits (16 in the integer part,
/// plus a fraction that is a multiple of 1/8, 1/4 or 1/2 — the only ulps
/// below 2^52), so it cannot be a tie. Below 1e15 the ulp is 1/8 and the
/// only tie in range is 999999999999999.5 itself, where the digit being
/// kept is a 9: both rules round it up.
fn round_to_15_significant(n: f64) -> f64 {
    format!("{n:.14e}").parse().unwrap_or(n)
}

/// Format a number with 15 significant digits, equivalent to Go's
/// `strconv.FormatFloat(n, 'g', 15, 64)`.
///
/// Uses Rust's scientific formatting with 14 decimal places (= 15 sig digits),
/// then converts to the most compact non-scientific representation.
fn format_g15(n: f64) -> String {
    if n == 0.0 {
        return if n.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }

    // Format in scientific notation with 14 decimal places = 15 significant digits
    let sci = format!("{n:.14e}");
    let (mantissa_str, exp_str) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp_str.parse().unwrap_or(0);

    let negative = mantissa_str.starts_with('-');
    let mant = if negative {
        &mantissa_str[1..]
    } else {
        mantissa_str
    };

    // Remove trailing zeros from mantissa
    let mant_trimmed = mant.trim_end_matches('0').trim_end_matches('.');

    // Extract digits (without decimal point)
    let digits: String = mant_trimmed.replace('.', "");
    let num_digits = digits.len() as i32;

    // Decide format: 'g' uses scientific if exp < -1 or exp >= precision
    // For precision 15: scientific if exp < -1 or exp >= 15
    let use_scientific = !(-1..15).contains(&exp);

    let result = if use_scientific {
        // Scientific notation: d.dddde±dd
        if num_digits <= 1 {
            format!("{}e{:+03}", &digits, exp)
        } else {
            format!("{}.{}e{:+03}", &digits[..1], &digits[1..], exp)
        }
    } else if exp < 0 {
        // Needs leading zeros: 0.000...digits
        let zeros = (-(exp + 1)) as usize + 1;
        let mut r = String::from("0.");
        for _ in 1..zeros {
            r.push('0');
        }
        r.push_str(&digits);
        r
    } else {
        let decimal_pos = (exp + 1) as usize;
        if decimal_pos >= num_digits as usize {
            // All digits before decimal, pad with zeros
            let mut r = digits.clone();
            for _ in 0..(decimal_pos - num_digits as usize) {
                r.push('0');
            }
            r
        } else {
            // Decimal point within digits
            format!("{}.{}", &digits[..decimal_pos], &digits[decimal_pos..])
        }
    };

    if negative {
        format!("-{result}")
    } else {
        result
    }
}

/// Convert a number in the range [5e-7, 1e21) to full decimal representation.
/// Equivalent to Go's `strconv.FormatFloat(n, 'f', -1, 64)` — and so is
/// Rust's `Display`: shortest round-trip digits, positional notation only.
/// (A fixed-precision `{n:.20}` kept only ~14 significant digits for the
/// [5e-7, 1e-6) band, so $string(7/9000000) failed to round-trip.)
fn scientific_to_decimal(n: f64) -> String {
    n.to_string()
}

/// Clean up a scientific notation string: remove leading zeros from exponent,
/// ensure sign is present.
fn clean_exponent(s: &str) -> String {
    let (mantissa, exp) = if let Some(pos) = s.find('e') {
        (&s[..pos], &s[pos + 1..])
    } else if let Some(pos) = s.find('E') {
        (&s[..pos], &s[pos + 1..])
    } else {
        return s.to_owned();
    };

    let (sign, digits) = if exp.starts_with('+') || exp.starts_with('-') {
        (&exp[..1], &exp[1..])
    } else {
        ("+", exp)
    };

    let trimmed = digits.trim_start_matches('0');
    let trimmed = if trimmed.is_empty() { "0" } else { trimmed };

    format!("{mantissa}e{sign}{trimmed}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_integers() {
        assert_eq!(format_float(0.0), "0");
        assert_eq!(format_float(1.0), "1");
        assert_eq!(format_float(42.0), "42");
        assert_eq!(format_float(-1.0), "-1");
    }

    /// The integer fast path must be indistinguishable from the 'g'15
    /// pipeline, including at its boundaries.
    #[test]
    fn format_integer_fast_path_matches_g15_pipeline() {
        assert_eq!(format_float(123_456.0), "123456");
        assert_eq!(format_float(999_999_999_999_999.0), "999999999999999");
        assert_eq!(format_float(-999_999_999_999_999.0), "-999999999999999");
        // 1e15 is just past the fast path; the 'g'15 pipeline goes
        // scientific and step 3 converts back to full decimal — same text
        // the fast path would have produced, via the slow route.
        assert_eq!(format_float(1e15), "1000000000000000");
    }

    /// jsonata-js-verified (2026-08-14): `$string(-0)` is "0", matching
    /// `String(-0)` and the `ryu-js` JSON layer. The pre-jsntrs-p0v.5
    /// output was Go's "-0", which made `$string(0 * -1)` disagree with the
    /// serialized form of the same value.
    #[test]
    fn negative_zero_prints_unsigned() {
        assert_eq!(format_float(-0.0), "0");
        assert_eq!(format_float(0.0), "0");
        // The way `0 * -1` reaches the formatter from an expression.
        assert_eq!(format_float(f64::from(0) * f64::from(-1)), "0");
    }

    #[test]
    fn format_decimals() {
        assert_eq!(format_float(0.5), "0.5");
        assert_eq!(format_float(3.25), "3.25");
    }

    #[test]
    fn format_nan_inf_as_null() {
        assert_eq!(format_float(f64::NAN), "null");
        assert_eq!(format_float(f64::INFINITY), "null");
        assert_eq!(format_float(f64::NEG_INFINITY), "null");
    }

    #[test]
    fn format_scientific_large() {
        // 1e21 and above should use scientific notation
        assert_eq!(format_float(1e21), "1e+21");
        assert_eq!(format_float(1e25), "1e+25");
    }

    /// jsonata-js 2.2.2-verified (2026-08-15, jsntrs-p0v.24): a non-integral
    /// number at or above 1e15 is cast through
    /// `Number(val.toPrecision(15))` before it is serialized, so the digits
    /// past the 15th become zeros. Printing the shortest round-tripping
    /// digits instead — what the `'g'15`-then-`'f',-1` pipeline does — leaked
    /// them: `$string(1234567890123456.7)` came out "1234567890123456.8".
    #[test]
    fn format_rounds_non_integers_at_or_above_1e15() {
        assert_eq!(format_float(1_234_567_890_123_456.7), "1234567890123460");
        assert_eq!(format_float(-1_234_567_890_123_456.7), "-1234567890123460");
        assert_eq!(format_float(1.234_567_890_123_456_7e15), "1234567890123460");
        // Every ulp in the band, written as a sum because the eighths and
        // quarters need 16+ digits to spell out literally: 1/8 below 2^50,
        // then 1/4, then 1/2 up to 2^52. All three additions are exact.
        assert_eq!(format_float(1e15 + 0.125), "1000000000000000");
        assert_eq!(format_float(1e15 + 0.5), "1000000000000000");
        assert_eq!(
            format_float(1_234_567_890_123_456.0 + 0.75),
            "1234567890123460"
        );
        assert_eq!(
            format_float(2_251_799_813_685_248.0 + 0.5),
            "2251799813685250"
        );
        // Just below the band the 'g'15 pipeline already rounded, and
        // 999999999999999.9 rounds up into 16 digits.
        assert_eq!(format_float(999_999_999_999_999.9), "1000000000000000");
    }

    /// jsonata-js 2.2.2-verified (2026-08-15, jsntrs-p0v.24): the same
    /// replacer leaves *integers* alone (`!Number.isInteger(val)` guards the
    /// cast), so they keep their exact ECMAScript digits however long those
    /// run. This is what function-string/case030 pins, and the rounding
    /// branch must not reach them.
    #[test]
    fn format_keeps_large_integers_exact() {
        assert_eq!(format_float(5_890_840_712_243_076.0), "5890840712243076");
        assert_eq!(
            format_float(1.234_567_890_123_456_8e20),
            "123456789012345680000"
        );
        assert_eq!(
            format_float(-1.234_567_890_123_456_8e20),
            "-123456789012345680000"
        );
        // 2^53 and its neighbours: 16 and 17 significant digits, all kept.
        assert_eq!(format_float(9_007_199_254_740_992.0), "9007199254740992");
        assert_eq!(format_float(9_007_199_254_740_994.0), "9007199254740994");
        assert_eq!(format_float(1_000_000_000_000_001.0), "1000000000000001");
        assert_eq!(format_float(1.234_567_890_123_455e16), "12345678901234550");
        // Past 1e21 the exact digits ride the exponential form, and f64::MAX
        // keeps all 17 of them.
        assert_eq!(
            format_float(1.234_567_890_123_456_7e21),
            "1.2345678901234568e+21"
        );
        assert_eq!(format_float(f64::MAX), "1.7976931348623157e+308");
        assert_eq!(format_float(f64::MIN), "-1.7976931348623157e+308");
    }

    #[test]
    fn format_scientific_small() {
        // Below 5e-7 should use scientific notation
        assert_eq!(format_float(1e-7), "1e-7");
        assert_eq!(format_float(5e-8), "5e-8");
    }

    #[test]
    fn format_decimal_range() {
        // Between 5e-7 and 1e21 should use decimal
        assert_eq!(format_float(0.000001), "0.000001");
        assert_eq!(
            format_float(999999999999999900000.0),
            "999999999999999900000"
        );
    }

    /// Go-verified (2026-08-07): the [5e-7, 1e-6) band prints full decimal
    /// with shortest round-trip digits, not 14-sig-digit truncation
    /// (gnata-nuo.4).
    #[test]
    fn format_small_band_keeps_full_precision() {
        assert_eq!(
            format_float(7.0_f64 / 9_000_000.0),
            "0.0000007777777777777778"
        );
        assert_eq!(format_float(0.000_000_5), "0.0000005");
        assert_eq!(
            format_float(0.000_000_999_999_999_999_999_7),
            "0.0000009999999999999997"
        );
        assert_eq!(
            format_float(0.000_001_234_567_890_123_456_7),
            "0.0000012345678901234567"
        );
        // Just below the band stays scientific.
        assert_eq!(format_float(0.000_000_49), "4.9e-7");
    }

    #[test]
    fn format_matches_go_reference() {
        // case001: $string(22/7) — 15 significant digits
        assert_eq!(format_float(22.0_f64 / 7.0), "3.14285714285714");
        // case008: sum results
        assert_eq!(format_float(90.57), "90.57");
        assert_eq!(format_float(245.79), "245.79");
        // case018: 78.8 / 2
        assert_eq!(format_float(39.4), "39.4");
    }
}
