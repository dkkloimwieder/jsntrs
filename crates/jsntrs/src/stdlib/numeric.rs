//! Numeric functions: $number, $abs, $floor, $ceil, $round, $power, $sqrt, $random,
//! $sum, $max, $min, $average, $formatBase.

use crate::error::{JsonataError, JsonataResult};
use crate::value::Value;

use super::context_arg_code;

pub fn fn_number(args: &[Value], focus: &Value) -> JsonataResult {
    let from_focus = args.is_empty();
    let arg = match args.len() {
        0 => focus,
        1 => &args[0],
        _ => return Err(JsonataError::new("T0410", "$number: too many arguments")),
    };
    if arg.is_undefined() {
        return Ok(Value::Undefined);
    }
    // The reference parameter is `(nsb)-`, so a context value outside
    // number/string/boolean never reaches the cast: it is T0411 (jsntrs-p0v.18).
    let code = context_arg_code(from_focus);
    match arg {
        Value::Null => Err(JsonataError::new(
            code,
            "$number: cannot cast null to number",
        )),
        Value::Number(n) => Ok(Value::Number(*n)),
        Value::Bool(b) => Ok(Value::Number(if *b { 1.0 } else { 0.0 })),
        Value::String(s) => {
            let s = s.trim();
            // Support hex/binary/octal prefixes.
            if s.len() >= 2 && s.starts_with('0') {
                let prefix = s.as_bytes()[1];
                let (radix, offset) = match prefix {
                    b'x' | b'X' => (16, 2),
                    b'b' | b'B' => (2, 2),
                    b'o' | b'O' => (8, 2),
                    _ => (0, 0),
                };
                if radix > 0 {
                    return match i64::from_str_radix(&s[offset..], radix) {
                        Ok(n) => Ok(Value::Number(n as f64)),
                        Err(_) => Err(JsonataError::new(
                            "D3030",
                            format!("$number: unable to cast \"{s}\" to a number"),
                        )),
                    };
                }
            }
            match s.parse::<f64>() {
                Ok(f) if f.is_finite() => Ok(Value::Number(f)),
                _ => Err(JsonataError::new(
                    "D3030",
                    format!("$number: unable to cast \"{s}\" to a number"),
                )),
            }
        }
        Value::Array(_) => Err(JsonataError::new(
            code,
            "$number: cannot cast array to number",
        )),
        Value::Object(_) => Err(JsonataError::new(
            code,
            "$number: cannot cast object to number",
        )),
        _ => Err(JsonataError::new(code, "$number: unsupported type")),
    }
}

fn require_number(args: &[Value], name: &str) -> Result<Option<f64>, JsonataError> {
    if args.is_empty() {
        return Err(JsonataError::new(
            "T0410",
            format!("{name}: argument is required"),
        ));
    }
    if args[0].is_undefined() {
        return Ok(None);
    }
    match args[0].as_f64() {
        Some(n) => Ok(Some(n)),
        None => Err(JsonataError::new(
            "T0410",
            format!("{name}: argument must be a number"),
        )),
    }
}

fn to_number_array(v: &Value) -> Option<Vec<f64>> {
    match v {
        Value::Number(n) => Some(vec![*n]),
        Value::Array(arr) => {
            let mut nums = Vec::with_capacity(arr.len());
            for item in arr.iter() {
                match item.as_f64() {
                    Some(n) => nums.push(n),
                    None => return None,
                }
            }
            Some(nums)
        }
        Value::Sequence(seq) => to_number_array(&seq.collapse()),
        _ => None,
    }
}

pub fn fn_abs(args: &[Value], _focus: &Value) -> JsonataResult {
    match require_number(args, "$abs")? {
        Some(n) => Ok(Value::Number(n.abs())),
        None => Ok(Value::Undefined),
    }
}

pub fn fn_floor(args: &[Value], _focus: &Value) -> JsonataResult {
    match require_number(args, "$floor")? {
        Some(n) => Ok(Value::Number(n.floor())),
        None => Ok(Value::Undefined),
    }
}

pub fn fn_ceil(args: &[Value], _focus: &Value) -> JsonataResult {
    match require_number(args, "$ceil")? {
        Some(n) => Ok(Value::Number(n.ceil())),
        None => Ok(Value::Undefined),
    }
}

