//! ISO-8601 and picture-format datetime parsing for `$toMillis`.

use crate::error::{JsonataError, JsonataResult};
use crate::stdlib::number_words::roman_value;
use crate::value::Value;

use super::calendar::{datetime_to_epoch_ms, secs_to_ymd_hms};
use super::current_millis;
use super::tz::parse_tz_from_input;
use super::words::parse_word_number_from_string;
use super::{MONTH_NAMES, VALID_COMPONENTS, WEEKDAY_NAMES};

// ── ISO 8601 parsing (no picture) ───────────────────────────────────────────

pub(super) fn parse_iso_to_millis(s: &str) -> JsonataResult {
    // Try ISO 8601 with timezone: "YYYY-MM-DDTHH:MM:SS.sssZ" or "+HH:MM" suffix.
    if let Some(ms) = try_parse_iso_with_tz(s) {
        return Ok(Value::Number(ms as f64));
    }

    // Try date-only "YYYY-MM-DD".
    if let Some(ms) = try_parse_date_only(s) {
        return Ok(Value::Number(ms as f64));
    }

    // Try year-only "YYYY".
    if let Some(ms) = try_parse_year_only(s) {
        return Ok(Value::Number(ms as f64));
    }

    // Try datetime without timezone "YYYY-MM-DDTHH:MM:SS" or with sub-seconds.
    if let Some(ms) = try_parse_datetime_no_tz(s) {
        return Ok(Value::Number(ms as f64));
    }

    Err(JsonataError::new(
        "D3110",
        format!("$toMillis: the value '{s}' does not match the standard datetime format"),
    ))
}

/// Parse ISO 8601 / RFC 3339 with timezone suffix (Z, +HH:MM, -HHMM, etc.)
pub(super) fn try_parse_iso_with_tz(s: &str) -> Option<i64> {
    // ISO 8601 is ASCII-only; rejecting non-ASCII up front keeps the fixed
    // byte-offset slices below on char boundaries.
    if !s.is_ascii() {
        return None;
    }
    // Must have at least "YYYY-MM-DDTHH:MM:SSZ" = 20 chars
    if s.len() < 20 || s.as_bytes()[4] != b'-' || s.as_bytes()[10] != b'T' {
        return None;
    }

    let y: i32 = s[0..4].parse().ok()?;
    let m: u8 = s[5..7].parse().ok()?;
    let d: u8 = s[8..10].parse().ok()?;
    let h: u8 = s[11..13].parse().ok()?;
    let mi: u8 = s[14..16].parse().ok()?;
    let sec: u8 = s[17..19].parse().ok()?;

    // Parse fractional seconds and find where the tz suffix starts.
    let rest = &s[19..];
    let (ms, tz_part) = if rest.starts_with('.') {
        // Find end of digits after dot.
        let frac_end = 1 + rest[1..].bytes().take_while(|b| b.is_ascii_digit()).count();
        let frac_str = &rest[1..frac_end];
        let frac_val: i32 = frac_str.parse().ok()?;
        let ms_val = match frac_str.len() {
            1 => frac_val * 100,
            2 => frac_val * 10,
            3 => frac_val,
            n if n > 3 => (frac_val as f64 / 10f64.powi(n as i32 - 3)) as i32,
            _ => 0,
        };
        (ms_val, &rest[frac_end..])
    } else {
        (0, rest)
    };

    // Parse timezone suffix.
    let offset_secs = match tz_part {
        "Z" | "z" => 0i64,
        _ if tz_part.len() >= 5 => {
            let sign: i64 = if tz_part.starts_with('+') {
                1
            } else if tz_part.starts_with('-') {
                -1
            } else {
                return None;
            };
            let tz_digits = &tz_part[1..];
            let (th, tm) = if tz_digits.contains(':') && tz_digits.len() >= 5 {
                let th: i64 = tz_digits[0..2].parse().ok()?;
                let tm: i64 = tz_digits[3..5].parse().ok()?;
                (th, tm)
            } else if tz_digits.len() >= 4 {
                let th: i64 = tz_digits[0..2].parse().ok()?;
                let tm: i64 = tz_digits[2..4].parse().ok()?;
                (th, tm)
            } else {
                return None;
            };
            sign * (th * 3600 + tm * 60)
        }
        _ => return None,
    };

    let base_ms = datetime_to_epoch_ms(
        y,
        m.into(),
        d.into(),
        h.into(),
        mi.into(),
        sec.into(),
        ms.into(),
    )?;
    base_ms.checked_sub(offset_secs * 1000)
}

