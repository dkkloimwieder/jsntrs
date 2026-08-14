//! Datetime functions: $now, $millis, $fromMillis, $toMillis.
//!
//! Implements XPath/JSONata picture-format datetime formatting and parsing.
//! Port of Go `functions/datetime_funcs.go`, `datetime_format.go`, `datetime_parse.go`.

mod calendar;
mod format;
mod parse;
mod tz;
mod words;

use crate::error::{JsonataError, JsonataResult};
use crate::value::Value;

use format::{format_default_iso, format_with_picture};
use parse::{parse_iso_to_millis, parse_with_picture};
use tz::parse_tz;

/// Get current time as milliseconds since Unix epoch.
/// Uses std::time on native/WASI, js_sys::Date::now() on browser WASM.
fn current_millis() -> i64 {
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        js_sys::Date::now() as i64
    }
}

// ── Public entry points ─────────────────────────────────────────────────────

/// `$now(picture?, timezone?)`
///
/// jsonata-js binds `$now` to `fromMillis(now, picture, timezone)`, so the
/// timezone applies whether or not a picture was supplied — `$now(undefined,
/// "+0500")` renders the default ISO form at +05:00, not at UTC
/// (jsntrs-p0v.5).
///
/// # Errors
/// Returns `T0410` for invalid picture/timezone arguments and `D3137` for date formatting errors.
pub fn fn_now(args: &[Value], _focus: &Value) -> JsonataResult {
    let millis = current_millis();
    let picture = match args.first() {
        None | Some(Value::Undefined) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return Err(JsonataError::new(
                "T0410",
                "$now: picture argument must be a string",
            ));
        }
    };
    let tz_offset = match args.get(1) {
        None | Some(Value::Undefined) => 0,
        Some(Value::String(s)) => parse_tz(s)?,
        Some(_) => return Err(JsonataError::new("T0410", "timezone must be a string")),
    };
    match picture {
        Some(p) => Ok(Value::String(
            format_with_picture(millis, &p, tz_offset)?.into(),
        )),
        // Default: ISO 8601 with milliseconds.
        None => Ok(Value::String(format_default_iso(millis, tz_offset).into())),
    }
}

/// `$millis()`
///
/// # Errors
/// This function does not return errors under normal operation.
#[expect(
    clippy::unnecessary_wraps,
    reason = "must match the BuiltinFn signature"
)]
pub fn fn_millis(_args: &[Value], _focus: &Value) -> JsonataResult {
    Ok(Value::Number(current_millis() as f64))
}

/// `$fromMillis(millis, picture?, timezone?)`
///
/// # Errors
/// Returns `T0410` for type mismatches and `D3137` for date formatting errors.
pub fn fn_from_millis(args: &[Value], focus: &Value) -> JsonataResult {
    // With no args, use focus as millis argument.
    let from_focus = args.is_empty();
    let effective_args: &[Value];
    let tmp;
    if args.is_empty() {
        if focus.is_undefined() {
            return Ok(Value::Undefined);
        }
        tmp = std::slice::from_ref(focus);
        effective_args = tmp;
    } else {
        effective_args = args;
    }

    if effective_args[0].is_undefined() {
        return Ok(Value::Undefined);
    }

    let ms = match value_to_f64(&effective_args[0]) {
        Some(v) => v as i64,
        None => {
            return Err(JsonataError::new(
                crate::stdlib::context_arg_code(from_focus),
                "$fromMillis: argument must be a number",
            ));
        }
    };

    // Resolve timezone offset (arg index 2).
    let tz_offset = if effective_args.len() >= 3 && !effective_args[2].is_undefined() {
        match &effective_args[2] {
            Value::String(s) => parse_tz(s)?,
            _ => return Err(JsonataError::new("T0410", "timezone must be a string")),
        }
    } else {
        0
    };

    if effective_args.len() >= 2 && !effective_args[1].is_undefined() {
        let picture = match &effective_args[1] {
            Value::String(s) => s.clone(),
            _ => {
                return Err(JsonataError::new(
                    "T0410",
                    "$fromMillis: picture argument must be a string",
                ));
            }
        };
        let s = format_with_picture(ms, &picture, tz_offset)?;
        return Ok(Value::String(s.into()));
    }

    // No picture: ISO 8601 default.
    Ok(Value::String(format_default_iso(ms, tz_offset).into()))
}

