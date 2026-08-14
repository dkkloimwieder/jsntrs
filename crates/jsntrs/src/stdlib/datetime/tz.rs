//! Timezone offset parsing and formatting.

use crate::error::JsonataError;

pub(super) fn format_timezone(
    component: char,
    modifier: &str,
    tz_offset_secs: i32,
) -> Result<String, JsonataError> {
    let use_z = modifier.ends_with('t');
    let mod_ = if use_z {
        &modifier[..modifier.len() - 1]
    } else {
        modifier
    };

    let prefix = if component == 'z' { "GMT" } else { "" };

    if tz_offset_secs == 0 && use_z {
        return Ok(format!("{prefix}Z"));
    }

    let sign = if tz_offset_secs >= 0 { '+' } else { '-' };
    let abs_secs = tz_offset_secs.unsigned_abs() as i32;
    let hours = abs_secs / 3600;
    let mins = (abs_secs % 3600) / 60;

    let s = match mod_ {
        "0" => {
            if mins == 0 {
                format!("{prefix}{sign}{hours}")
            } else {
                format!("{prefix}{sign}{hours:}:{mins:02}")
            }
        }
        "0101" => format!("{prefix}{sign}{hours:02}{mins:02}"),
        "01:01" | "" | "Z" => format!("{prefix}{sign}{hours:02}:{mins:02}"),
        "010101" | "01:01:01" => {
            return Err(JsonataError::new(
                "D3134",
                format!("invalid picture component: [{component}{modifier}]"),
            ));
        }
        _ => format!("{prefix}{sign}{hours:02}:{mins:02}"),
    };
    Ok(s)
}

pub(super) fn parse_tz_from_input(runes: &[char], component: char) -> (i32, usize) {
    if runes.is_empty() {
        return (0, 0);
    }
    let s: String = runes.iter().collect();

    // Handle GMT prefix (z component).
    if (component == 'z' || s.starts_with("GMT")) && s.starts_with("GMT") {
        let rest: Vec<char> = runes[3..].to_vec();
        let (offset, n) = parse_tz_from_input(&rest, 'Z');
        return (offset, 3 + n);
    }

    if runes[0] == 'Z' {
        return (0, 1);
    }

    let sign: i32 = match runes[0] {
        '+' => 1,
        '-' => -1,
        _ => return (0, 0),
    };
    let mut i = 1;

    let h_start = i;
    while i < runes.len() && runes[i].is_ascii_digit() && i - h_start < 2 {
        i += 1;
    }
    if i == h_start {
        return (0, 0);
    }
    let hours: i32 = runes[h_start..i]
        .iter()
        .collect::<String>()
        .parse()
        .unwrap_or(0);

    // Optional colon.
    if i < runes.len() && runes[i] == ':' {
        i += 1;
    }

    let m_start = i;
    while i < runes.len() && runes[i].is_ascii_digit() && i - m_start < 2 {
        i += 1;
    }
    let mins: i32 = if i > m_start {
        runes[m_start..i]
            .iter()
            .collect::<String>()
            .parse()
            .unwrap_or(0)
    } else {
        0
    };

    (sign * (hours * 3600 + mins * 60), i)
}

// ── Timezone parsing ─────────────────────────────────────────────────────────

/// Parse a `$now`/`$fromMillis` timezone argument into an offset in seconds.
///
/// Accepts a numeric offset (`"+0530"`, `"-05:00"`, `"0530"`) and the three
/// zero-offset spellings `"UTC"`, `"GMT"` and `"Z"`. IANA zone names
/// (`"America/New_York"`) are **not** supported and never will be: the
/// datetime layer is hand-rolled calendar math with no timezone database,
/// so there is nothing to resolve a name against. Such an argument gets a
/// D3137 naming the accepted forms.
///
/// jsonata-js parses this argument as `parseInt(timezone)` and does not
/// error at all: every name yields `NaN` and formats as the literal text
/// `"NaN"` (`$fromMillis(0, "[Y]", "UTC")` is `"NaN"` there), and a
/// colon-separated offset silently loses its minutes (`"+05:30"` parses as
/// `+5`, then `5 / 100 = 0` hours). jsntrs deliberately reads the whole
/// offset and reports the unusable input (jsntrs-p0v.5).
pub(super) fn parse_tz(s: &str) -> Result<i32, JsonataError> {
    if s.eq_ignore_ascii_case("UTC") || s.eq_ignore_ascii_case("GMT") || s == "Z" {
        return Ok(0);
    }
    parse_numeric_tz(s).map_err(|_| {
        JsonataError::new(
            "D3137",
            format!(
                "unknown timezone {s:?}: expected a numeric offset such as \"+0530\" or \"-05:00\", \
                 or \"UTC\"/\"GMT\"/\"Z\" (there is no IANA timezone database)"
            ),
        )
    })
}

pub(super) fn parse_numeric_tz(s: &str) -> Result<i32, String> {
    if s.is_empty() {
        return Err("empty".into());
    }
    let (sign, rest) = match s.as_bytes()[0] {
        b'+' => (1i32, &s[1..]),
        b'-' => (-1i32, &s[1..]),
        _ => {
            // Try "0000" (treat as positive).
            if s.chars().all(|c| c.is_ascii_digit() || c == ':') {
                (1i32, s)
            } else {
                return Err(format!("bad tz: {s}"));
            }
        }
    };
    let rest = rest.replace(':', "");
    // The length check counts bytes, so "a€" (1+3 bytes) would pass and the
    // fixed slices below would split the multi-byte char — require ASCII.
    if rest.len() != 4 || !rest.is_ascii() {
        return Err(format!("bad tz len: {rest}"));
    }
    let h: i32 = rest[..2].parse().map_err(|_| "bad h".to_string())?;
    let m: i32 = rest[2..].parse().map_err(|_| "bad m".to_string())?;
    Ok(sign * (h * 3600 + m * 60))
}
