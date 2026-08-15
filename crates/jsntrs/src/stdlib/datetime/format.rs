//! Picture-format datetime formatting (XPath `format-dateTime` semantics).
//!
//! Component formatters for the `[Y..]`/`[M..]`-style pictures used by
//! `$fromMillis`, plus the default ISO-8601 rendering.

use crate::error::JsonataError;
use crate::stdlib::number_words::{
    int_to_words, int_to_words_ordinal, ordinal_suffix, to_alphabetic, to_roman,
};

use super::calendar::{
    day_of_week, day_of_year, iso_week, iso_week_thursday, secs_to_ymd_hms, week_of_month,
};
use super::tz::format_timezone;
use super::{MONTH_NAMES, WEEKDAY_NAMES};

// ── Default ISO 8601 formatting ──────────────────────────────────────────────

pub(super) fn format_default_iso(ms: i64, tz_offset_secs: i32) -> String {
    // Apply timezone offset to get local time. Saturating: ms may be the
    // clamped cast of an arbitrary f64 (e.g. $fromMillis(1e300)), where
    // a plain add would overflow near i64::MAX.
    let local_ms = ms.saturating_add(i64::from(tz_offset_secs) * 1000);
    let secs = local_ms.div_euclid(1000);
    let millis_part = local_ms.rem_euclid(1000);

    let (y, mo, d, h, mi, s) = secs_to_ymd_hms(secs);

    let base = format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis_part:03}");

    if tz_offset_secs == 0 {
        return base + "Z";
    }
    let sign = if tz_offset_secs >= 0 { '+' } else { '-' };
    let abs_offset = tz_offset_secs.unsigned_abs();
    let oh = abs_offset / 3600;
    let om = (abs_offset % 3600) / 60;
    format!("{base}{sign}{oh:02}:{om:02}")
}

// ── Picture-format formatting ────────────────────────────────────────────────

