//! `$formatNumber` — XPath/JSONata picture-string number formatting.
//!
//! Port of Go `functions/string_format_number.go`.

use compact_str::CompactString;

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
    /// Scaling markers, matched as *strings*. jsonata-js never compares a
    /// picture character to these: it asks whether the sub-picture *contains*
    /// the option's value (`subpicture.indexOf(properties.percent)`), and
    /// scales by 100 or 1000 when it does. So a multi-character value matches
    /// only a run of that many characters — `{"percent": "0a"}` leaves an
    /// ordinary `%` as passive suffix text but scales `"00a"` — and an empty
    /// value matches everywhere, which is why `{"per-mille": ""}` makes every
    /// non-empty picture a D3083. jsntrs used to take the first character of
    /// the value, so a multi-character one broke pictures that never
    /// mentioned it: `$formatNumber(7, "00", {"per-mille": "0a"})` was D3083
    /// (jsntrs-p0v.27).
    pub(crate) percent: CompactString,
    pub(crate) per_mille: CompactString,
    pub(crate) zero_digit: char,
    pub(crate) digit: char,
    /// Split on as a string, like `String.prototype.split`: a
    /// multi-character value splits on the whole run (and leaves `;` an
    /// ordinary passive character), and an empty one splits between every
    /// character, so `{"pattern-separator": ""}` makes `"000"` three
    /// sub-pictures and a D3080.
    pub(crate) pattern_sep: CompactString,
    /// Character treated as the exponent separator, or `None` when no
    /// character can be: jsonata-js compares single picture characters against
    /// the option value, so an empty or multi-character value never matches
    /// and the picture then has no exponent part.
    ///
    /// This one stays a character. jsonata-js locates it with
    /// `subpicture.indexOf(...)` like the scaling markers, but then *emits* it
    /// and slices the mantissa at its index, so the string behaviour is
    /// entangled with the off-by-prefix bug `locate_exponent` documents: an
    /// empty value matches at the prefix boundary and makes every picture an
    /// empty-mantissa error there. jsntrs keeps "no such separator" instead
    /// (jsntrs-p0v.27).
    pub(crate) exponent_sep: Option<char>,
}

