//! `$formatNumber` — XPath/JSONata picture-string number formatting.
//!
//! Port of Go `functions/string_format_number.go`.

use crate::error::{JsonataError, JsonataResult};
use crate::value::Value;

// ── Public entry point ────────────────────────────────────────────────────────

pub fn fn_format_number(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.len() < 2 {
        return Err(JsonataError::new(
            "D3006",
            "$formatNumber: requires at least 2 arguments",
        ));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let n = match &args[0] {
        Value::Number(f) => *f,
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$formatNumber: argument 1 must be a number",
            ));
        }
    };
    let picture = match &args[1] {
        Value::String(s) => s.clone(),
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$formatNumber: argument 2 must be a string",
            ));
        }
    };

    // Inf/NaN must never reach the picture formatters: they render through
    // `format!("{n:.p$}")` as "inf"/"NaN", which the picture machinery then
    // decorates with separators and digit groups ("inf.00", "NaN.00") — a
    // string that is not a number in any digit family. jsonata-js emits its
    // own junk here ("NaN.00" for infinity), so there is nothing to match;
    // follow the guard $formatInteger already carries (jsntrs-ecq.3) and the
    // one $string uses (string_funcs.rs). JSON input carries no Inf/NaN, but
    // `1/0`, `evaluate_value` and custom-function results all do. The guard
    // sits after the argument type checks so T0410 still wins over D3001.
    if !n.is_finite() {
        return Err(JsonataError::new(
            "D3001",
            "$formatNumber: Number out of range",
        ));
    }

    // Collect options from optional third argument (object).
    let mut opts: Vec<(&str, &str)> = Vec::new();
    if args.len() >= 3
        && let Value::Object(map) = &args[2]
    {
        for (k, v) in map.iter() {
            if let Value::String(s) = v {
                opts.push((k.as_str(), s.as_str()));
            }
        }
    }

    let result = format_number_picture(n, &picture, &opts)?;
    Ok(Value::String(result.into()))
}

// ── Format character set ──────────────────────────────────────────────────────

/// Default `exponent-separator`, used when the option is absent.
const DEFAULT_EXPONENT_SEP: char = 'e';

#[derive(Clone)]
pub(crate) struct FmtChars {
    pub(crate) decimal_sep: char,
    pub(crate) grouping_sep: char,
    pub(crate) percent: char,
    pub(crate) per_mille: char,
    pub(crate) zero_digit: char,
    pub(crate) digit: char,
    pub(crate) pattern_sep: char,
    /// Character treated as the exponent separator, or `None` when no
    /// character can be: jsonata-js compares single picture characters against
    /// the option value, so an empty or multi-character value never matches
    /// and the picture then has no exponent part.
    pub(crate) exponent_sep: Option<char>,
    pub(crate) per_mille_str: String,
}

impl Default for FmtChars {
    fn default() -> Self {
        FmtChars {
            decimal_sep: '.',
            grouping_sep: ',',
            percent: '%',
            per_mille: '\u{2030}', // ‰
            zero_digit: '0',
            digit: '#',
            pattern_sep: ';',
            exponent_sep: Some(DEFAULT_EXPONENT_SEP),
            per_mille_str: "\u{2030}".to_string(),
        }
    }
}

impl FmtChars {
    fn from_opts(opts: &[(&str, &str)]) -> Self {
        let mut fc = FmtChars::default();
        for &(key, val) in opts {
            let chars: Vec<char> = val.chars().collect();
            match key {
                "decimal-separator" if chars.len() == 1 => fc.decimal_sep = chars[0],
                "grouping-separator" if chars.len() == 1 => fc.grouping_sep = chars[0],
                "percent" if chars.len() == 1 => fc.percent = chars[0],
                "per-mille" if !val.is_empty() => {
                    fc.per_mille_str = val.to_string();
                    fc.per_mille = chars[0];
                }
                "zero-digit" if chars.len() == 1 => fc.zero_digit = chars[0],
                "digit" if chars.len() == 1 => fc.digit = chars[0],
                "pattern-separator" if chars.len() == 1 => fc.pattern_sep = chars[0],
                "exponent-separator" => {
                    fc.exponent_sep = if chars.len() == 1 {
                        Some(chars[0])
                    } else {
                        None
                    };
                }
                _ => {}
            }
        }
        fc
    }