pub(super) fn try_parse_date_only(s: &str) -> Option<i64> {
    // ASCII-only, like all ISO 8601 forms — keeps byte slicing safe.
    if !s.is_ascii() {
        return None;
    }
    // "YYYY-MM-DD"
    if s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        let y: i32 = s[0..4].parse().ok()?;
        let m: i64 = s[5..7].parse().ok()?;
        let d: i64 = s[8..10].parse().ok()?;
        return datetime_to_epoch_ms(y, m, d, 0, 0, 0, 0);
    }
    None
}

pub(super) fn try_parse_year_only(s: &str) -> Option<i64> {
    if s.len() == 4 {
        let y: i32 = s.parse().ok()?;
        return datetime_to_epoch_ms(y, 1, 1, 0, 0, 0, 0);
    }
    None
}

pub(super) fn try_parse_datetime_no_tz(s: &str) -> Option<i64> {
    // ASCII-only, like all ISO 8601 forms — keeps byte slicing safe.
    if !s.is_ascii() {
        return None;
    }
    // "YYYY-MM-DDTHH:MM:SS" or "YYYY-MM-DDTHH:MM:SS.sss"
    if s.len() >= 19 && s.as_bytes()[4] == b'-' && s.as_bytes()[10] == b'T' {
        let date_part = &s[0..10];
        let time_part = &s[11..];
        let y: i32 = date_part[0..4].parse().ok()?;
        let m: u8 = date_part[5..7].parse().ok()?;
        let d: u8 = date_part[8..10].parse().ok()?;

        let h: u8 = time_part[0..2].parse().ok()?;
        let mi: u8 = time_part[3..5].parse().ok()?;
        let sec: u8 = time_part[6..8].parse().ok()?;
        let ms: i32 = if time_part.len() > 9 && time_part.as_bytes()[8] == b'.' {
            let frac = &time_part[9..];
            let frac = &frac[..frac.len().min(3)];
            let v: i32 = frac.parse().ok()?;
            match frac.len() {
                1 => v * 100,
                2 => v * 10,
                _ => v,
            }
        } else {
            0
        };

        return datetime_to_epoch_ms(
            y,
            m.into(),
            d.into(),
            h.into(),
            mi.into(),
            sec.into(),
            ms.into(),
        );
    }
    None
}

// ── Picture-format parsing ───────────────────────────────────────────────────

#[derive(Debug)]
pub(super) struct PicturePart {
    is_token: bool,
    component: char,
    modifier: String,
    literal: String,
}

