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

#[derive(Clone)]
pub(crate) struct FmtChars {
    pub(crate) decimal_sep: char,
    pub(crate) grouping_sep: char,
    pub(crate) percent: char,
    pub(crate) per_mille: char,
    pub(crate) zero_digit: char,
    pub(crate) digit: char,
    pub(crate) pattern_sep: char,
    pub(crate) exponent_sep: char,
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
            exponent_sep: 'e',
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
            || c == self.exponent_sep
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
fn scan_sub_picture_region<'a>(
    runes: &'a [char],
    fc: &FmtChars,
    sp: &mut SubPicture,
) -> Result<&'a [char], JsonataError> {
    let mut start = 0;
    while start < runes.len() && !fc.is_active_char(runes[start]) {
        start += 1;
    }
    // end is inclusive; use isize arithmetic to handle empty-rune edge case safely.
    let mut end_i = runes.len() as isize - 1;
    while end_i >= 0 && !fc.is_active_char(runes[end_i as usize]) {
        end_i -= 1;
    }
    if start as isize > end_i {
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
    Ok(active)
}

fn locate_sub_picture_separators(
    active: &[char],
    fc: &FmtChars,
    scale: u8,
) -> Result<(Option<usize>, Option<usize>), JsonataError> {
    let mut dec_pos: Option<usize> = None;
    let mut exp_pos: Option<usize> = None;

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
        if c == fc.exponent_sep && exp_pos.is_none() {
            exp_pos = Some(i);
        }
    }

    if let Some(ep) = exp_pos {
        if scale != 0 {
            return Err(JsonataError::new(
                "D3092",
                "$formatNumber: percent/per-mille cannot appear in picture with exponent separator",
            ));
        }
        if active[ep..].contains(&fc.grouping_sep) {
            return Err(JsonataError::new(
                "D3093",
                "$formatNumber: grouping separator cannot appear in exponent",
            ));
        }
    }

    Ok((dec_pos, exp_pos))
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

    let active = scan_sub_picture_region(&runes, fc, &mut sp)?;
    let (dec_pos, exp_pos) = locate_sub_picture_separators(active, fc, sp.scale)?;

    let (int_part, frac_part, exp_part) = match (dec_pos, exp_pos) {
        (Some(d), Some(e)) => {
            if e < d {
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

    format!(
        "{}{}{}{}",
        mantissa_part, fc.exponent_sep, exp_sign, exp_str
    )
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
}