    fn is_digit_char(&self, c: char) -> bool {
        c >= self.zero_digit && (c as u32) < (self.zero_digit as u32) + 10
    }

    fn is_active_char(&self, c: char) -> bool {
        self.is_digit_char(c)
            || c == self.digit
            || c == self.grouping_sep
            || c == self.decimal_sep
            || self.exponent_sep == Some(c)
    }

    /// Narrower notion of "active", used only when locating the picture's
    /// first and last active character. jsonata-js excludes the exponent
    /// separator there (`ch !== properties['exponent-separator']`), so a
    /// separator outside the mantissa is passive text: `"e0.0"` has prefix
    /// `"e"`, and the trailing `e` of `"0.0e"` lands in the suffix. The
    /// separator stays active everywhere else — splitting the sub-picture
    /// and emitting the exponent both rely on it.
    fn is_region_edge_char(&self, c: char) -> bool {
        self.is_active_char(c) && self.exponent_sep != Some(c)
    }
}

// ── Sub-picture ───────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
pub(crate) struct SubPicture {
    pub(crate) prefix: String,
    pub(crate) suffix: String,
    pub(crate) int_mandatory: usize,
    pub(crate) int_optional: usize,
    pub(crate) frac_mandatory: usize,
    pub(crate) frac_optional: usize,
    pub(crate) exp_mandatory: usize,
    pub(crate) exp_min_width: usize,
    /// 0=none, 1=percent, 2=per-mille
    pub(crate) scale: u8,
    pub(crate) int_grp_pos: Vec<usize>,
    pub(crate) frac_grp_pos: Vec<usize>,
    pub(crate) has_decimal: bool,
    pub(crate) has_any_int_digit: bool,
}

// ── Parsing helpers ───────────────────────────────────────────────────────────

fn contains_scaling(s: &[char], fc: &FmtChars) -> Result<u8, JsonataError> {
    let pct = s.iter().filter(|&&c| c == fc.percent).count();
    let pm = s.iter().filter(|&&c| c == fc.per_mille).count();
    if pct > 1 {
        return Err(JsonataError::new(
            "D3082",
            "$formatNumber: picture has more than one percent character",
        ));
    }
    if pm > 1 {
        return Err(JsonataError::new(
            "D3083",
            "$formatNumber: picture has more than one per-mille character",
        ));
    }
    if pct > 0 && pm > 0 {
        return Err(JsonataError::new(
            "D3084",
            "$formatNumber: picture has both percent and per-mille characters",
        ));
    }
    if pct > 0 {
        return Ok(1);
    }
    if pm > 0 {
        return Ok(2);
    }
    Ok(0)
}

/// Find the active region (between first and last active char), extract prefix/suffix,
/// validate internal chars, determine scale.
///
/// Returns the active slice and its offset in `runes`; the offset is where the
/// search for the exponent separator begins (jsonata-js searches from
/// `prefix.length`).
fn scan_sub_picture_region<'a>(
    runes: &'a [char],
    fc: &FmtChars,
    sp: &mut SubPicture,
) -> Result<(&'a [char], usize), JsonataError> {
    let mut start = 0;
    while start < runes.len() && !fc.is_region_edge_char(runes[start]) {
        start += 1;
    }
    // end is inclusive; use isize arithmetic to handle empty-rune edge case safely.
    let mut end_i = runes.len() as isize - 1;
    while end_i >= 0 && !fc.is_region_edge_char(runes[end_i as usize]) {
        end_i -= 1;
    }
    if start as isize > end_i {
        // A picture made only of exponent separators and passive characters
        // ("e") leaves jsonata-js dereferencing an undefined prefix and
        // throwing a TypeError; D3085 is the honest diagnosis and is what
        // jsntrs has always answered here.
        return Err(JsonataError::new(
            "D3085",
            "$formatNumber: picture has no digit or separator characters",
        ));
    }
    let end = end_i as usize;

    sp.prefix = runes[..start].iter().collect();
    sp.suffix = runes[end + 1..].iter().collect();
    let active = &runes[start..=end];

    for &c in active {
        if !fc.is_active_char(c) && c != fc.percent && c != fc.per_mille {
            return Err(JsonataError::new(
                "D3086",
                "$formatNumber: invalid character in active picture region",
            ));
        }
    }

    sp.scale = contains_scaling(runes, fc)?;
    Ok((active, start))
}