#[expect(clippy::too_many_lines)]
pub(super) fn parse_with_picture(input: &str, picture: &str) -> Result<Option<i64>, JsonataError> {
    let runes: Vec<char> = picture.chars().collect();
    let mut parts: Vec<PicturePart> = Vec::new();
    let mut i = 0;

    while i < runes.len() {
        if runes[i] == '[' {
            if i + 1 < runes.len() && runes[i + 1] == '[' {
                parts.push(PicturePart {
                    is_token: false,
                    component: '\0',
                    modifier: String::new(),
                    literal: "[".into(),
                });
                i += 2;
                continue;
            }
            let mut j = i + 1;
            while j < runes.len() && runes[j] != ']' {
                j += 1;
            }
            if j >= runes.len() {
                return Ok(None); // Unclosed bracket → undefined
            }
            let tok: String = runes[i + 1..j]
                .iter()
                .filter(|&&c| c != ' ' && c != '\n' && c != '\r' && c != '\t')
                .collect();
            if tok.is_empty() {
                i = j + 1;
                continue;
            }
            let comp = tok
                .chars()
                .next()
                .ok_or_else(|| JsonataError::new("D3132", "unexpected empty picture token"))?;
            if !VALID_COMPONENTS.contains(&comp) {
                return Err(JsonataError::new(
                    "D3132",
                    format!("$toMillis: unknown picture component '{comp}'"),
                ));
            }
            let mod_: String = tok.chars().skip(1).collect();
            // [YN] is invalid.
            if comp == 'Y' && mod_ == "N" {
                return Err(JsonataError::new(
                    "D3133",
                    "$toMillis: the picture string is not valid: unsupported modifier [YN]",
                ));
            }
            parts.push(PicturePart {
                is_token: true,
                component: comp,
                modifier: mod_,
                literal: String::new(),
            });
            i = j + 1;
        } else if runes[i] == ']' && i + 1 < runes.len() && runes[i + 1] == ']' {
            parts.push(PicturePart {
                is_token: false,
                component: '\0',
                modifier: String::new(),
                literal: "]".into(),
            });
            i += 2;
        } else {
            parts.push(PicturePart {
                is_token: false,
                component: '\0',
                modifier: String::new(),
                literal: runes[i].to_string(),
            });
            i += 1;
        }
    }

    // Track which components appear.
    let mut has_cal_y = false;
    let mut has_week_y = false;
    let mut has_m = false;
    let mut has_d = false;
    let mut has_doy = false;
    let mut has_h = false;
    let mut has_hour = false;
    let mut has_min = false;
    let mut has_sec = false;

    for p in &parts {
        if !p.is_token {
            continue;
        }
        match p.component {
            'Y' => has_cal_y = true,
            'X' => has_week_y = true,
            'M' => has_m = true,
            'D' => has_d = true,
            'd' => has_doy = true,
            'H' => {
                has_h = true;
                has_hour = true;
            }
            'h' => has_hour = true,
            'm' => has_min = true,
            's' => has_sec = true,
            _ => {}
        }
    }

    // If the picture has no datetime components at all, return undefined.
    let has_any_token = parts.iter().any(|p| p.is_token);
    if !has_any_token {
        return Ok(None);
    }

    // Validation.
    if has_d && !has_m && !has_doy {
        return Err(JsonataError::new(
            "D3136",
            "$toMillis: the date/time picture is underspecified; missing month component",
        ));
    }
    if (has_min || has_sec) && !has_hour && !has_h {
        return Err(JsonataError::new(
            "D3136",
            "$toMillis: the date/time picture is underspecified; missing hour component",
        ));
    }
    if has_week_y && !has_cal_y {
        return Err(JsonataError::new(
            "D3136",
            "$toMillis: the date/time picture is underspecified; week-based year requires full calendar date",
        ));
    }

    // Parse the input using parts.
    let input_runes: Vec<char> = input.chars().collect();
    let mut pos = 0;
    // Wide accumulators: parsed component values are not range-limited
    // (excess rolls into larger units at epoch conversion), so narrow
    // types would silently truncate — e.g. "250 pm" is hour 262.
    let mut year = 0i32;
    let mut month = 0i64;
    let mut day = 0i64;
    let mut hour = 0i64;
    let mut minute = 0i64;
    let mut second = 0i64;
    let mut millisec = 0i32;
    let mut day_of_year = 0i64;
    let mut tz_offset = 0i32;
    let mut is_pm = false;
    let mut is_12h = false;
    let mut has_tz = false;
    let mut has_year = false;

    for part in &parts {
        if !part.is_token {
            // Consume literal.
            for lr in part.literal.chars() {
                if pos >= input_runes.len() {
                    return Ok(None);
                }
                if input_runes[pos].to_lowercase().next() != lr.to_lowercase().next() {
                    return Ok(None);
                }
                pos += 1;
            }
            continue;
        }

        match part.component {
            'Y' | 'X' => {
                has_year = true;
                let (v, n) = parse_token_value(&input_runes[pos..], &part.modifier);
                if n < 0 {
                    return Ok(None);
                }
                // A year outside i32 cannot form a representable timestamp.
                let Ok(v) = i32::try_from(v) else {
                    return Ok(None);
                };
                year = v;
                pos += n as usize;
            }
            'M' => {
                let (v, n) = parse_token_value(&input_runes[pos..], &part.modifier);
                if n < 0 {
                    return Ok(None);
                }
                month = v;
                pos += n as usize;
            }
            'D' => {
                let (v, n) = parse_token_value(&input_runes[pos..], &part.modifier);
                if n < 0 {
                    return Ok(None);
                }
                day = v;
                pos += n as usize;
            }
            'd' => {
                let (v, n) = parse_token_value(&input_runes[pos..], &part.modifier);
                if n < 0 {
                    return Ok(None);
                }
                day_of_year = v;
                pos += n as usize;
            }
            'H' => {
                let (v, n) = parse_token_value(&input_runes[pos..], &part.modifier);
                if n < 0 {
                    return Ok(None);
                }
                hour = v;
                pos += n as usize;
            }
            'h' => {
                is_12h = true;
                let (v, n) = parse_token_value(&input_runes[pos..], &part.modifier);
                if n < 0 {
                    return Ok(None);
                }
                hour = v;
                pos += n as usize;
            }
            'm' => {
                let (v, n) = parse_token_value(&input_runes[pos..], &part.modifier);
                if n < 0 {
                    return Ok(None);
                }
                minute = v;
                pos += n as usize;
            }
            's' => {
                let (v, n) = parse_token_value(&input_runes[pos..], &part.modifier);
                if n < 0 {
                    return Ok(None);
                }
                second = v;
                pos += n as usize;
            }
            'f' => {
                let (v, n) = parse_token_value(&input_runes[pos..], &part.modifier);
                if n < 0 {
                    return Ok(None);
                }
                // Normalize to milliseconds.
                let ms_val = normalize_frac_to_ms(v, n as usize);
                millisec = ms_val;
                pos += n as usize;
            }
            'P' => {
                if pos + 2 > input_runes.len() {
                    return Ok(None);
                }
                let s2: String = input_runes[pos..pos + 2].iter().collect();
                match s2.to_lowercase().as_str() {
                    "am" => {
                        is_pm = false;
                        pos += 2;
                    }
                    "pm" => {
                        is_pm = true;
                        pos += 2;
                    }
                    _ => return Ok(None),
                }
            }
            'F' => {
                let n = consume_name_or_number(&input_runes[pos..], &part.modifier);
                if n > 0 {
                    pos += n;
                }
            }
            'Z' | 'z' => {
                let (offset, n) = parse_tz_from_input(&input_runes[pos..]);
                if n > 0 {
                    tz_offset = offset;
                    has_tz = true;
                    pos += n;
                }
            }
            'W' | 'w' | 'x' => {
                let n = consume_name_or_number(&input_runes[pos..], &part.modifier);
                if n > 0 {
                    pos += n;
                }
            }
            _ => {}
        }
    }

    // Adjust 12-hour clock.
    if is_12h {
        if is_pm && hour != 12 {
            hour += 12;
        } else if !is_pm && hour == 12 {
            hour = 0;
        }
    }

    // If no year seen, use today for time-only pictures.
    if !has_year && year == 0 && (has_hour || has_min) {
        let ms = current_millis();
        let (y, mo, d, _, _, _) = secs_to_ymd_hms(ms / 1000);
        year = y;
        month = mo.into();
        day = d.into();
    }

    if day_of_year > 0 {
        let ms = date_to_ms_with_doy(year, day_of_year, hour, minute, second, millisec);
        let ms_utc = ms.and_then(|ms| ms.checked_sub(i64::from(tz_offset) * 1000));
        return Ok(ms_utc);
    }

    if month == 0 {
        month = 1;
    }
    if day == 0 {
        day = 1;
    }

    let ms = datetime_to_epoch_ms(year, month, day, hour, minute, second, millisec.into());
    let ms_utc = if has_tz {
        ms.and_then(|ms| ms.checked_sub(i64::from(tz_offset) * 1000))
    } else {
        ms
    };
    Ok(ms_utc)
}