/// Format epoch milliseconds using an XPath picture string.
///
/// # Errors
/// Returns `D3132` for an unknown component specifier, `D3133` for an
/// unsupported modifier, `D3134` for an over-wide timezone specifier and
/// `D3135` for an unclosed variable marker.
pub fn format_with_picture(
    ms: i64,
    picture: &str,
    tz_offset_secs: i32,
) -> Result<String, JsonataError> {
    // Apply TZ offset (saturating — see format_default_iso).
    let local_ms = ms.saturating_add(i64::from(tz_offset_secs) * 1000);
    let secs = local_ms.div_euclid(1000);
    let ms_frac = local_ms.rem_euclid(1000) as u32;
    let (year, month, day, hour, minute, second) = secs_to_ymd_hms(secs);
    let weekday = day_of_week(year, month, day); // 0=Sun..6=Sat

    // Pre-scan for unclosed brackets.
    let runes: Vec<char> = picture.chars().collect();
    let mut i = 0;
    while i < runes.len() {
        if runes[i] == '[' {
            if i + 1 < runes.len() && runes[i + 1] == '[' {
                i += 2;
                continue;
            }
            let mut j = i + 1;
            while j < runes.len() && runes[j] != ']' {
                j += 1;
            }
            if j >= runes.len() {
                return Err(JsonataError::new(
                    "D3135",
                    "the picture string has an unclosed variable marker '[...'",
                ));
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }

    let mut result = String::new();
    let mut i = 0;
    while i < runes.len() {
        let ch = runes[i];
        if ch == '[' {
            if i + 1 < runes.len() && runes[i + 1] == '[' {
                result.push('[');
                i += 2;
                continue;
            }
            let mut j = i + 1;
            while j < runes.len() && runes[j] != ']' {
                j += 1;
            }
            // Strip whitespace from token.
            let token: String = runes[i + 1..j]
                .iter()
                .filter(|&&c| c != ' ' && c != '\n' && c != '\r' && c != '\t')
                .collect();
            let s = format_token(
                &token,
                year,
                month,
                day,
                hour,
                minute,
                second,
                ms_frac as i32,
                weekday,
                tz_offset_secs,
            )?;
            result.push_str(&s);
            i = j + 1;
            continue;
        }
        if ch == ']' && i + 1 < runes.len() && runes[i + 1] == ']' {
            result.push(']');
            i += 2;
            continue;
        }
        result.push(ch);
        i += 1;
    }
    Ok(result)
}

#[expect(clippy::too_many_arguments)]
pub(super) fn format_token(
    token: &str,
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    ms_frac: i32,
    weekday: u8, // 0=Sun..6=Sat
    tz_offset_secs: i32,
) -> Result<String, JsonataError> {
    // An empty marker (`[]`, or `[ ]` once whitespace is stripped) has no
    // component specifier at all — D3132 with an empty value, same as
    // jsonata-js.
    let Some(component) = token.chars().next() else {
        return Err(unknown_component(""));
    };
    let modifier: String = token.chars().skip(1).collect();

    match component {
        'Y' => format_year_component(year, &modifier),
        'M' => Ok(format_month_token(i64::from(month), &modifier)),
        'D' => Ok(format_day_component(i64::from(day), &modifier)),
        'H' => Ok(format_integer_mod(i64::from(hour), &modifier)),
        'h' => {
            let h12 = i64::from((hour + 11) % 12 + 1);
            Ok(format_integer_mod(h12, &modifier))
        }
        'm' => {
            let m = if modifier.is_empty() { "01" } else { &modifier };
            Ok(format_integer_mod(i64::from(minute), m))
        }
        's' => {
            let m = if modifier.is_empty() { "01" } else { &modifier };
            Ok(format_integer_mod(i64::from(second), m))
        }
        'f' => Ok(format_frac_second(ms_frac, &modifier)),
        'F' => Ok(format_weekday_token(weekday, &modifier)),
        'Z' | 'z' => format_timezone(component, &modifier, tz_offset_secs),
        'P' => Ok(format_ampm(hour, &modifier)),
        'E' | 'C' => Ok("ISO".to_string()),
        'd' => {
            let doy = i64::from(day_of_year(year, month, day));
            Ok(format_day_of_year_token(doy, &modifier))
        }
        'W' => {
            let (_, w) = iso_week(year, month, day);
            Ok(format_integer_mod(i64::from(w), &modifier))
        }
        'X' => {
            let (iso_y, _) = iso_week(year, month, day);
            Ok(format_year_token(iso_y, &modifier))
        }
        'w' => {
            // Week of month (ISO week Thursday method).
            let (_thy, _thm, thd) = iso_week_thursday(year, month, day);
            let wom = week_of_month(thd);
            Ok(format_integer_mod(i64::from(wom), &modifier))
        }
        'x' => {
            // Month of the ISO week (Thursday-based month).
            let (_thy, thm, _thd) = iso_week_thursday(year, month, day);
            Ok(format_iso_week_month(thm, &modifier))
        }
        // Not one of the nineteen XPath component specifiers. jsonata-js
        // raises D3132 here when the marker carries no presentation
        // modifier (`[q]`) and silently emits the JavaScript text
        // "undefined" when it does (`[q1]`); jsntrs raises D3132 for both
        // rather than echoing the marker back (jsntrs-p0v.5).
        _ => Err(unknown_component(&component.to_string())),
    }
}

/// D3132, worded like jsonata-js's message for the same condition.
fn unknown_component(component: &str) -> JsonataError {
    JsonataError::new(
        "D3132",
        format!("unknown component specifier {component:?} in the date/time picture string"),
    )
}

pub(super) fn format_year_component(y: i32, modifier: &str) -> Result<String, JsonataError> {
    match modifier {
        "I" => Ok(to_roman(y.into(), true)),
        "i" => Ok(to_roman(y.into(), false)),
        "w" => Ok(int_to_words(y.into())),
        "W" => Ok(int_to_words(y.into()).to_uppercase()),
        "a" => Ok(to_alphabetic(y.into(), 'a')),
        "A" => Ok(to_alphabetic(y.into(), 'A')),
        "N" => Err(JsonataError::new(
            "D3133",
            format!("the picture string is not valid: unsupported modifier in [Y{modifier}]"),
        )),
        _ => Ok(format_year_token(y, modifier)),
    }
}

pub(super) fn format_year_token(y: i32, modifier: &str) -> String {
    if let Some(rest) = modifier.strip_prefix(',') {
        return truncate_year(y, rest);
    }
    if let Some(comma_pos) = modifier.find(',') {
        let prefix = &modifier[..comma_pos];
        let suffix = &modifier[comma_pos + 1..];
        // Check for max-width truncation: prefix + "-N".
        if let Some(dash_pos) = suffix.find('-') {
            let max_str = &suffix[dash_pos + 1..];
            if let Ok(max_width) = max_str
                .trim_matches(|c: char| c == '#' || c == '*' || c == ' ')
                .parse::<usize>()
                && max_width > 0
            {
                let s = format_integer_mod(i64::from(y), prefix);
                return if s.len() > max_width {
                    s[s.len() - max_width..].to_string()
                } else {
                    s
                };
            }
        }
        // "9,999,*" style grouping.
        if prefix.contains('9') || suffix.contains('9') {
            return format_integer_with_grouping(y);
        }
        return format_integer_mod(i64::from(y), prefix);
    }
    format_integer_mod(i64::from(y), modifier)
}

pub(super) fn truncate_year(y: i32, width_spec: &str) -> String {
    let width: usize = if let Some(dash) = width_spec.find('-') {
        width_spec[..dash].parse().unwrap_or(0)
    } else {
        width_spec.parse().unwrap_or(0)
    };
    if width == 0 {
        return y.to_string();
    }
    let s = y.to_string();
    if s.len() > width {
        s[s.len() - width..].to_string()
    } else {
        s
    }
}

pub(super) fn format_integer_with_grouping(v: i32) -> String {
    let s = v.to_string();
    if s.len() <= 3 {
        return s;
    }
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    result
}

pub(super) fn format_month_token(m: i64, modifier: &str) -> String {
    let month_names = MONTH_NAMES;
    let idx = (m - 1) as usize;
    match modifier {
        mod_ if mod_.starts_with("Nn") => {
            let name = month_names[idx];
            if let Some(suffix) = mod_.strip_prefix("Nn").and_then(|s| s.strip_prefix(',')) {
                let width = parse_width_from_suffix(suffix);
                if width > 0 && name.len() > width {
                    return name[..width].to_string();
                }
            }
            name.to_string()
        }
        "N" => month_names[idx].to_uppercase(),
        "a" => to_alphabetic(m, 'a'),
        "A" => to_alphabetic(m, 'A'),
        "I" => to_roman(m, true),
        "i" => to_roman(m, false),
        _ => format_numeric_with_min_width(m as i32, modifier),
    }
}

pub(super) fn parse_width_from_suffix(suffix: &str) -> usize {
    if let Some(dash) = suffix.find('-') {
        suffix[..dash].parse().unwrap_or(0)
    } else {
        suffix.parse().unwrap_or(0)
    }
}

pub(super) fn format_numeric_with_min_width(v: i32, modifier: &str) -> String {
    let (primary, min_width) = if let Some(comma) = modifier.find(',') {
        let suffix = &modifier[comma + 1..];
        let w = parse_width_from_suffix(suffix);
        (&modifier[..comma], w)
    } else {
        (modifier, 0)
    };
    let s = format_integer_mod(i64::from(v), primary);
    if min_width > 0 && s.len() < min_width {
        format!("{s:0>min_width$}")
    } else {
        s
    }
}

pub(super) fn format_day_component(day: i64, modifier: &str) -> String {
    match modifier {
        "I" => to_roman(day, true),
        "i" => to_roman(day, false),
        "a" => to_alphabetic(day, 'a'),
        "A" => to_alphabetic(day, 'A'),
        "wo" => int_to_words_ordinal(day),
        "Wo" => int_to_words_ordinal(day).to_uppercase(),
        "w" => int_to_words(day),
        "W" => int_to_words(day).to_uppercase(),
        _ => format_day_token(day, modifier),
    }
}

pub(super) fn format_day_token(d: i64, modifier: &str) -> String {
    let is_ordinal = modifier.ends_with('o');
    let base_modifier = if is_ordinal {
        &modifier[..modifier.len() - 1]
    } else {
        modifier
    };
    let s = if base_modifier.contains(',') {
        format_numeric_with_min_width(d as i32, base_modifier)
    } else {
        format_integer_mod(d, base_modifier)
    };
    if is_ordinal { s + ordinal_suffix(d) } else { s }
}

pub(super) fn format_day_of_year_token(doy: i64, modifier: &str) -> String {
    match modifier {
        "wo" => int_to_words_ordinal(doy),
        "Wo" => int_to_words_ordinal(doy).to_uppercase(),
        "w" => int_to_words(doy),
        "W" => int_to_words(doy).to_uppercase(),
        _ => {
            if let Some(base) = modifier.strip_suffix('o') {
                format_integer_mod(doy, base) + ordinal_suffix(doy)
            } else {
                format_integer_mod(doy, modifier)
            }
        }
    }
}

pub(super) fn format_weekday_token(wd: u8, modifier: &str) -> String {
    // wd: 0=Sun, 1=Mon, ..., 6=Sat
    let names = WEEKDAY_NAMES;
    match modifier {
        "" | "n" => names[wd as usize].to_lowercase(),
        m if m.starts_with("Nn") => {
            let name = names[wd as usize];
            if let Some(suffix) = m.strip_prefix("Nn").and_then(|s| s.strip_prefix(',')) {
                let width = parse_width_from_suffix(suffix);
                if width > 0 && name.len() > width {
                    return name[..width].to_string();
                }
            }
            name.to_string()
        }
        "N" => names[wd as usize].to_uppercase(),
        _ => {
            // Numeric: ISO weekday (Mon=1, ..., Sun=7)
            let iso = (i32::from(wd) + 6) % 7 + 1;
            format_integer_mod(i64::from(iso), modifier)
        }
    }
}

pub(super) fn format_ampm(hour: u8, modifier: &str) -> String {
    let s = if hour < 12 { "am" } else { "pm" };
    if modifier == "N" {
        s.to_uppercase()
    } else {
        s.to_string()
    }
}

pub(super) fn format_frac_second(ns_millis: i32, modifier: &str) -> String {
    let width = if modifier.is_empty() {
        3
    } else {
        modifier.len()
    };
    // ms_frac is milliseconds (0-999); pad to 9 digits as nanoseconds.
    let ms_as_ns = i64::from(ns_millis) * 1_000_000;
    let full = format!("{ms_as_ns:09}");
    if width <= 9 {
        full[..width].to_string()
    } else {
        full + &"0".repeat(width - 9)
    }
}

pub(super) fn format_iso_week_month(month: u8, modifier: &str) -> String {
    let m = month as usize;
    match modifier {
        "Nn" | "n" => MONTH_NAMES[m - 1].to_string(),
        "N" => MONTH_NAMES[m - 1].to_uppercase(),
        _ => m.to_string(),
    }
}

/// Format an integer using a modifier string (picture pattern like "", "1", "01", "001", "#", "9,999,*").
pub(super) fn format_integer_mod(v: i64, modifier: &str) -> String {
    let modifier = modifier.trim();

    if modifier.is_empty() || modifier == "1" {
        return v.to_string();
    }

    // Grouping separator (contains comma).
    if modifier.contains(',') {
        let s = v.to_string();
        let neg = s.starts_with('-');
        let digits = if neg { &s[1..] } else { &s };
        if digits.len() > 3 {
            let chars: Vec<char> = digits.chars().collect();
            let n = chars.len();
            let mut result = String::new();
            for (i, &c) in chars.iter().enumerate() {
                if i > 0 && (n - i).is_multiple_of(3) {
                    result.push(',');
                }
                result.push(c);
            }
            return if neg { format!("-{result}") } else { result };
        }
        return s;
    }

    // Optional "#" prefix means plain numeric, no padding.
    if modifier.starts_with('#') {
        return v.to_string();
    }

    // Count leading zeros to determine padding width.
    if modifier.starts_with('0') {
        let digit_count = modifier.chars().take_while(char::is_ascii_digit).count();
        if digit_count > 0 {
            if v < 0 {
                return format!("-{:0>width$}", -v, width = digit_count);
            }
            return format!("{v:0>digit_count$}");
        }
    }

    v.to_string()
}