pub fn fn_round(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$round: argument is required"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let n = args[0]
        .as_f64()
        .ok_or_else(|| JsonataError::new("T0410", "$round: argument must be a number"))?;
    let scale = if args.len() >= 2 && !args[1].is_undefined() {
        args[1]
            .as_f64()
            .ok_or_else(|| JsonataError::new("T0410", "$round: scale must be a number"))?
            as i32
    } else {
        0
    };
    Ok(Value::Number(bankers_round(n, scale)))
}

pub(crate) fn bankers_round(n: f64, scale: i32) -> f64 {
    if scale >= 0 {
        return bankers_round_decimal(n, scale.unsigned_abs() as usize);
    }
    // Negative scale: round to the nearest 10^|scale|. Mirrors Go
    // (math.Pow-based) — extreme scales underflow the same way there
    // and in jsonata-js (Math.pow(10, -400) is 0, giving NaN).
    let mult = 10f64.powf(-f64::from(scale));
    let scaled = n / mult;
    bankers_round_decimal(scaled, 0) * mult
}

/// Round `n` to `places` decimal digits, half to even, working from the
/// shortest decimal representation instead of scaling by a power of ten:
/// scaling has IEEE 754 artifacts (4.525*100 is 452.50000000000006 and
/// 0.5655*1000 is 565.4999999999999) and overflows for large scales
/// ($round(1, 400) must be 1, not NaN). Port of Go's bankersRoundDecimal;
/// Rust's `Display` matches Go's FormatFloat(n, 'f', -1, 64) — shortest
/// round-trip digits, positional notation only.
fn bankers_round_decimal(n: f64, places: usize) -> f64 {
    if !n.is_finite() {
        return n;
    }
    let negative = n < 0.0;
    let n_abs = n.abs();
    let mut s = format!("{n_abs}");
    let dot = if let Some(i) = s.find('.') {
        i
    } else {
        s.push('.');
        s.len() - 1
    };
    let decimals = &s[dot + 1..];
    // No digit at the rounding position — value is already at or below
    // the requested precision.
    if places >= decimals.len() {
        return if negative { -n_abs } else { n_abs };
    }
    let round_digit = s.as_bytes()[dot + 1 + places] - b'0';

    let truncated = if places == 0 {
        &s[..dot]
    } else {
        &s[..dot + 1 + places]
    };
    let mut base: f64 = truncated.parse().unwrap_or(0.0);
    let step = 10f64.powf(-(places as f64));

    match round_digit.cmp(&5) {
        std::cmp::Ordering::Less => {}
        std::cmp::Ordering::Greater => base += step,
        std::cmp::Ordering::Equal => {
            // A non-zero digit after the 5 means this is not a tie.
            let has_remainder = s.as_bytes()[dot + 2 + places..].iter().any(|&b| b != b'0');
            if has_remainder {
                base += step;
            } else {
                // Exact tie: round to even on the last kept digit.
                let last_digit = if places == 0 {
                    if dot > 0 {
                        s.as_bytes()[dot - 1] - b'0'
                    } else {
                        0
                    }
                } else {
                    s.as_bytes()[dot + places] - b'0'
                };
                if last_digit % 2 != 0 {
                    base += step;
                }
            }
        }
    }

    // Re-format at the target precision to strip float trailing error
    // from the `+ step` additions (e.g. 12.000000000000002 → 12).
    let result: f64 = format!("{base:.places$}").parse().unwrap_or(base);
    if negative { -result } else { result }
}

pub fn fn_power(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.len() < 2 {
        return Err(JsonataError::new("T0410", "$power: requires 2 arguments"));
    }
    if args[0].is_undefined() || args[1].is_undefined() {
        return Ok(Value::Undefined);
    }
    let base = args[0]
        .as_f64()
        .ok_or_else(|| JsonataError::new("T0410", "$power: arguments must be numbers"))?;
    let exp = args[1]
        .as_f64()
        .ok_or_else(|| JsonataError::new("T0410", "$power: arguments must be numbers"))?;
    let result = base.powf(exp);
    if !result.is_finite() {
        return Err(JsonataError::new("D3061", "$power: result is non-finite"));
    }
    Ok(Value::Number(result))
}