pub(super) fn normalize_frac_to_ms(v: i64, n: usize) -> i32 {
    let mut val = v;
    if n < 3 {
        for _ in n..3 {
            val *= 10;
        }
    } else if n > 3 {
        for _ in 3..n {
            val /= 10;
        }
    }
    val as i32
}

pub(super) fn date_to_ms_with_doy(
    year: i32,
    doy: i64,
    hour: i64,
    minute: i64,
    second: i64,
    ms: i32,
) -> Option<i64> {
    // Start from Jan 1 of year, add (doy - 1) days.
    let jan1_ms = datetime_to_epoch_ms(year, 1, 1, hour, minute, second, ms.into())?;
    jan1_ms.checked_add((doy - 1).checked_mul(86_400_000)?)
}

// ── Token value parsing ──────────────────────────────────────────────────────

pub(super) fn parse_token_value(runes: &[char], modifier: &str) -> (i64, i64) {
    if runes.is_empty() {
        return (-1, -1);
    }
    if modifier == "I" || modifier == "i" {
        return parse_roman(runes);
    }
    if modifier == "a" || modifier == "A" {
        return parse_alphabetic(runes);
    }
    if modifier == "N"
        || modifier == "n"
        || modifier == "Nn"
        || modifier.starts_with("Nn")
        || modifier.starts_with('N')
    {
        return parse_month_name(runes, modifier);
    }
    if modifier == "w"
        || modifier == "W"
        || modifier.starts_with("wo")
        || modifier.starts_with("Wo")
        || modifier.starts_with("Ww")
        || modifier.starts_with("ww")
    {
        return parse_word_number(runes);
    }
    if modifier.ends_with('o') {
        return parse_ordinal_number(runes);
    }
    parse_numeric_value(runes, modifier)
}