impl Default for FmtChars {
    fn default() -> Self {
        FmtChars {
            decimal_sep: '.',
            grouping_sep: ',',
            percent: CompactString::const_new("%"),
            per_mille: CompactString::const_new("\u{2030}"), // ‰
            zero_digit: '0',
            digit: '#',
            pattern_sep: CompactString::const_new(";"),
            exponent_sep: Some(DEFAULT_EXPONENT_SEP),
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
                "percent" => fc.percent = CompactString::new(val),
                "per-mille" => fc.per_mille = CompactString::new(val),
                "zero-digit" if chars.len() == 1 => fc.zero_digit = chars[0],
                "digit" if chars.len() == 1 => fc.digit = chars[0],
                "pattern-separator" => fc.pattern_sep = CompactString::new(val),
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

fn count_char(hay: &[char], needle: char) -> usize {
    hay.iter().filter(|&&c| c == needle).count()
}

/// Distinct positions at which `needle` occurs in `hay`.
///
/// The rules that use this ask only whether there are none, one, or more than
/// one — jsonata-js phrases "more than one instance" as
/// `indexOf(x) !== lastIndexOf(x)`. An empty needle matches at every position
/// including one past the end, exactly as `String.prototype.indexOf` reports
/// it, so an empty `percent` or `per-mille` option is "more than one instance"
/// for every non-empty picture.
fn count_occurrences(hay: &[char], needle: &str) -> usize {
    let len = needle.chars().count();
    let Some(last_start) = hay.len().checked_sub(len) else {
        return 0;
    };
    (0..=last_start)
        .filter(|&i| hay[i..i + len].iter().copied().eq(needle.chars()))
        .count()
}

/// 0 = no scaling, 1 = percent, 2 = per-mille.
///
/// Only reached once validation has ruled out a picture carrying both.
fn scaling_factor(picture: &[char], fc: &FmtChars) -> u8 {
    if count_occurrences(picture, &fc.percent) > 0 {
        1
    } else if count_occurrences(picture, &fc.per_mille) > 0 {
        2
    } else {
        0
    }
}

/// Find the active region (between first and last active char) and extract
/// prefix/suffix.
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
    Ok((&runes[start..=end], start))
}

/// Locate the exponent separator, as an index into the active slice.
///
/// The search starts at the prefix boundary, so a separator the prefix scan
/// stepped over ("e0.0") is not picked up again. A separator sitting
/// immediately *after* the active region (index `active.len()`, i.e. the
/// first character of the suffix) still introduces an exponent part — an
/// empty one, which `validate_sub_picture` rejects. That is what makes
/// `"0.0e"` a D3093 rather than a literal `e` suffix.
///
/// The index is relative to the active region, which is where the mantissa
/// and exponent split. jsonata-js instead keeps the separator's index in the
/// whole sub-picture and then slices the *active part* with it, so every
/// picture with both a prefix and an exponent splits at the wrong offset
/// there: `"$0.0e0"` is a spurious D3093 (its exponent part comes out empty),
/// and `"-0E.0"` with `exponent-separator` `E` is accepted, the `.` having
/// been swallowed into the mantissa. jsntrs stays XPath-correct and does not
/// replicate the off-by-prefix bug — a deliberate deviation, like the D3085
/// answer above (jsntrs-p0v.23).
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

/// A sub-picture cut into the regions the F&O 4.7.3 rules are phrased over.
struct SubPictureParts<'a> {
    /// The whole sub-picture. Several rules search this rather than the
    /// active region: jsonata-js applies them to `subpicture` directly, so a
    /// grouping separator in the *suffix* still trips D3087/D3089.
    picture: &'a [char],
    active: &'a [char],
    mantissa: &'a [char],
    integer: &'a [char],
    fraction: &'a [char],
    /// `None` when the picture has no exponent separator at all; `Some(&[])`
    /// when it has one with nothing behind it.
    exponent: Option<&'a [char]>,
}

/// The worst rule violated so far.
///
/// jsonata-js `validate()` walks the rules in ascending code order assigning a
/// single `error` variable, so on a multiply-invalid picture the *last*
/// assignment — the highest-numbered violated rule — is the one reported:
/// `"0%,"` is D3088, not D3082-adjacent; `"0.0e%0"` is D3093, not D3092;
/// `"#%.0.0#"` is D3086, not D3081. jsntrs used to report the first rule it
/// tripped over. Keeping the maximum reproduces "last assignment wins"
/// without tying the answer to the order the checks happen to run in
/// (jsntrs-p0v.23).
#[derive(Default)]
struct WorstRule(Option<&'static str>);

impl WorstRule {
    fn note(&mut self, code: &'static str) {
        match self.0 {
            Some(seen) if seen >= code => {}
            _ => self.0 = Some(code),
        }
    }
}

fn picture_error(code: &'static str) -> JsonataError {
    let message = match code {
        "D3081" => "picture has more than one decimal separator",
        "D3082" => "picture has more than one percent character",
        "D3083" => "picture has more than one per-mille character",
        "D3084" => "picture has both percent and per-mille characters",
        "D3085" => "picture mantissa has no digit placeholders",
        "D3086" => "invalid character in active picture region",
        "D3087" => "grouping separator adjacent to decimal separator",
        "D3088" => "grouping separator at end of integer part",
        "D3089" => "adjacent grouping separators in picture",
        "D3090" => "optional digit cannot follow mandatory digit in integer part",
        "D3091" => "mandatory digit cannot follow optional digit in fraction part",
        "D3092" => "percent/per-mille cannot appear in picture with exponent separator",
        _ => "exponent part must comprise digit-family characters",
    };
    JsonataError::new(code, format!("$formatNumber: {message}"))
}

/// Check every F&O 4.7.3 rule and report the highest-numbered violation.
fn validate_sub_picture(p: &SubPictureParts<'_>, fc: &FmtChars) -> Option<JsonataError> {
    let mut worst = WorstRule::default();

    if count_char(p.picture, fc.decimal_sep) > 1 {
        worst.note("D3081");
    }
    let percents = count_occurrences(p.picture, &fc.percent);
    let per_milles = count_occurrences(p.picture, &fc.per_mille);
    if percents > 1 {
        worst.note("D3082");
    }
    if per_milles > 1 {
        worst.note("D3083");
    }
    if percents > 0 && per_milles > 0 {
        worst.note("D3084");
    }

    if !p
        .mantissa
        .iter()
        .any(|&c| fc.is_digit_char(c) || c == fc.digit)
    {
        worst.note("D3085");
    }

    // Percent and per-mille are passive characters: legal only in the prefix
    // or suffix, never between active characters. Exempting them here let
    // "0,%." reach the grouping scan with a separator that has no digits to
    // its right, whose zero position looped forever in
    // `compute_int_group_positions` (jsntrs-spm).
    if p.active.iter().any(|&c| !fc.is_active_char(c)) {
        worst.note("D3086");
    }

    // A grouping separator on either side of the decimal separator is
    // D3087 — including one in the suffix, since the reference tests the
    // characters around the separator in the whole sub-picture. Only a
    // picture with no decimal separator at all reaches the D3088 rule.
    match p.picture.iter().position(|&c| c == fc.decimal_sep) {
        Some(d) => {
            let before = d.checked_sub(1).map(|i| p.picture[i]);
            let after = p.picture.get(d + 1).copied();
            if before == Some(fc.grouping_sep) || after == Some(fc.grouping_sep) {
                worst.note("D3087");
            }
        }
        None => {
            if p.integer.last() == Some(&fc.grouping_sep) {
                worst.note("D3088");
            }
        }
    }

    if p.picture
        .windows(2)
        .any(|w| w[0] == fc.grouping_sep && w[1] == fc.grouping_sep)
    {
        worst.note("D3089");
    }

    if let Some(i) = p.integer.iter().position(|&c| c == fc.digit)
        && p.integer[..i].iter().any(|&c| fc.is_digit_char(c))
    {
        worst.note("D3090");
    }
    if let Some(i) = p.fraction.iter().rposition(|&c| c == fc.digit)
        && p.fraction[i..].iter().any(|&c| fc.is_digit_char(c))
    {
        worst.note("D3091");
    }

    if let Some(exponent) = p.exponent {
        if !exponent.is_empty() && (percents > 0 || per_milles > 0) {
            worst.note("D3092");
        }
        // The exponent part must comprise one or more digit-family
        // characters: `#` and a grouping separator are as invalid there as a
        // passive character, and D3093 outranks the D3092 an exponent
        // picture with a percent sign also trips.
        if exponent.is_empty() || exponent.iter().any(|&c| !fc.is_digit_char(c)) {
            worst.note("D3093");
        }
    }

    worst.0.map(picture_error)
}

fn analyse_int_part(int_part: &[char], fc: &FmtChars, sp: &mut SubPicture) {
    for &c in int_part {
        if fc.is_digit_char(c) {
            sp.int_mandatory += 1;
            sp.has_any_int_digit = true;
        } else if c == fc.digit {
            sp.int_optional += 1;
            sp.has_any_int_digit = true;
        }
    }

    // Grouping positions, counted in digits from the right.
    let mut digits_to_the_right: usize = 0;
    for i in (0..int_part.len()).rev() {
        let c = int_part[i];
        if fc.is_digit_char(c) || c == fc.digit {
            digits_to_the_right += 1;
        } else if c == fc.grouping_sep {
            sp.int_grp_pos.push(digits_to_the_right);
        }
    }
}

fn analyse_frac_part(frac_part: &[char], fc: &FmtChars, sp: &mut SubPicture) {
    let mut digits_to_the_left: usize = 0;

    for &c in frac_part {
        if fc.is_digit_char(c) {
            digits_to_the_left += 1;
            sp.frac_mandatory += 1;
        } else if c == fc.digit {
            digits_to_the_left += 1;
            sp.frac_optional += 1;
        } else if c == fc.grouping_sep {
            sp.frac_grp_pos.push(digits_to_the_left);
        }
    }
}

pub(crate) fn parse_sub_picture(pic: &str, fc: &FmtChars) -> Result<SubPicture, JsonataError> {
    let runes: Vec<char> = pic.chars().collect();
    let mut sp = SubPicture::default();

    let (active, start) = scan_sub_picture_region(&runes, fc, &mut sp)?;
    let exp_pos = locate_exponent(&runes, start, active.len(), fc);
    // `exp_pos` may sit one past the active region (a separator that opens the
    // suffix), which leaves the whole region as the mantissa and the exponent
    // part empty.
    let mantissa = exp_pos.map_or(active, |e| &active[..e.min(active.len())]);
    let exponent = exp_pos.map(|e| active.get(e + 1..).unwrap_or(&[]));
    // The decimal separator is looked for in the mantissa alone: one in the
    // exponent part is a D3093, not a mantissa split. When a single character
    // is configured as both separators the exponent wins, exactly as in
    // jsonata-js — "0.0" with `exponent-separator` "." is mantissa "0",
    // exponent "0", and jsntrs used to panic on the backwards slice range.
    let decimal = mantissa.iter().position(|&c| c == fc.decimal_sep);
    let (integer, fraction) = match decimal {
        Some(d) => (&mantissa[..d], &mantissa[d + 1..]),
        None => (mantissa, &mantissa[..0]),
    };

    if let Some(err) = validate_sub_picture(
        &SubPictureParts {
            picture: &runes,
            active,
            mantissa,
            integer,
            fraction,
            exponent,
        },
        fc,
    ) {
        return Err(err);
    }

    sp.scale = scaling_factor(&runes, fc);
    sp.has_decimal = decimal.is_some();
    analyse_int_part(integer, fc, &mut sp);
    analyse_frac_part(fraction, fc, &mut sp);

    // Validation has already rejected anything but digit-family characters in
    // the exponent part.
    sp.exp_mandatory = exponent.map_or(0, <[char]>::len);
    sp.exp_min_width = sp.exp_mandatory;

    Ok(sp)
}

// ── Formatting helpers ────────────────────────────────────────────────────────

fn compute_int_group_positions(grp_pos: &[usize], int_len: usize) -> Vec<usize> {
    if grp_pos.is_empty() {
        return Vec::new();
    }
    // `grp_pos` is non-decreasing (parse_int_part scans right to left and
    // pushes a running count), which the window subtraction relies on.
    let primary = grp_pos[0];
    let all_equal = grp_pos.windows(2).all(|w| w[1] - w[0] == primary);
    let mut result = Vec::new();
    // `primary == 0` (a separator with no digits to its right) must not
    // enter the repeating branch: advancing by zero never terminates.
    // Picture validation no longer produces it (jsntrs-spm), but the
    // expansion has to stay total regardless; the literal positions are
    // harmless, a zero simply never matches a real digit position.
    if primary > 0 && (grp_pos.len() == 1 || all_equal) {
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

/// Cut the picture into sub-pictures, with `String.prototype.split`
/// semantics: the separator is matched as a string, so a multi-character
/// `pattern-separator` splits on the whole run, and an empty one splits
/// between every character.
///
/// JS returns *no* parts when both the picture and the separator are empty;
/// jsonata-js then analyses an undefined sub-picture and throws a TypeError.
/// jsntrs keeps the single empty sub-picture it has always answered D3085
/// for, which is the same deviation `scan_sub_picture_region` documents.
pub(crate) fn split_on_pattern_sep(picture: &str, sep: &str) -> Vec<String> {
    if sep.is_empty() {
        if picture.is_empty() {
            return vec![String::new()];
        }
        return picture.chars().map(String::from).collect();
    }
    picture.split(sep).map(str::to_string).collect()
}

fn format_number_picture(
    n: f64,
    picture: &str,
    opts: &[(&str, &str)],
) -> Result<String, JsonataError> {
    let fc = FmtChars::from_opts(opts);

    let pics = split_on_pattern_sep(picture, &fc.pattern_sep);
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
    // `-0.0 < 0.0` is false, so negative zero already formats through the
    // positive sub-picture (jsonata-js branches on `value >= 0`, which agrees).
    // Its sign still has to go: `format!("{:.2}", -0.0)` writes "-0.00", and the
    // minus then travels through the picture machinery as if it were a digit —
    // with grouping, "9,9,99.99" produced "0,0,-0.00" (jsntrs-p0v.26). Adding
    // zero normalises -0.0 to 0.0 and leaves every other value alone.
    let mut value = if negative { -n } else { n + 0.0 };

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

    /// Negative zero formats as zero. `-0.0 < 0.0` is false, so it already
    /// took the positive sub-picture (jsonata-js branches on `value >= 0`
    /// and agrees), but the sign `format!("{:.p$}")` writes still reached the
    /// picture machinery: jsntrs answered "-0.00", and with grouping the
    /// minus was grouped as if it were a digit — "9,9,99.99" gave
    /// "0,0,-0.00". Expected values verified against jsonata-js 2.1.0
    /// (jsntrs-p0v.26).
    #[test]
    fn negative_zero_formats_as_zero() {
        assert_eq!(fmt(-0.0, "0.00"), "0.00");
        assert_eq!(fmt(-0.0, "0.0"), "0.0");
        assert_eq!(fmt(-0.0, "9,9,99.99"), "0,0,00.00");
        assert_eq!(fmt(-0.0, "#,##0.00"), "0.00");
        assert_eq!(fmt(-0.0, "0.00%"), "0.00%");
        assert_eq!(fmt(-0.0, "‰0.00"), "‰0.00");
        // The negative sub-picture is for negative numbers, and -0.0 is not
        // one of them.
        assert_eq!(fmt(-0.0, "0.00;(0.00)"), "0.00");
        assert_eq!(fmt(-0.0, "0.00;-0.00"), "0.00");
        // An exponent picture has no reference answer — jsonata-js loops
        // forever on a zero mantissa — but the sign must be gone all the
        // same.
        assert_eq!(fmt(-0.0, "0.0e0"), fmt(0.0, "0.0e0"));
        assert_eq!(fmt(-0.0, "00.000e0"), fmt(0.0, "00.000e0"));
        // A number that really is negative still takes the negative picture.
        assert_eq!(fmt(-0.000_000_001, "0.00"), "-0.00");
        assert_eq!(fmt(-0.4, "0"), "-0");
        assert_eq!(fmt(-1.0, "0.00;(0.00)"), "(1.00)");
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

    /// `percent` and `per-mille` are matched as strings: jsonata-js only ever
    /// asks whether the sub-picture *contains* the option's value. Expected
    /// values verified against jsonata-js 2.1.0 (jsntrs-p0v.27); jsntrs took
    /// the value's first character, so a multi-character per-mille broke
    /// pictures that never mentioned it.
    #[test]
    fn scaling_options_are_matched_as_strings() {
        // First-character matching made this D3083: "00" has two '0'.
        assert_eq!(
            fmt_opts(7.0, "00", r#"{"per-mille": "0a"}"#),
            Ok("07".to_string())
        );
        assert_eq!(
            fmt_opts(7.0, "00", r#"{"percent": "0a"}"#),
            Ok("07".to_string())
        );
        // Once the option replaces it, the default marker is passive text.
        assert_eq!(
            fmt_opts(7.0, "0%", r#"{"percent": "0a"}"#),
            Ok("7%".to_string())
        );
        assert_eq!(
            fmt_opts(7.0, "0‰", r#"{"per-mille": "0a"}"#),
            Ok("7‰".to_string())
        );
        // A multi-character value that does occur still scales.
        assert_eq!(
            fmt_opts(7.0, "00a", r#"{"percent": "0a"}"#),
            Ok("700a".to_string())
        );
        assert_eq!(
            fmt_opts(7.0, "00a", r#"{"per-mille": "0a"}"#),
            Ok("7000a".to_string())
        );
        assert_eq!(
            fmt_opts(7.0, "0ab", r#"{"percent": "ab"}"#),
            Ok("700ab".to_string())
        );
        // An empty value matches at every position, so every non-empty
        // picture holds "more than one instance" of it.
        assert_eq!(fmt_opts(7.0, "0", r#"{"percent": ""}"#), Err("D3082"));
        assert_eq!(fmt_opts(7.0, "0", r#"{"per-mille": ""}"#), Err("D3083"));
    }

    /// `pattern-separator` splits the picture as a string, like
    /// `String.prototype.split`. Expected values verified against jsonata-js
    /// 2.1.0 (jsntrs-p0v.27); jsntrs ignored any value that was not a single
    /// character and split on `;` regardless.
    #[test]
    fn pattern_separator_splits_on_the_whole_value() {
        let aa = r#"{"pattern-separator": "aa"}"#;
        assert_eq!(fmt_opts(7.0, "0aa00", aa), Ok("7".to_string()));
        assert_eq!(fmt_opts(-7.0, "0aa00", aa), Ok("07".to_string()));
        // `;` is then an ordinary passive character between active ones.
        assert_eq!(fmt_opts(7.0, "0;00", aa), Err("D3086"));
        assert_eq!(fmt_opts(-7.0, "0aa#0aa0", aa), Err("D3080"));
        // An empty separator splits between every character, and a separator
        // occurring twice yields three sub-pictures.
        let empty = r#"{"pattern-separator": ""}"#;
        assert_eq!(fmt_opts(7.0, "0", empty), Ok("7".to_string()));
        assert_eq!(fmt_opts(7.0, "00", empty), Ok("7".to_string()));
        assert_eq!(fmt_opts(7.0, "000", empty), Err("D3080"));
        assert_eq!(
            fmt_opts(7.0, "00", r#"{"pattern-separator": "0"}"#),
            Err("D3080")
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

    /// One character configured as both the decimal and the exponent
    /// separator is an exponent separator: the mantissa ends there, so
    /// `"0.0"` is mantissa "0", exponent "0". Expected values verified
    /// against jsonata-js 2.1.0. jsntrs used to panic here ("slice index
    /// starts at 2 but ends at 1") because the mantissa split ran backwards,
    /// then answered D3085 (jsntrs-p0v.23).
    #[test]
    fn separator_that_is_both_decimal_and_exponent_splits_the_exponent() {
        let both = r#"{"exponent-separator": "."}"#;
        assert_eq!(fmt_opts(1234.5678, "0.0", both), Ok("1.3".to_string()));
        assert_eq!(fmt_opts(1234.5678, "0.00", both), Ok("1.03".to_string()));
        assert_eq!(fmt_opts(1.3, "0.0", both), Ok("1.0".to_string()));
        assert_eq!(fmt_opts(1.3, "0.00", both), Ok("1.00".to_string()));
        // Two of them are still two decimal separators — and the second one
        // lands in the exponent part, which outranks D3081.
        assert_eq!(fmt_opts(1.3, "0.0.0", both), Err("D3093"));
    }

    /// Expected codes verified against jsonata-js 2.1.0 (jsntrs-p0v.23).
    /// jsonata-js `validate()` assigns one error variable in ascending code
    /// order, so the highest-numbered violated rule is the one reported;
    /// jsntrs used to answer whichever rule it checked first.
    #[test]
    fn the_highest_numbered_violated_rule_is_reported() {
        for (picture, code) in [
            ("0%,", "D3088"),    // over D3082's neighbourhood
            ("0#%0", "D3090"),   // over D3086
            ("0.0%e0", "D3092"), // over D3086
            ("0.0e%0", "D3093"), // over D3092
            ("#%.0.0#", "D3086"),
            ("‰,‰0- ", "D3086"), // over D3083
            ("0.,,0", "D3089"),  // over D3087
        ] {
            assert_eq!(
                fmt_args(&[Value::Number(7.0), Value::String(picture.into())]),
                Err(code),
                "{picture}"
            );
        }
    }

    /// Rules the reference phrases over the whole sub-picture, not over the
    /// active region: a grouping separator on either side of the decimal
    /// separator is D3087 wherever it sits, and two adjacent grouping
    /// separators are D3089. Expected codes verified against jsonata-js
    /// 2.1.0; jsntrs saw only the integer part and accepted "0.,0".
    #[test]
    fn grouping_separator_rules_span_the_whole_sub_picture() {
        for (picture, code) in [("0,.0", "D3087"), ("0.,0", "D3087"), ("0,,0", "D3089")] {
            assert_eq!(
                fmt_args(&[Value::Number(7.0), Value::String(picture.into())]),
                Err(code),
                "{picture}"
            );
        }
    }

    /// The exponent part must comprise digit-family characters: an optional
    /// digit, a grouping separator or a sign are all D3093. Expected codes
    /// verified against jsonata-js 2.1.0; jsntrs only rejected an empty
    /// exponent part and a grouping separator in it (jsntrs-p0v.23).
    #[test]
    fn exponent_part_must_be_digit_family() {
        for picture in ["0.0e#", "0.0e0#", "0.0e-0", "0.0e0,0", "0e0.0"] {
            assert_eq!(
                fmt_args(&[Value::Number(1234.5678), Value::String(picture.into())]),
                Err("D3093"),
                "{picture}"
            );
        }
    }

    /// jsonata-js indexes the *active part* with the exponent separator's
    /// position in the whole sub-picture, so any picture with both a prefix
    /// and an exponent splits at the wrong offset there: "$0.0e0" is a
    /// spurious D3093 (its exponent part comes out empty) and "-0E.0" is
    /// accepted with the '.' swallowed into the mantissa. jsntrs is
    /// XPath-correct and does not replicate the off-by-prefix bug
    /// (jsntrs-p0v.23) — these expectations deliberately differ from the
    /// reference.
    #[test]
    fn the_exponent_split_is_not_offset_by_the_prefix() {
        assert_eq!(fmt(1234.5678, "$0.0e0"), "$1.2e3");
        assert_eq!(
            fmt_opts(7.0, "-0E.0", r#"{"exponent-separator": "E"}"#),
            Err("D3093")
        );
    }

    /// Expected codes verified against jsonata-js 2.1.0 (jsntrs-spm): a
    /// percent or per-mille buried between active characters is a D3086
    /// picture error. jsntrs used to accept it, and "0,%." then recorded a
    /// grouping separator with no digits to its right — a zero grouping
    /// position that `compute_int_group_positions` expanded forever,
    /// allocating without bound.
    #[test]
    fn interior_percent_and_per_mille_are_picture_errors() {
        for pic in ["0,%.", "0,‰.", "#,%.", "0,%.0", "0%0", "0‰0"] {
            assert_eq!(
                fmt_args(&[Value::Number(7.0), Value::String(pic.into())]),
                Err("D3086"),
                "{pic}"
            );
        }
        // The loop was input-independent; zero is not special.
        assert_eq!(
            fmt_args(&[Value::Number(0.0), Value::String("0,%.".into())]),
            Err("D3086")
        );
        // A custom percent character is just as passive (jsonata-js agrees);
        // the old exemption accepted "0@0" here and formatted "700".
        assert_eq!(fmt_opts(7.0, "0@0", r#"{"percent": "@"}"#), Err("D3086"));
        // In the prefix or suffix the scaling characters stay legal.
        assert_eq!(fmt(1234.0, "#,##0%"), "123,400%");
        assert_eq!(fmt(7.0, "0%"), "700%");
        assert_eq!(fmt(7.0, "%0"), "%700");
    }

    /// The grouping expansion must stay total even if a zero position ever
    /// reaches it again: advance-by-zero looped forever (jsntrs-spm).
    #[test]
    fn zero_grouping_position_inserts_no_separator() {
        assert_eq!(apply_int_grouping("700", &[0], ','), "700");
        assert_eq!(apply_int_grouping("1234567", &[3], ','), "1,234,567");
    }
}