pub fn fn_sqrt(args: &[Value], _focus: &Value) -> JsonataResult {
    match require_number(args, "$sqrt")? {
        Some(n) if n < 0.0 => Err(JsonataError::new(
            "D3060",
            "$sqrt: square root of a negative number",
        )),
        Some(n) => Ok(Value::Number(n.sqrt())),
        None => Ok(Value::Undefined),
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "must match the BuiltinFn signature"
)]
pub fn fn_random(_args: &[Value], _focus: &Value) -> JsonataResult {
    Ok(Value::Number(fastrand::f64()))
}

pub fn fn_sum(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$sum: argument 1 is required"));
    }
    // Arity enforced by SignedBuiltin signature at call site.
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let nums = to_number_array(&args[0])
        .ok_or_else(|| JsonataError::new("T0412", "$sum: argument must be an array of numbers"))?;
    Ok(Value::Number(nums.iter().sum()))
}

pub fn fn_max(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$max: argument 1 is required"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let nums = to_number_array(&args[0])
        .ok_or_else(|| JsonataError::new("T0412", "$max: argument must be an array of numbers"))?;
    if nums.is_empty() {
        return Ok(Value::Undefined);
    }
    Ok(Value::Number(
        nums.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    ))
}

pub fn fn_min(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$min: argument 1 is required"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let nums = to_number_array(&args[0])
        .ok_or_else(|| JsonataError::new("T0412", "$min: argument must be an array of numbers"))?;
    if nums.is_empty() {
        return Ok(Value::Undefined);
    }
    Ok(Value::Number(
        nums.iter().copied().fold(f64::INFINITY, f64::min),
    ))
}

pub fn fn_average(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new(
            "T0410",
            "$average: argument 1 is required",
        ));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let nums = to_number_array(&args[0]).ok_or_else(|| {
        JsonataError::new("T0412", "$average: argument must be an array of numbers")
    })?;
    if nums.is_empty() {
        return Ok(Value::Undefined);
    }
    let sum: f64 = nums.iter().sum();
    Ok(Value::Number(sum / nums.len() as f64))
}

pub fn fn_format_base(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new(
            "T0410",
            "$formatBase: requires at least 1 argument",
        ));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let n = args[0].as_f64().ok_or_else(|| {
        JsonataError::new("T0410", "$formatBase: first argument must be a number")
    })?;
    let radix = if args.len() >= 2 && !args[1].is_undefined() {
        args[1].as_f64().ok_or_else(|| {
            JsonataError::new("T0410", "$formatBase: second argument must be a number")
        })? as u32
    } else {
        10 // default base 10
    };
    if !(2..=36).contains(&radix) {
        return Err(JsonataError::new(
            "D3100",
            "$formatBase: radix must be between 2 and 36",
        ));
    }
    let int_val = n.round() as i64;
    let formatted = format_radix(int_val.unsigned_abs(), radix);
    if int_val < 0 {
        Ok(Value::String(format!("-{formatted}").into()))
    } else {
        Ok(Value::String(formatted.into()))
    }
}