/// Locate the exponent separator, as an index into the active slice.
///
/// The search starts at the prefix boundary, so a separator the prefix scan
/// stepped over ("e0.0") is not picked up again. A separator sitting
/// immediately *after* the active region (index `active.len()`, i.e. the
/// first character of the suffix) still introduces an exponent part — an
/// empty one, which `locate_sub_picture_separators` rejects. That is what
/// makes `"0.0e"` a D3093 rather than a literal `e` suffix.
fn locate_exponent(
    runes: &[char],
    start: usize,
    active_len: usize,
    fc: &FmtChars,
) -> Option<usize> {
    let sep = fc.exponent_sep?;
    let rel = runes[start..].iter().position(|&c| c == sep)?;
    (rel <= active_len).then_some(rel)
}

fn locate_sub_picture_separators(
    active: &[char],
    exp_pos: Option<usize>,
    fc: &FmtChars,
    scale: u8,
) -> Result<Option<usize>, JsonataError> {
    let mut dec_pos: Option<usize> = None;

    for (i, &c) in active.iter().enumerate() {
        if c == fc.decimal_sep {
            if dec_pos.is_some() {
                return Err(JsonataError::new(
                    "D3081",
                    "$formatNumber: picture has more than one decimal separator",
                ));
            }
            dec_pos = Some(i);
        }
    }

    if let Some(ep) = exp_pos {
        // `ep` may be one past the active region, and it may be the last
        // active character; either way the exponent part is empty, which
        // jsonata-js reports as D3093 (the exponent part must comprise one
        // or more digit-family characters).
        let exp_part = active.get(ep + 1..).unwrap_or(&[]);
        if exp_part.is_empty() {
            return Err(JsonataError::new(
                "D3093",
                "$formatNumber: exponent part must contain at least one digit",
            ));
        }
        if scale != 0 {
            return Err(JsonataError::new(
                "D3092",
                "$formatNumber: percent/per-mille cannot appear in picture with exponent separator",
            ));
        }
        if exp_part.contains(&fc.grouping_sep) {
            return Err(JsonataError::new(
                "D3093",
                "$formatNumber: grouping separator cannot appear in exponent",
            ));
        }
    }

    Ok(dec_pos)
}

fn parse_int_part(
    int_part: &[char],
    fc: &FmtChars,
    sp: &mut SubPicture,
) -> Result<(), JsonataError> {
    let mut last_was_group = false;
    let mut seen_mandatory = false;

    for (i, &c) in int_part.iter().enumerate() {
        if fc.is_digit_char(c) {
            seen_mandatory = true;
            last_was_group = false;
            sp.int_mandatory += 1;
            sp.has_any_int_digit = true;
        } else if c == fc.digit {
            if seen_mandatory {
                return Err(JsonataError::new(
                    "D3090",
                    "$formatNumber: optional digit cannot follow mandatory digit in integer part",
                ));
            }
            last_was_group = false;
            sp.int_optional += 1;
            sp.has_any_int_digit = true;
        } else if c == fc.grouping_sep {
            if last_was_group {
                return Err(JsonataError::new(
                    "D3089",
                    "$formatNumber: adjacent grouping separators in picture",
                ));
            }
            if i == int_part.len() - 1 {
                if sp.has_decimal {
                    return Err(JsonataError::new(
                        "D3087",
                        "$formatNumber: grouping separator adjacent to decimal separator",
                    ));
                }
                return Err(JsonataError::new(
                    "D3088",
                    "$formatNumber: grouping separator at end of integer part",
                ));
            }
            last_was_group = true;
        } else if c == fc.percent || c == fc.per_mille {
            last_was_group = false;
        }
    }

    // Compute grouping positions (from the right).
    let mut int_digit_count_from_right: usize = 0;
    for i in (0..int_part.len()).rev() {
        let c = int_part[i];
        if fc.is_digit_char(c) || c == fc.digit {
            int_digit_count_from_right += 1;
        } else if c == fc.grouping_sep {
            sp.int_grp_pos.push(int_digit_count_from_right);
        }
    }

    Ok(())
}