pub(super) fn parse_numeric_value(runes: &[char], modifier: &str) -> (i64, i64) {
    let mut i = 0;
    let sign: i64 = if !runes.is_empty() && runes[0] == '-' {
        i += 1;
        -1
    } else {
        1
    };
    let start = i;
    let max_w = modifier_field_width(modifier);
    while i < runes.len() && runes[i].is_ascii_digit() {
        if max_w > 0 && (i - start) >= max_w {
            break;
        }
        i += 1;
    }
    if i == start {
        return (-1, -1);
    }
    let s: String = runes[start..i].iter().collect();
    match s.parse::<i64>() {
        Ok(n) => (sign * n, i as i64),
        Err(_) => (-1, -1),
    }
}

pub(super) fn modifier_field_width(modifier: &str) -> usize {
    if modifier.is_empty() {
        return 0;
    }
    if modifier.starts_with(',') {
        if let Some(dash) = modifier.rfind('-') {
            let part = &modifier[dash + 1..];
            if let Ok(n) = part.parse::<usize>()
                && n > 0
            {
                return n;
            }
        }
        return 0;
    }
    if modifier.len() < 2 {
        return 0;
    }
    if modifier.chars().all(|c| c == '0' || c == '1') {
        return modifier.len();
    }
    0
}

pub(super) fn parse_ordinal_number(runes: &[char]) -> (i64, i64) {
    let mut i = 0;
    while i < runes.len() && runes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return (-1, -1);
    }
    let s: String = runes[..i].iter().collect();
    let n: i64 = match s.parse() {
        Ok(v) => v,
        Err(_) => return (-1, -1),
    };
    // Consume optional ordinal suffix (st/nd/rd/th).
    if i + 2 <= runes.len() {
        let suffix: String = runes[i..i + 2].iter().collect();
        match suffix.to_lowercase().as_str() {
            "st" | "nd" | "rd" | "th" => {
                i += 2;
            }
            _ => {}
        }
    }
    (n, i as i64)
}

pub(super) fn parse_roman(runes: &[char]) -> (i64, i64) {
    let mut i = 0;
    while i < runes.len() && roman_value(runes[i]).is_some() {
        i += 1;
    }
    if i == 0 {
        return (-1, -1);
    }
    let mut total: i64 = 0;
    let mut prev: i64 = 0;
    for j in (0..i).rev() {
        let v = roman_value(runes[j]).unwrap_or(0);
        if v < prev {
            total -= v;
        } else {
            total += v;
            prev = v;
        }
    }
    (total, i as i64)
}