fn format_radix(mut n: u64, radix: u32) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let mut digits = Vec::new();
    while n > 0 {
        let d = (n % u64::from(radix)) as u32;
        digits.push(char::from_digit(d, radix).unwrap_or('?'));
        n /= u64::from(radix);
    }
    digits.reverse();
    digits.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    const U: &Value = &Value::Undefined;

    fn n(x: f64) -> Value {
        Value::Number(x)
    }
    fn num(r: JsonataResult) -> f64 {
        match r {
            Ok(Value::Number(x)) => x,
            other => panic!("expected number, got {other:?}"),
        }
    }
    fn text(r: JsonataResult) -> String {
        match r {
            Ok(Value::String(s)) => s.to_string(),
            other => panic!("expected string, got {other:?}"),
        }
    }
    fn code(r: JsonataResult) -> &'static str {
        match r {
            Err(e) => e.code,
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// $round is half-to-even (banker's), not half-away-from-zero
    /// (spec.md: JS Number semantics for $round).
    #[test]
    fn round_is_half_to_even() {
        assert_eq!(bankers_round(0.5, 0), 0.0);
        assert_eq!(bankers_round(1.5, 0), 2.0);
        assert_eq!(bankers_round(2.5, 0), 2.0);
        assert_eq!(bankers_round(-1.5, 0), -2.0);
        // Positive scale shifts the rule to that decimal place.
        assert_eq!(bankers_round(1.25, 1), 1.2);
        assert_eq!(bankers_round(1.75, 1), 1.8);
        // Negative scale rounds to tens.
        assert_eq!(bankers_round(125.0, -1), 120.0);
    }

    #[test]
    fn round_builtin_applies_scale_argument() {
        assert_eq!(num(fn_round(&[n(1.25), n(1.0)], U)), 1.2);
        assert_eq!(num(fn_round(&[n(2.5)], U)), 2.0);
    }

    /// Go-verified corner cases (2026-08-07): rounding works from the
    /// shortest decimal string, so scaling artifacts and huge scales
    /// cannot distort the result (gnata-nuo.5).
    #[test]
    fn round_string_based_corner_cases() {
        // A hair above the tie must round up, not to even.
        assert_eq!(bankers_round(2.500_000_000_01, 0), 3.0);
        // IEEE scaling artifacts: 4.525*100 = 452.50000000000006 and
        // 0.5655*1000 = 565.4999999999999 — both are true ties/round-ups
        // in decimal.
        assert_eq!(bankers_round(4.525, 2), 4.52);
        assert_eq!(bankers_round(0.5655, 3), 0.566);
        // Scales beyond f64's exponent range no longer overflow to NaN/Inf.
        assert_eq!(bankers_round(1.0, 400), 1.0);
        assert_eq!(bankers_round(1e300, 10), 1e300);
        assert_eq!(bankers_round(1e-300, 2), 0.0);
        // Step additions reformat cleanly: 11.99 + 0.01 is not 12.0 in f64.
        assert_eq!(bankers_round(11.999_999_999_999_998, 2), 12.0);
        // Negative scale keeps its Go/jsonata-js semantics, including the
        // shared NaN underflow for absurd scales.
        assert_eq!(bankers_round(123.456, -2), 100.0);
        assert!(bankers_round(5.0, -400).is_nan());
    }

    #[test]
    fn power_rejects_non_finite_results() {
        assert_eq!(num(fn_power(&[n(2.0), n(10.0)], U)), 1024.0);
        assert_eq!(code(fn_power(&[n(0.0), n(-1.0)], U)), "D3061");
        assert_eq!(code(fn_power(&[n(-2.0), n(0.5)], U)), "D3061");
    }

    #[test]
    fn sqrt_of_negative_is_an_error() {
        assert_eq!(num(fn_sqrt(&[n(144.0)], U)), 12.0);
        assert_eq!(code(fn_sqrt(&[n(-1.0)], U)), "D3060");
    }

    #[test]
    fn format_base_covers_radix_range() {
        assert_eq!(text(fn_format_base(&[n(100.0), n(2.0)], U)), "1100100");
        assert_eq!(text(fn_format_base(&[n(255.0), n(16.0)], U)), "ff");
        assert_eq!(text(fn_format_base(&[n(-100.0), n(2.0)], U)), "-1100100");
        // Radix defaults to 10.
        assert_eq!(text(fn_format_base(&[n(12.0)], U)), "12");
        assert_eq!(code(fn_format_base(&[n(12.0), n(1.0)], U)), "D3100");
        assert_eq!(code(fn_format_base(&[n(12.0), n(37.0)], U)), "D3100");
    }

    #[test]
    fn aggregates_handle_boundaries() {
        let nums = Value::Array(Rc::from(vec![n(1.0), n(2.0), n(3.0), n(4.0)]));
        let nums = std::slice::from_ref(&nums);
        assert_eq!(num(fn_sum(nums, U)), 10.0);
        assert_eq!(num(fn_average(nums, U)), 2.5);
        assert_eq!(num(fn_max(nums, U)), 4.0);
        assert_eq!(num(fn_min(nums, U)), 1.0);
        // Empty arrays: $sum → 0, $max/$min/$average → undefined.
        let empty = Value::Array(Rc::from(Vec::<Value>::new()));
        let empty = std::slice::from_ref(&empty);
        assert_eq!(num(fn_sum(empty, U)), 0.0);
        assert!(matches!(fn_max(empty, U), Ok(Value::Undefined)));
        assert!(matches!(fn_min(empty, U), Ok(Value::Undefined)));
        assert!(matches!(fn_average(empty, U), Ok(Value::Undefined)));
        // Non-numeric element → T0412.
        let mixed = Value::Array(Rc::from(vec![n(1.0), Value::String("x".into())]));
        assert_eq!(code(fn_sum(&[mixed], U)), "T0412");
    }
}