fn parse_frac_part(
    frac_part: &[char],
    fc: &FmtChars,
    sp: &mut SubPicture,
) -> Result<(), JsonataError> {
    let mut seen_optional = false;
    let mut frac_digit_count: usize = 0;

    for &c in frac_part {
        if fc.is_digit_char(c) {
            if seen_optional {
                return Err(JsonataError::new(
                    "D3091",
                    "$formatNumber: mandatory digit cannot follow optional digit in fraction part",
                ));
            }
            frac_digit_count += 1;
            sp.frac_mandatory += 1;
        } else if c == fc.digit {
            seen_optional = true;
            frac_digit_count += 1;
            sp.frac_optional += 1;
        } else if c == fc.grouping_sep {
            sp.frac_grp_pos.push(frac_digit_count);
        }
    }

    Ok(())
}

pub(crate) fn parse_sub_picture(pic: &str, fc: &FmtChars) -> Result<SubPicture, JsonataError> {
    let runes: Vec<char> = pic.chars().collect();
    let mut sp = SubPicture::default();

    let (active, start) = scan_sub_picture_region(&runes, fc, &mut sp)?;
    let exp_pos = locate_exponent(&runes, start, active.len(), fc);
    let dec_pos = locate_sub_picture_separators(active, exp_pos, fc, sp.scale)?;

    // The exponent part was validated as non-empty above, so `e` indexes the
    // active slice with at least one character behind it.
    let (int_part, frac_part, exp_part) = match (dec_pos, exp_pos) {
        (Some(d), Some(e)) => {
            // `e == d` when one character is configured as both separators;
            // splitting the mantissa there has no meaning. jsntrs used to
            // panic on the empty slice range this produced.
            if e <= d {
                return Err(JsonataError::new("D3085", "$formatNumber: invalid picture"));
            }
            (&active[..d], &active[d + 1..e], &active[e + 1..])
        }
        (Some(d), None) => (&active[..d], &active[d + 1..], &active[0..0]),
        (None, Some(e)) => (&active[..e], &active[0..0], &active[e + 1..]),
        (None, None) => (active, &active[0..0], &active[0..0]),
    };

    if dec_pos.is_some() {
        sp.has_decimal = true;
    }

    parse_int_part(int_part, fc, &mut sp)?;

    let has_frac_digit = frac_part
        .iter()
        .any(|&c| fc.is_digit_char(c) || c == fc.digit);
    if !sp.has_any_int_digit && !has_frac_digit && (dec_pos.is_some() || exp_pos.is_some()) {
        return Err(JsonataError::new(
            "D3085",
            "$formatNumber: picture has no digit placeholders in mantissa",
        ));
    }

    parse_frac_part(frac_part, fc, &mut sp)?;

    for &c in exp_part {
        if fc.is_digit_char(c) || c == fc.digit {
            sp.exp_mandatory += 1;
        }
    }
    sp.exp_min_width = sp.exp_mandatory;

    Ok(sp)
}

// ── Formatting helpers ────────────────────────────────────────────────────────

fn compute_int_group_positions(grp_pos: &[usize], int_len: usize) -> Vec<usize> {
    if grp_pos.is_empty() {
        return Vec::new();
    }
    let primary = grp_pos[0];
    let all_equal = grp_pos.windows(2).all(|w| w[1] - w[0] == primary);
    let mut result = Vec::new();
    if grp_pos.len() == 1 || all_equal {
        let mut pos = primary;
        while pos < int_len {
            result.push(pos);
            pos += primary;
        }
    } else {
        result.extend_from_slice(grp_pos);
    }
    result.sort_unstable();
    result
}