/// `$toMillis(str, picture?, timezone?)`
///
/// # Errors
/// Returns `T0410` for type mismatches and `D3137` for date parsing errors.
pub fn fn_to_millis(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() || args[0].is_undefined() {
        return Ok(Value::Undefined);
    }

    let s = match &args[0] {
        Value::String(s) => s.clone(),
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$toMillis: argument must be a string",
            ));
        }
    };

    if args.len() >= 2 && !args[1].is_undefined() {
        let picture = match &args[1] {
            Value::String(p) => p.clone(),
            _ => {
                return Err(JsonataError::new(
                    "T0410",
                    "$toMillis: picture argument must be a string",
                ));
            }
        };
        let result = parse_with_picture(&s, &picture)?;
        return match result {
            Some(ms) => Ok(Value::Number(ms as f64)),
            None => Ok(Value::Undefined),
        };
    }

    // ISO 8601 / RFC 3339 parsing.
    parse_iso_to_millis(&s)
}

// ── Shared name tables ──────────────────────────────────────────────────────

pub(super) const WEEKDAY_NAMES: &[&str] = &[
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
pub(super) const MONTH_NAMES: &[&str] = &[
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
pub(super) const VALID_COMPONENTS: &[char] = &[
    'Y', 'M', 'D', 'd', 'H', 'h', 'm', 's', 'f', 'F', 'Z', 'z', 'P', 'C', 'E', 'W', 'w', 'X', 'x',
];

// ── Value extraction helper ──────────────────────────────────────────────────

fn value_to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => Some(*n),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::calendar::{day_of_week, iso_week, secs_to_ymd_hms};
    use super::parse::parse_word_number;
    use super::*;
    use crate::stdlib::number_words::{int_to_words, to_roman};

    /// The consumed count is in chars of the original input, not bytes of
    /// the lowercased copy (gnata-nuo.7).
    #[test]
    fn parse_word_number_returns_char_count() {
        let runes: Vec<char> = "twenty-first century".chars().collect();
        assert_eq!(parse_word_number(&runes), (21, 12));
        let runes: Vec<char> = "TWELVE more".chars().collect();
        assert_eq!(parse_word_number(&runes), (12, 6));
        let runes: Vec<char> = "no number".chars().collect();
        assert_eq!(parse_word_number(&runes), (-1, -1));
    }

    fn millis_val(ms: i64) -> Value {
        Value::Number(ms as f64)
    }
    fn str_val(s: &str) -> Value {
        Value::String(s.into())
    }

    fn from_millis_1arg(ms: i64) -> String {
        match fn_from_millis(&[millis_val(ms)], &Value::Undefined) {
            Ok(Value::String(s)) => s.to_string(),
            other => panic!("expected string, got {:?}", other),
        }
    }

    fn from_millis_picture(ms: i64, picture: &str) -> String {
        match fn_from_millis(&[millis_val(ms), str_val(picture)], &Value::Undefined) {
            Ok(Value::String(s)) => s.to_string(),
            other => panic!("expected string, got {:?}", other),
        }
    }

    fn from_millis_picture_tz(ms: i64, picture: &str, tz: &str) -> String {
        match fn_from_millis(
            &[millis_val(ms), str_val(picture), str_val(tz)],
            &Value::Undefined,
        ) {
            Ok(Value::String(s)) => s.to_string(),
            other => panic!("expected string, got {:?}", other),
        }
    }

    fn to_millis_1arg(s: &str) -> i64 {
        match fn_to_millis(&[str_val(s)], &Value::Undefined) {
            Ok(Value::Number(n)) => n as i64,
            other => panic!("expected number, got {:?}", other),
        }
    }

    fn to_millis_picture(s: &str, picture: &str) -> Option<i64> {
        match fn_to_millis(&[str_val(s), str_val(picture)], &Value::Undefined) {
            Ok(Value::Number(n)) => Some(n as i64),
            Ok(Value::Undefined) => None,
            other => panic!("expected number or undefined, got {:?}", other),
        }
    }

    // ── $fromMillis basic ────────────────────────────────────────────

    #[test]
    fn test_from_millis_epoch_plus_1() {
        assert_eq!(from_millis_1arg(1), "1970-01-01T00:00:00.001Z");
    }

    /// Non-ASCII input must produce errors, not char-boundary panics from
    /// the fixed byte-offset slicing in the ISO and timezone parsers.
    #[test]
    fn non_ascii_input_errors_instead_of_panicking() {
        let r = fn_to_millis(&[str_val("2018-\u{20AC}xxT00:00:00Z")], &Value::Undefined);
        assert_eq!(r.expect_err("should not parse").code, "D3110");

        let r = fn_from_millis(
            &[millis_val(0), str_val("[Y]"), str_val("+a\u{20AC}")],
            &Value::Undefined,
        );
        assert!(r.is_err(), "expected bad-timezone error, got {r:?}");

        // Date-only and no-tz forms hit the same slicing.
        for s in ["2018-01-\u{20AC}\u{20AC}", "2018-\u{20AC}xxT00:00:00"] {
            let r = fn_to_millis(&[str_val(s)], &Value::Undefined);
            assert!(r.is_err(), "expected D3110 for {s:?}, got {r:?}");
        }
    }

    #[test]
    fn test_from_millis_known_timestamp() {
        assert_eq!(from_millis_1arg(1509380732935), "2017-10-30T16:25:32.935Z");
    }

    /// Overflowing datetime arithmetic must not panic or wrap (gnata-emj.5).
    #[test]
    fn overflowing_datetime_inputs_do_not_panic() {
        // Year whose epoch milliseconds exceed i64 → undefined, not wrap.
        assert_eq!(to_millis_picture("999999999", "[Y]"), None);
        // 14 alphabetic "digits" overflow the base-26 accumulator → undefined.
        assert_eq!(to_millis_picture("aaaaaaaaaaaaaa", "[Ya]"), None);
        // A year outside i32 → undefined instead of silent truncation.
        assert_eq!(to_millis_picture("99999999999", "[Y]"), None);
        // Excess hours roll into days like JS Date.UTC / Go time.Date:
        // "250 pm" is hour 262 (previously wrapped through u8 to 06:00).
        let base = to_millis_picture("0", "[H]").expect("today midnight parses");
        let rolled = to_millis_picture("250 pm", "[h] [P]").expect("hour 262 parses");
        assert_eq!(rolled - base, 262 * 3_600_000);
        // Saturated $fromMillis input formats (garbage-but-defined date,
        // like Go) instead of overflowing on the timezone adjustment.
        for args in [
            vec![Value::Number(1e300)],
            vec![Value::Number(1e300), str_val("[Y]"), str_val("+0100")],
            vec![Value::Number(-1e300)],
        ] {
            let r = fn_from_millis(&args, &Value::Undefined);
            assert!(
                matches!(r, Ok(Value::String(_))),
                "expected a string for {args:?}, got {r:?}"
            );
        }
    }

    #[test]
    fn test_from_millis_undefined_input() {
        let result = fn_from_millis(&[Value::Undefined], &Value::Undefined);
        assert!(matches!(result, Ok(Value::Undefined)));
    }

    #[test]
    fn test_from_millis_picture_year() {
        assert_eq!(
            from_millis_picture(1521801216617, "Year: [Y0001]"),
            "Year: 2018"
        );
    }

    #[test]
    fn test_from_millis_picture_date() {
        assert_eq!(
            from_millis_picture(1521801216617, "[Y0001]-[M01]-[D01]"),
            "2018-03-23"
        );
    }

    #[test]
    fn test_from_millis_picture_datetime_us() {
        assert_eq!(
            from_millis_picture(1521801216617, "[M01]/[D01]/[Y0001] at [H01]:[m01]:[s01]"),
            "03/23/2018 at 10:33:36"
        );
    }

    #[test]
    fn test_from_millis_picture_iso_with_frac_and_tz() {
        assert_eq!(
            from_millis_picture(
                1521801216617,
                "[Y]-[M01]-[D01]T[H01]:[m]:[s].[f001][Z01:01t]"
            ),
            "2018-03-23T10:33:36.617Z"
        );
    }

    #[test]
    fn test_from_millis_picture_tz_bst() {
        assert_eq!(
            from_millis_picture_tz(
                1521801216617,
                "[Y]-[M01]-[D01]T[H01]:[m]:[s].[f001][Z0101t]",
                "+0100"
            ),
            "2018-03-23T11:33:36.617+0100"
        );
    }

    #[test]
    fn test_from_millis_picture_tz_minus5() {
        assert_eq!(
            from_millis_picture_tz(1531310400000, "[Y]-[M01]-[D01]T[H01]:[m]:[s][Z]", "-0500"),
            "2018-07-11T07:00:00-05:00"
        );
    }

    #[test]
    fn test_from_millis_picture_tz_z_modifier() {
        assert_eq!(
            from_millis_picture(1531310400000, "[Y]-[M01]-[D01]T[H01]:[m]:[s][Z01:01t]"),
            "2018-07-11T12:00:00Z"
        );
    }

    #[test]
    fn test_from_millis_picture_roman_year() {
        assert_eq!(
            from_millis_picture(1521801216617, "[D1] [M01] [YI]"),
            "23 03 MMXVIII"
        );
    }

    #[test]
    fn test_from_millis_picture_ordinal() {
        assert_eq!(
            from_millis_picture(1521801216617, "[D1o] [M01] [Y]"),
            "23rd 03 2018"
        );
    }

    #[test]
    fn test_from_millis_picture_year_words() {
        assert_eq!(
            from_millis_picture(1521801216617, "[Yw]"),
            "two thousand and eighteen"
        );
    }

    #[test]
    fn test_from_millis_picture_month_name() {
        assert_eq!(
            from_millis_picture(1521801216617, "[D1o] [MNn] [Y]"),
            "23rd March 2018"
        );
    }

    #[test]
    fn test_from_millis_picture_weekday_name() {
        assert_eq!(
            from_millis_picture(1521801216617, "[FNn], [D1o] [MNn] [Y]"),
            "Friday, 23rd March 2018"
        );
    }

    #[test]
    fn test_from_millis_picture_default_modifiers() {
        assert_eq!(
            from_millis_picture(1521801216617, "[F], [D]/[M]/[Y] [h]:[m]:[s] [P]"),
            "friday, 23/3/2018 10:33:36 am"
        );
    }

    #[test]
    fn test_from_millis_picture_ampm_uppercase() {
        assert_eq!(
            from_millis_picture(1521801216617, "[F], [D]/[M]/[Y] [h]:[m]:[s] [PN]"),
            "friday, 23/3/2018 10:33:36 AM"
        );
    }

    #[test]
    fn test_from_millis_picture_day_of_year() {
        assert_eq!(
            from_millis_picture(1514808000000, "[dwo] day of the year"),
            "first day of the year"
        );
    }

    #[test]
    fn test_from_millis_picture_week_of_year() {
        assert_eq!(from_millis_picture(1514808000000, "Week: [W]"), "Week: 1");
    }

    #[test]
    fn test_from_millis_picture_week_of_month() {
        assert_eq!(
            from_millis_picture(1359460800000, "Week: [w] of [xNn]"),
            "Week: 5 of January"
        );
    }

    #[test]
    fn test_from_millis_error_unclosed_bracket() {
        let result = fn_from_millis(
            &[millis_val(1419940800000), str_val("[YN]-[M")],
            &Value::Undefined,
        );
        assert!(
            matches!(result, Err(ref e) if e.code == "D3135"),
            "got {:?}",
            result
        );
    }

    #[test]
    fn test_from_millis_error_named_year() {
        let result = fn_from_millis(
            &[millis_val(1419940800000), str_val("[YN]-[M]-[D]")],
            &Value::Undefined,
        );
        assert!(
            matches!(result, Err(ref e) if e.code == "D3133"),
            "got {:?}",
            result
        );
    }

    #[test]
    fn test_from_millis_picture_tz_6digit_error() {
        let result = fn_from_millis(
            &[
                millis_val(1230757500000),
                str_val("[Y]-[M01]-[D01]T[H01]:[m]:[s].[f001][Z010101t]"),
                str_val("+0530"),
            ],
            &Value::Undefined,
        );
        assert!(
            matches!(result, Err(ref e) if e.code == "D3134"),
            "got {:?}",
            result
        );
    }

    // ── $toMillis basic ──────────────────────────────────────────────

    /// jsonata-js binds `$now` straight to `fromMillis(now, picture, tz)`,
    /// so the timezone argument applies with no picture too — the offset
    /// used to be dropped, leaving the default ISO form at UTC
    /// (jsntrs-p0v.5).
    #[test]
    fn now_honours_the_timezone_without_a_picture() {
        let with_tz = match fn_now(&[Value::Undefined, str_val("+0530")], &Value::Undefined) {
            Ok(Value::String(s)) => s.to_string(),
            other => panic!("expected string, got {other:?}"),
        };
        assert!(
            with_tz.ends_with("+05:30"),
            "expected a +05:30 offset, got {with_tz}"
        );
        // The picture form already honoured it and still does.
        let with_picture = match fn_now(&[str_val("[Z]"), str_val("-0500")], &Value::Undefined) {
            Ok(Value::String(s)) => s.to_string(),
            other => panic!("expected string, got {other:?}"),
        };
        assert_eq!(with_picture, "-05:00");
        // No timezone: unchanged, UTC with the trailing Z.
        let plain = match fn_now(&[], &Value::Undefined) {
            Ok(Value::String(s)) => s.to_string(),
            other => panic!("expected string, got {other:?}"),
        };
        assert!(plain.ends_with('Z'), "expected UTC, got {plain}");
    }

    /// jsonata-js raises D3132 for an unknown component specifier and emits
    /// the literal text "undefined" when the unknown marker carries a
    /// presentation modifier; jsntrs raises D3132 for both instead of
    /// echoing the marker back verbatim (jsntrs-p0v.5).
    #[test]
    fn unknown_format_component_is_d3132() {
        for picture in ["[q]", "[q1]", "[qN]", "[G1]", "[]", "[ ]", "[Y]-[M01] [k]"] {
            let result = fn_from_millis(&[millis_val(0), str_val(picture)], &Value::Undefined);
            assert!(
                matches!(result, Err(ref e) if e.code == "D3132"),
                "picture {picture:?}: got {result:?}"
            );
        }
        // The nineteen real specifiers keep working, including the two that
        // render the calendar name.
        assert_eq!(from_millis_picture(0, "[E]/[C]"), "ISO/ISO");
    }

    /// There is no IANA database behind the hand-rolled calendar math, so a
    /// zone name is a clean D3137 rather than jsonata-js's `parseInt` NaN
    /// (which formats as the literal text "NaN").
    #[test]
    fn unknown_timezone_name_is_d3137() {
        for tz in ["America/New_York", "bogus", "+05:3", ""] {
            let result = fn_from_millis(
                &[millis_val(0), str_val("[Y]"), str_val(tz)],
                &Value::Undefined,
            );
            assert!(
                matches!(result, Err(ref e) if e.code == "D3137"),
                "timezone {tz:?}: got {result:?}"
            );
        }
        // The accepted spellings still resolve to a zero offset.
        for tz in ["UTC", "utc", "GMT", "Z"] {
            assert_eq!(from_millis_picture_tz(0, "[H01]", tz), "00");
        }
    }

    #[test]
    fn test_to_millis_iso8601() {
        assert_eq!(to_millis_1arg("1970-01-01T00:00:00.001Z"), 1);
    }

    #[test]
    fn test_to_millis_known_timestamp() {
        assert_eq!(to_millis_1arg("2017-10-30T16:25:32.935Z"), 1509380732935);
    }

    #[test]
    fn test_to_millis_undefined_input() {
        let result = fn_to_millis(&[Value::Undefined], &Value::Undefined);
        assert!(matches!(result, Ok(Value::Undefined)));
    }

    #[test]
    fn test_to_millis_picture_year() {
        assert_eq!(to_millis_picture("2018", "[Y1]"), Some(1514764800000));
    }

    #[test]
    fn test_to_millis_picture_ymd() {
        assert_eq!(
            to_millis_picture("2018-03-27", "[Y1]-[M01]-[D01]"),
            Some(1522108800000)
        );
    }

    #[test]
    fn test_to_millis_picture_iso_format() {
        assert_eq!(
            to_millis_picture(
                "2018-03-27T14:03:00.123Z",
                "[Y0001]-[M01]-[D01]T[H01]:[m01]:[s01].[f001]Z"
            ),
            Some(1522159380123)
        );
    }

    #[test]
    fn test_to_millis_picture_ordinal() {
        assert_eq!(
            to_millis_picture("27th 3 1976", "[D1o] [M#1] [Y0001]"),
            Some(196732800000)
        );
    }

    #[test]
    fn test_to_millis_picture_roman_year() {
        assert_eq!(to_millis_picture("MCMLXXXIV", "[YI]"), Some(441763200000));
    }

    #[test]
    fn test_to_millis_picture_month_name() {
        assert_eq!(
            to_millis_picture("27th April 2008", "[D1o] [MNn] [Y0001]"),
            Some(1209254400000)
        );
    }

    #[test]
    fn test_to_millis_picture_words() {
        assert_eq!(
            to_millis_picture("one thousand, nine hundred and eighty-four", "[Yw]"),
            Some(441763200000)
        );
    }

    #[test]
    fn test_to_millis_picture_12h_am() {
        assert_eq!(
            to_millis_picture("4/4/2018 12:06 am", "[D1]/[M1]/[Y0001] [h]:[m] [P]"),
            Some(1522800360000)
        );
    }

    #[test]
    fn test_to_millis_picture_day_of_year() {
        assert_eq!(
            to_millis_picture("2018-094", "[Y0001]-[d001]"),
            Some(1522800000000)
        );
    }

    #[test]
    fn test_to_millis_picture_timezone() {
        let result = to_millis_picture(
            "2020-09-09 08:00:00 +02:00",
            "[Y0001]-[M01]-[D01] [H01]:[m01]:[s01] [Z]",
        );
        // 2020-09-09 08:00:00 +02:00 = 2020-09-09 06:00:00 UTC
        let ts = fn_from_millis(&[Value::Number(result.unwrap() as f64)], &Value::Undefined);
        assert!(
            matches!(ts, Ok(Value::String(ref s)) if s.starts_with("2020-09-09T06:00:00")),
            "got {:?}",
            ts
        );
    }

    #[test]
    fn test_to_millis_picture_error_unknown_component() {
        let result = fn_to_millis(
            &[str_val("2018-05-22"), str_val("[Y]-[M]-[q]")],
            &Value::Undefined,
        );
        assert!(
            matches!(result, Err(ref e) if e.code == "D3132"),
            "got {:?}",
            result
        );
    }

    #[test]
    fn test_to_millis_picture_error_named_year() {
        let result = fn_to_millis(
            &[str_val("2018-05-22"), str_val("[YN]-[M]-[D]")],
            &Value::Undefined,
        );
        assert!(
            matches!(result, Err(ref e) if e.code == "D3133"),
            "got {:?}",
            result
        );
    }

    #[test]
    fn test_to_millis_picture_error_no_month() {
        let result = fn_to_millis(&[str_val("2018-22"), str_val("[Y]-[D]")], &Value::Undefined);
        assert!(
            matches!(result, Err(ref e) if e.code == "D3136"),
            "got {:?}",
            result
        );
    }

    #[test]
    fn test_to_millis_picture_no_match() {
        let result = to_millis_picture("irrelevant string", "[Y]-[M]-[D]");
        assert_eq!(result, None);
    }

    // ── Calendar helpers ─────────────────────────────────────────────

    #[test]
    fn test_secs_to_ymd_hms_epoch() {
        assert_eq!(secs_to_ymd_hms(0), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn test_secs_to_ymd_hms_known() {
        // 2018-03-23T10:33:36
        assert_eq!(secs_to_ymd_hms(1521801216), (2018, 3, 23, 10, 33, 36));
    }

    #[test]
    fn test_secs_to_ymd_hms_negative() {
        // 1969-12-31T23:59:59 = epoch - 1 second
        assert_eq!(secs_to_ymd_hms(-1), (1969, 12, 31, 23, 59, 59));
    }

    #[test]
    fn test_day_of_week_friday() {
        // 2018-03-23 is a Friday (5 in our encoding 0=Sun,5=Fri)
        assert_eq!(day_of_week(2018, 3, 23), 5);
    }

    #[test]
    fn test_iso_week_known() {
        // 2018-01-01 is Monday, week 1
        assert_eq!(iso_week(2018, 1, 1), (2018, 1));
        // 2018-12-31 is Monday, week 1 of 2019
        assert_eq!(iso_week(2018, 12, 31), (2019, 1));
    }

    #[test]
    fn test_iso_week_all_edge_cases() {
        // From isoWeekDate.json test suite
        assert_eq!(iso_week(2005, 1, 1), (2004, 53)); // Sat 1 Jan 2005
        assert_eq!(iso_week(2005, 1, 2), (2004, 53)); // Sun 2 Jan 2005
        assert_eq!(iso_week(2005, 12, 31), (2005, 52)); // Sat 31 Dec 2005
        assert_eq!(iso_week(2006, 1, 1), (2005, 52)); // Sun 1 Jan 2006
        assert_eq!(iso_week(2006, 1, 2), (2006, 1)); // Mon 2 Jan 2006
        assert_eq!(iso_week(2006, 12, 31), (2006, 52)); // Sun 31 Dec 2006
        assert_eq!(iso_week(2007, 1, 1), (2007, 1)); // Mon 1 Jan 2007
        assert_eq!(iso_week(2007, 12, 30), (2007, 52)); // Sun 30 Dec 2007
        assert_eq!(iso_week(2007, 12, 31), (2008, 1)); // Mon 31 Dec 2007
        assert_eq!(iso_week(2008, 1, 1), (2008, 1)); // Tue 1 Jan 2008
        assert_eq!(iso_week(2008, 12, 28), (2008, 52)); // Sun 28 Dec 2008
        assert_eq!(iso_week(2008, 12, 29), (2009, 1)); // Mon 29 Dec 2008
        assert_eq!(iso_week(2008, 12, 30), (2009, 1)); // Tue 30 Dec 2008
        assert_eq!(iso_week(2008, 12, 31), (2009, 1)); // Wed 31 Dec 2008
        assert_eq!(iso_week(2009, 1, 1), (2009, 1)); // Thu 1 Jan 2009
        assert_eq!(iso_week(2009, 12, 31), (2009, 53)); // Thu 31 Dec 2009
        assert_eq!(iso_week(2010, 1, 1), (2009, 53)); // Fri 1 Jan 2010
        assert_eq!(iso_week(2010, 1, 2), (2009, 53)); // Sat 2 Jan 2010
        assert_eq!(iso_week(2010, 1, 3), (2009, 53)); // Sun 3 Jan 2010
        // From formatDateTime.json W tests
        assert_eq!(iso_week(2014, 12, 23), (2014, 52)); // Tue
        assert_eq!(iso_week(2014, 12, 28), (2014, 52)); // Sun
        assert_eq!(iso_week(2014, 12, 29), (2015, 1)); // Mon
        assert_eq!(iso_week(2015, 1, 1), (2015, 1)); // Thu
        assert_eq!(iso_week(2015, 1, 5), (2015, 2)); // Mon
        assert_eq!(iso_week(2015, 12, 28), (2015, 53)); // Mon
        assert_eq!(iso_week(2015, 12, 31), (2015, 53)); // Thu
        assert_eq!(iso_week(2016, 1, 2), (2015, 53)); // Sat
    }

    #[test]
    fn test_int_to_words() {
        assert_eq!(int_to_words(0), "zero");
        assert_eq!(int_to_words(1), "one");
        assert_eq!(int_to_words(18), "eighteen");
        assert_eq!(int_to_words(42), "forty-two");
        assert_eq!(
            int_to_words(1984),
            "one thousand, nine hundred and eighty-four"
        );
        assert_eq!(int_to_words(2018), "two thousand and eighteen");
    }

    #[test]
    fn test_to_roman() {
        assert_eq!(to_roman(2018, true), "MMXVIII");
        assert_eq!(to_roman(1984, true), "MCMLXXXIV");
        assert_eq!(to_roman(3, false), "iii");
    }

    #[test]
    fn test_format_default_iso() {
        assert_eq!(format_default_iso(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_default_iso(1, 0), "1970-01-01T00:00:00.001Z");
        assert_eq!(
            format_default_iso(1509380732935, 0),
            "2017-10-30T16:25:32.935Z"
        );
    }
}