/// Read an alphabetic numeral (`a`/`A` picture component) off the front of
/// `runes`: `a` = 1 … `z` = 26, `aa` = 27, base 26.
///
/// Case-insensitive, and deliberately not parameterised by the picture's
/// case: XPath 3.1 F&O §4.6 specifies these sequences for *formatting*
/// (format-integer) and says nothing about parsing them back, and the
/// JSONata documentation for `$parseInteger` says only that it "parses the
/// contents of the `string` parameter to an integer … using the format
/// specified by the `picture` string". Neither obliges the parser to reject
/// `"AB"` for picture `a`, so it does not. The function used to compute
/// `let base = if modifier == "A" { 'A' } else { 'a' }` and then discard it
/// with `let _ = base;`, which read as if the case mattered.
pub(super) fn parse_alphabetic(runes: &[char]) -> (i64, i64) {
    let mut i = 0;
    let mut result: i64 = 0;
    while i < runes.len() && runes[i].is_ascii_alphabetic() {
        let c = runes[i].to_lowercase().next().unwrap_or(runes[i]);
        let digit = (c as i64) - ('a' as i64) + 1;
        // 14+ letters overflow i64 — treat as unparseable, don't wrap.
        result = match result.checked_mul(26).and_then(|r| r.checked_add(digit)) {
            Some(r) => r,
            None => return (-1, -1),
        };
        i += 1;
    }
    if i == 0 {
        return (-1, -1);
    }
    (result, i as i64)
}

pub(super) fn parse_month_name(runes: &[char], modifier: &str) -> (i64, i64) {
    let max_len = parse_name_max_len(modifier);
    for (mi, &name) in MONTH_NAMES.iter().enumerate() {
        if max_len > 0 && max_len < name.chars().count() {
            let abbr: String = name.chars().take(max_len).collect();
            if runes.len() >= max_len {
                let s: String = runes[..max_len].iter().collect();
                if s.to_lowercase() == abbr.to_lowercase() {
                    return ((mi + 1) as i64, max_len as i64);
                }
            }
        } else {
            let name_len = name.chars().count();
            if runes.len() >= name_len {
                let s: String = runes[..name_len].iter().collect();
                if s.to_lowercase() == name.to_lowercase() {
                    return ((mi + 1) as i64, name_len as i64);
                }
            }
        }
    }
    (-1, -1)
}

pub(super) fn parse_name_max_len(modifier: &str) -> usize {
    if modifier.contains(',') {
        let parts: Vec<&str> = modifier.splitn(2, ',').collect();
        if parts.len() == 2 {
            let range_part = parts[1];
            let range_parts: Vec<&str> = range_part.split('-').collect();
            if let Some(last) = range_parts.last()
                && let Ok(v) = last.parse::<usize>()
            {
                return v;
            }
            if let Ok(v) = range_parts[0].parse::<usize>() {
                return v;
            }
        }
    }
    0
}

pub(super) fn parse_word_number(runes: &[char]) -> (i64, i64) {
    let s: String = runes.iter().collect();
    match parse_word_number_from_string(&s) {
        (consumed, val) if consumed > 0 => {
            // `consumed` is a byte offset into s.to_lowercase(); the caller
            // advances a rune index. Map it back through each original
            // char's lowercase byte length (identical for the ASCII words
            // the parser matches, but the contract shouldn't rely on that).
            let mut lower_bytes = 0usize;
            let mut orig_chars = 0i64;
            for c in runes {
                if lower_bytes >= consumed {
                    break;
                }
                lower_bytes += c.to_lowercase().map(char::len_utf8).sum::<usize>();
                orig_chars += 1;
            }
            (val, orig_chars)
        }
        _ => (-1, -1),
    }
}

pub(super) fn consume_name_or_number(runes: &[char], modifier: &str) -> usize {
    if runes.is_empty() {
        return 0;
    }
    // Try weekday name match.
    for name in WEEKDAY_NAMES {
        if modifier == "N"
            || modifier == "n"
            || modifier == "Nn"
            || modifier.starts_with("Nn")
            || modifier.starts_with('N')
        {
            let max_len = parse_name_max_len(modifier);
            let name_runes: Vec<char> = name.chars().collect();
            if max_len > 0 && max_len < name_runes.len() {
                let abbr: String = name_runes[..max_len].iter().collect();
                if runes.len() >= max_len {
                    let s: String = runes[..max_len].iter().collect();
                    if s.to_lowercase() == abbr.to_lowercase() {
                        return max_len;
                    }
                }
            } else if runes.len() >= name_runes.len() {
                let s: String = runes[..name_runes.len()].iter().collect();
                if s.to_lowercase() == name.to_lowercase() {
                    return name_runes.len();
                }
            }
        }
    }
    // Numeric fallback.
    let mut i = 0;
    while i < runes.len() && runes[i].is_ascii_digit() {
        i += 1;
    }
    i
}