pub(crate) fn apply_digit_family(s: &str, zero_digit: char) -> String {
    if zero_digit == '0' {
        return s.to_string();
    }
    s.chars()
        .map(|c| {
            if c.is_ascii_digit() {
                char::from_u32(zero_digit as u32 + (c as u32 - '0' as u32)).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

fn apply_int_grouping(int_str: &str, grp_pos: &[usize], sep: char) -> String {
    let runes: Vec<char> = int_str.chars().collect();
    let group_positions = compute_int_group_positions(grp_pos, runes.len());
    if group_positions.is_empty() {
        return int_str.to_string();
    }
    let mut result = String::with_capacity(int_str.len() + group_positions.len());
    for (i, &c) in runes.iter().enumerate() {
        let pos_from_right = runes.len() - i;
        if group_positions.binary_search(&pos_from_right).is_ok() {
            result.push(sep);
        }
        result.push(c);
    }
    result
}

fn apply_frac_grouping(frac_str: &str, grp_pos: &[usize], sep: char) -> String {
    let runes: Vec<char> = frac_str.chars().collect();
    let mut result = String::with_capacity(frac_str.len() + grp_pos.len());
    for (i, &c) in runes.iter().enumerate() {
        result.push(c);
        if grp_pos.contains(&(i + 1)) && i + 1 < runes.len() {
            result.push(sep);
        }
    }
    result
}

pub(crate) fn format_fixed(n: f64, sp: &SubPicture, fc: &FmtChars) -> String {
    let total_frac_digits = sp.frac_mandatory + sp.frac_optional;
    let formatted = format!("{n:.total_frac_digits$}");
    let mut parts = formatted.splitn(2, '.');
    let mut int_str = parts.next().unwrap_or("").to_string();
    let mut frac_str = parts.next().unwrap_or("").to_string();

    // Minimum integer digits: force at least 1 when there's no decimal and no
    // digit placeholder, or when only optional-digit placeholders appear.
    let min_int = if sp.int_mandatory < 1 {
        usize::from((!sp.has_decimal && !sp.has_any_int_digit) || sp.int_optional > 0)
    } else {
        sp.int_mandatory
    };
    while int_str.len() < min_int {
        int_str.insert(0, '0');
    }

    if !sp.int_grp_pos.is_empty() {
        int_str = apply_int_grouping(&int_str, &sp.int_grp_pos, fc.grouping_sep);
    }

    // Trim optional trailing zeros.
    if sp.frac_optional > 0 && frac_str.len() > sp.frac_mandatory {
        let trimmed = frac_str.trim_end_matches('0');
        if trimmed.len() < sp.frac_mandatory {
            frac_str = frac_str[..sp.frac_mandatory].to_string();
        } else {
            frac_str = trimmed.to_string();
        }
    }
    while frac_str.len() < sp.frac_mandatory {
        frac_str.push('0');
    }

    if !sp.frac_grp_pos.is_empty() && !frac_str.is_empty() {
        frac_str = apply_frac_grouping(&frac_str, &sp.frac_grp_pos, fc.grouping_sep);
    }

    if !frac_str.is_empty() || sp.has_decimal {
        format!("{}{}{}", int_str, fc.decimal_sep, frac_str)
    } else {
        int_str
    }
}

pub(crate) fn format_with_exponent(n: f64, sp: &SubPicture, fc: &FmtChars) -> String {
    let cap_n = sp.int_mandatory;
    let mut frac_sig = sp.frac_mandatory + sp.frac_optional;
    if cap_n == 0 && sp.frac_mandatory == 0 && sp.frac_optional == 0 {
        frac_sig += sp.int_optional;
    }

    let mut exp: i32 = 0;
    if n != 0.0 {
        let log_val = n.abs().log10().floor() as i32;
        if cap_n > 0 {
            exp = log_val - (cap_n as i32 - 1);
        } else {
            exp = log_val + 1;
        }
    }
    let mut mantissa = n / 10f64.powi(exp);

    let factor = 10f64.powi(frac_sig as i32);
    mantissa = (mantissa * factor).round() / factor;

    let threshold = if cap_n > 0 {
        10f64.powi(cap_n as i32)
    } else {
        1.0
    };
    if mantissa.abs() >= threshold {
        mantissa /= 10.0;
        exp += 1;
    }

    let mantissa_formatted = format!("{:.prec$}", mantissa.abs(), prec = frac_sig);
    let mut parts = mantissa_formatted.splitn(2, '.');
    let mut int_str = parts.next().unwrap_or("").to_string();
    let mut frac_str = parts.next().unwrap_or("").to_string();

    while int_str.len() < sp.int_mandatory {
        int_str.insert(0, '0');
    }
    if sp.int_mandatory == 0 && sp.int_optional > 0 && (int_str.is_empty() || int_str == "0") {
        int_str = "0".to_string();
    }

    if sp.frac_optional > 0 && frac_str.len() > sp.frac_mandatory {
        let trimmed = frac_str.trim_end_matches('0');
        if trimmed.len() < sp.frac_mandatory {
            frac_str = frac_str[..sp.frac_mandatory].to_string();
        } else {
            frac_str = trimmed.to_string();
        }
    }

    let mantissa_part = if sp.has_any_int_digit || sp.int_mandatory > 0 {
        if !frac_str.is_empty() || sp.has_decimal {
            format!("{}{}{}", int_str, fc.decimal_sep, frac_str)
        } else {
            int_str
        }
    } else if !frac_str.is_empty() {
        format!("{}{}", fc.decimal_sep, frac_str)
    } else {
        String::new()
    };

    let (exp_sign, exp_abs) = if exp < 0 {
        ("-", -exp as usize)
    } else {
        ("", exp as usize)
    };
    let mut exp_str = exp_abs.to_string();
    while exp_str.len() < sp.exp_min_width {
        exp_str.insert(0, '0');
    }

    // Only reached when the picture had an exponent part, which implies a
    // separator character was configured; the fallback keeps this total.
    let exp_sep = fc.exponent_sep.unwrap_or(DEFAULT_EXPONENT_SEP);
    format!("{mantissa_part}{exp_sep}{exp_sign}{exp_str}")
}

pub(crate) fn split_on_pattern_sep(picture: &str, sep: char) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in picture.chars() {
        if c == sep {
            parts.push(cur);
            cur = String::new();
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    parts
}

fn format_number_picture(
    n: f64,
    picture: &str,
    opts: &[(&str, &str)],
) -> Result<String, JsonataError> {
    let fc = FmtChars::from_opts(opts);

    let pics = split_on_pattern_sep(picture, fc.pattern_sep);
    if pics.len() > 2 {
        return Err(JsonataError::new(
            "D3080",
            "$formatNumber: picture has more than one pattern separator",
        ));
    }

    let pos_pic = parse_sub_picture(&pics[0], &fc)?;

    let neg_pic = if pics.len() == 2 {
        parse_sub_picture(&pics[1], &fc)?
    } else {
        let mut np = pos_pic.clone();
        np.prefix = format!("-{}", pos_pic.prefix);
        np
    };

    let negative = n < 0.0;
    let sp = if negative { &neg_pic } else { &pos_pic };
    let mut value = if negative { -n } else { n };

    match sp.scale {
        1 => value *= 100.0,
        2 => value *= 1000.0,
        _ => {}
    }

    let inner = if sp.exp_mandatory > 0 {
        format_with_exponent(value, sp, &fc)
    } else {
        format_fixed(value, sp, &fc)
    };

    let inner = apply_digit_family(&inner, fc.zero_digit);
    Ok(format!("{}{}{}", sp.prefix, inner, sp.suffix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    fn fmt(n: f64, picture: &str) -> String {
        match fn_format_number(
            &[Value::Number(n), Value::String(picture.into())],
            &Value::Undefined,
        ) {
            Ok(Value::String(s)) => s.to_string(),
            other => panic!("expected string, got {other:?}"),
        }
    }

    /// Expected values are taken from the JSONata documentation examples
    /// for $formatNumber (XPath F&O picture strings).
    #[test]
    fn format_number_matches_jsonata_documentation_examples() {
        assert_eq!(fmt(12345.6, "#,###.00"), "12,345.60");
        assert_eq!(fmt(1234.5678, "00.000e0"), "12.346e2");
        assert_eq!(fmt(0.14, "01%"), "14%");
        assert_eq!(fmt(1234.5678, "#,##0.00"), "1,234.57");
    }

    /// The second sub-picture formats negative numbers.
    #[test]
    fn negative_sub_picture_is_applied() {
        assert_eq!(fmt(-1.0, "#0.00;(#0.00)"), "(1.00)");
        assert_eq!(fmt(1.0, "#0.00;(#0.00)"), "1.00");
    }

    /// Call the builtin with arbitrary arguments; `Err` carries the code.
    fn fmt_args(args: &[Value]) -> Result<String, &'static str> {
        match fn_format_number(args, &Value::Undefined) {
            Ok(Value::String(s)) => Ok(s.to_string()),
            Ok(other) => panic!("expected string, got {other:?}"),
            Err(e) => Err(e.code),
        }
    }

    /// Non-finite input is rejected before any picture processing
    /// (jsntrs-p0v.13). Before the guard the formatters rendered `inf`/`NaN`
    /// and the picture machinery decorated it: `1/0` with `"0.00"` gave
    /// "inf.00", `0/0` with `"#,##0.00"` gave "NaN.00".
    #[test]
    fn non_finite_errors_for_every_picture() {
        let pictures = ["0.00", "#,##0.00", "0.0e0", "#0.0;(#0.0)", "01%", "###"];
        for picture in pictures {
            for n in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
                assert_eq!(
                    fmt_args(&[Value::Number(n), Value::String(picture.into())]),
                    Err("D3001"),
                    "picture {picture:?}, input {n}"
                );
            }
        }
    }

    /// Argument-type errors are diagnosed before the range guard, so a
    /// non-finite number with a bad picture still reports T0410.
    #[test]
    fn argument_type_errors_win_over_the_range_guard() {
        assert_eq!(
            fmt_args(&[Value::Number(f64::INFINITY), Value::Number(5.0)]),
            Err("T0410")
        );
        assert_eq!(
            fmt_args(&[Value::String("x".into()), Value::String("0.00".into())]),
            Err("T0410")
        );
        // Undefined still propagates ahead of everything else.
        let propagated = fn_format_number(
            &[Value::Undefined, Value::String("0.00".into())],
            &Value::Undefined,
        );
        assert!(matches!(propagated, Ok(Value::Undefined)));
    }

    /// The same guard through the public API: `evaluate_value` accepts a
    /// `Value` built in Rust, so Inf/NaN reach the builtin without going
    /// through JSON (which cannot represent them); `1/0` and `0/0` inside an
    /// expression are the other route.
    #[test]
    fn non_finite_errors_through_evaluate_value() {
        for picture in ["0.00", "#,##0.00", "0.0e0"] {
            let src = format!("$formatNumber($, '{picture}')");
            let expr = crate::expression::Expression::compile(&src).unwrap();
            for n in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
                let err = expr.evaluate_value(&Value::Number(n)).unwrap_err();
                assert_eq!(err.code, "D3001", "picture {picture:?}, input {n}");
            }

            for src in [
                format!("$formatNumber(1/0, '{picture}')"),
                format!("$formatNumber(-1/0, '{picture}')"),
                format!("$formatNumber(0/0, '{picture}')"),
            ] {
                let expr = crate::expression::Expression::compile(&src).unwrap();
                assert_eq!(
                    expr.evaluate_value(&Value::Undefined).unwrap_err().code,
                    "D3001",
                    "{src}"
                );
            }
        }
    }

    /// Format with an options object; `Err` carries the error code.
    fn fmt_opts(n: f64, picture: &str, options: &str) -> Result<String, &'static str> {
        let opts = Value::from_json_str(options).expect("options fixture must be valid JSON");
        match fn_format_number(
            &[Value::Number(n), Value::String(picture.into()), opts],
            &Value::Undefined,
        ) {
            Ok(Value::String(s)) => Ok(s.to_string()),
            Ok(other) => panic!("expected string, got {other:?}"),
            Err(e) => Err(e.code),
        }
    }

    /// Expected values verified against jsonata-js 2.x: `exponent-separator`
    /// picks the picture character that introduces the exponent and the
    /// character emitted in its place, and the default `e` is passive once
    /// another separator is configured.
    #[test]
    fn exponent_separator_option_is_honoured() {
        let e_sep = r#"{"exponent-separator": "E"}"#;
        assert_eq!(fmt_opts(1234.5678, "0.0E0", e_sep), Ok("1.2E3".to_string()));
        assert_eq!(
            fmt_opts(1234.5678, "00.000E0", e_sep),
            Ok("12.346E2".to_string())
        );
        assert_eq!(
            fmt_opts(0.000_012_345, "0.00E0", e_sep),
            Ok("1.23E-5".to_string())
        );
        // With a custom separator the default `e` is a passive character
        // sandwiched between active ones.
        assert_eq!(fmt_opts(1234.5678, "0.0e0", e_sep), Err("D3086"));
        // A multi-character value matches no single picture character, so the
        // picture has no exponent separator at all (jsonata-js agrees).
        assert_eq!(
            fmt_opts(1234.5678, "0.0e0", r#"{"exponent-separator": "EE"}"#),
            Err("D3086")
        );
        // Unrecognised keys are ignored, as in jsonata-js.
        assert_eq!(
            fmt_opts(1234.5678, "0.0e0", r#"{"bogus": "x"}"#),
            Ok("1.2e3".to_string())
        );
    }

    /// Expected values verified against jsonata-js 2.x (jsntrs-p0v.14): the
    /// prefix/suffix scan ignores the exponent separator, so a separator
    /// before the mantissa is passive text and one after it introduces an
    /// empty — and therefore invalid — exponent part. Before the fix the
    /// scan treated it as active: `"e0.0"` was D3085 and `"0.0e"` formatted
    /// as "1234.6".
    #[test]
    fn exponent_separator_is_passive_in_the_prefix_suffix_scan() {
        assert_eq!(fmt(1234.5678, "e0.0"), "e1234.6");
        assert_eq!(fmt(0.234, "e0.0"), "e0.2");
        assert_eq!(fmt(1234.5678, "ee0.0"), "ee1234.6");
        // Still active inside the mantissa, and still emitted.
        assert_eq!(fmt(1234.5678, "0.0e0"), "1.2e3");
        assert_eq!(fmt(1234.5678, "0.0e0e"), "1.2e3e");
        // A trailing separator leaves the exponent part empty.
        assert_eq!(
            fmt_args(&[Value::Number(1234.5678), Value::String("0.0e".into())]),
            Err("D3093")
        );
        assert_eq!(
            fmt_args(&[Value::Number(1234.5678), Value::String("e0.0e".into())]),
            Err("D3093")
        );
        assert_eq!(
            fmt_args(&[Value::Number(1234.5678), Value::String("0e".into())]),
            Err("D3093")
        );
        // A picture of nothing but separators has no mantissa at all.
        assert_eq!(
            fmt_args(&[Value::Number(1234.5678), Value::String("e".into())]),
            Err("D3085")
        );

        // The same rules follow a custom exponent-separator, and the default
        // `e` becomes ordinary passive text.
        let e_sep = r#"{"exponent-separator": "E"}"#;
        assert_eq!(
            fmt_opts(1234.5678, "E0.0", e_sep),
            Ok("E1234.6".to_string())
        );
        assert_eq!(fmt_opts(1234.5678, "0.0E", e_sep), Err("D3093"));
        assert_eq!(
            fmt_opts(1234.5678, "e0.0", e_sep),
            Ok("e1234.6".to_string())
        );
    }

    /// jsntrs used to panic ("slice index starts at 2 but ends at 1") when
    /// one character was configured as both the decimal and the exponent
    /// separator, because the mantissa split ran backwards. It is a picture
    /// error now. jsonata-js answers "1.3" here; matching that needs the
    /// mantissa/exponent split reworked, which is left for a follow-up.
    #[test]
    fn separator_that_is_both_decimal_and_exponent_is_an_error() {
        let both = r#"{"exponent-separator": "."}"#;
        assert_eq!(fmt_opts(1234.5678, "0.0", both), Err("D3085"));
        assert_eq!(fmt_opts(1234.5678, "0.00", both), Err("D3085"));
    }
}
