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

/// The `minus-sign` property. jsonata-js exposes it as an option and jsntrs
/// does not yet, so it is the default everywhere it is written: in front of
/// the negative sub-picture's prefix, and in front of a negative exponent.
const MINUS_SIGN: char = '-';

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

/// One analysed sub-picture: the variables F&O 4.7.4 `analyse` produces, in
/// the shape jsonata-js computes them. The formatting bullets in
/// `format_sub_picture` read nothing else.
#[derive(Default, Clone)]
pub(crate) struct SubPicture {
    prefix: String,
    suffix: String,
    /// Grouping positions in the integer part, in picture order (left to
    /// right, so *descending* digit counts). The order is load-bearing:
    /// bullet 10 walks the list in order and moves the decimal position along
    /// after each insertion.
    int_grp_pos: Vec<usize>,
    /// The regular grouping interval, or 0 when the positions are irregular.
    regular_grouping: usize,
    frac_grp_pos: Vec<usize>,
    min_int: usize,
    /// `minimumIntegerPartSize` as it stood *before* the 4.7.4 adjustments —
    /// the window bullet 5 normalises the mantissa into. jsonata-js snapshots
    /// it into `scalingFactor` and then keeps adjusting the original.
    scaling_factor: usize,
    min_frac: usize,
    max_frac: usize,
    min_exp: usize,
    /// 0=none, 1=percent, 2=per-mille
    scale: u8,
    /// Whether the *sub-picture* carries a decimal separator anywhere.
    /// Bullet 12 tests the picture, not the mantissa, which is why `"#e0"`
    /// (no separator at all) drops the one bullet 7 appended and formats
    /// `1234.5678` as "0.e4".
    has_decimal: bool,
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

// ── Analysis (F&O 4.7.4) ──────────────────────────────────────────────────────

/// Mandatory digits: characters of the picture's digit family.
fn count_mandatory(part: &[char], fc: &FmtChars) -> usize {
    part.iter().filter(|&&c| fc.is_digit_char(c)).count()
}

/// Digit places: digit-family characters plus the optional-digit character.
fn count_places(part: &[char], fc: &FmtChars) -> usize {
    part.iter()
        .filter(|&&c| fc.is_digit_char(c) || c == fc.digit)
        .count()
}

/// Grouping positions for one part, counted in digit places.
///
/// `to_left` counts the places to the left of each separator (the fractional
/// part rule); otherwise the places from the separator rightwards.
///
/// The scan advances through `int_part` whichever part it was handed, because
/// jsonata-js `getGroupingPositions` closes over `parts.integerPart` for the
/// "next separator" search. So the fractional scan finds only its own *first*
/// separator and then walks the integer part's separator indices, counting
/// each against the fractional part (clamped, `String.prototype.substring`
/// style). Replicated deliberately: it is the whole of the reference's
/// fractional grouping behaviour, and `"0.0,0,0"` formats 1234.5678 as
/// "1234.5,68" there — one separator, not two (jsonata 2.2.2, jsntrs-tx4).
fn grouping_positions(
    part: &[char],
    int_part: &[char],
    to_left: bool,
    fc: &FmtChars,
) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut at = part.iter().position(|&c| c == fc.grouping_sep);
    while let Some(i) = at {
        let cut = i.min(part.len());
        let counted = if to_left { &part[..cut] } else { &part[cut..] };
        positions.push(count_places(counted, fc));
        at = int_part
            .iter()
            .enumerate()
            .skip(i + 1)
            .find(|&(_, &c)| c == fc.grouping_sep)
            .map(|(j, _)| j);
    }
    positions
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// The regular grouping interval, or 0 when the positions are irregular.
///
/// jsonata-js takes the GCD of the positions and demands that every multiple
/// of it up to `positions.len()` is present. The two conditions together
/// force the set to be exactly `{f, 2f, …, nf}`, which is why this is the
/// same function as the "every gap equals the smallest position" rule jsntrs
/// carried before the port — proof and exhaustive check on
/// `regular_grouping_matches_equal_gap_rule` (jsntrs-4fr).
fn regular_grouping(positions: &[usize]) -> usize {
    let Some(factor) = positions.iter().copied().reduce(gcd) else {
        return 0;
    };
    for index in 1..=positions.len() {
        if !positions.contains(&index.saturating_mul(factor)) {
            return 0;
        }
    }
    factor
}

/// Fill in the 4.7.4 variables. `integer`, `fraction` and `exponent` are the
/// regions `parse_sub_picture` cut, `picture` the whole sub-picture.
fn analyse(
    picture: &[char],
    integer: &[char],
    fraction: &[char],
    exponent: Option<&[char]>,
    fc: &FmtChars,
    sp: &mut SubPicture,
) {
    sp.scale = scaling_factor(picture, fc);
    sp.has_decimal = picture.contains(&fc.decimal_sep);
    sp.int_grp_pos = grouping_positions(integer, integer, false, fc);
    sp.regular_grouping = regular_grouping(&sp.int_grp_pos);
    sp.frac_grp_pos = grouping_positions(fraction, integer, true, fc);

    sp.min_int = count_mandatory(integer, fc);
    sp.scaling_factor = sp.min_int;
    sp.min_frac = count_mandatory(fraction, fc);
    sp.max_frac = count_places(fraction, fc);

    let has_exponent = exponent.is_some();
    if sp.min_int == 0 && sp.max_frac == 0 {
        if has_exponent {
            sp.min_frac = 1;
            sp.max_frac = 1;
        } else {
            sp.min_int = 1;
        }
    }
    if has_exponent && sp.min_int == 0 && integer.contains(&fc.digit) {
        sp.min_int = 1;
    }
    if sp.min_int == 0 && sp.min_frac == 0 {
        sp.min_frac = 1;
    }
    sp.min_exp = exponent.map_or(0, |e| count_mandatory(e, fc));
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
    // With no decimal separator in the mantissa the fraction part is the
    // *suffix*, not nothing: jsonata-js `splitParts` writes
    // `fractionalPart = suffix` there. Every rule the fraction feeds counts
    // only active characters, which the suffix scan has already excluded, so
    // the two agree unless an option makes a digit-family character passive
    // (`{"exponent-separator": "5"}`) — carry the reference's definition
    // rather than the coincidence.
    let (integer, fraction) = match decimal {
        Some(d) => (&mantissa[..d], &mantissa[d + 1..]),
        None => (mantissa, &runes[start + active.len()..]),
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

    analyse(&runes, integer, fraction, exponent, fc, &mut sp);
    Ok(sp)
}

// ── Formatting (F&O 4.7.5, jsonata-js bullets 5–14) ───────────────────────────

/// `usize` as `isize`, saturating. Only picture- and digit-string-sized
/// values reach it, so the saturation is unreachable in practice.
fn as_isize(v: usize) -> isize {
    isize::try_from(v).unwrap_or(isize::MAX)
}

/// Index of `needle`, or -1 — `String.prototype.indexOf`, which the bullets
/// compare against and do arithmetic on.
fn index_of(hay: &[char], needle: char) -> isize {
    hay.iter().position(|&c| c == needle).map_or(-1, as_isize)
}

/// The insertion point `String.prototype.slice` splits at: a negative index
/// counts back from the end, and both directions clamp into range. Bullets 10
/// and 11 compute offsets that can fall outside the string — a grouping
/// separator further left than the number is long gives a negative one, and
/// jsonata-js then wraps it around rather than skipping the separator, so
/// `$formatNumber(7, "#,###,#")` is ",,7".
fn js_split_point(at: isize, len: usize) -> usize {
    let len_i = as_isize(len);
    let resolved = if at < 0 { len_i + at } else { at };
    usize::try_from(resolved.clamp(0, len_i)).unwrap_or(0)
}

/// jsonata-js `makeString`: the magnitude at `dp` decimal places, mapped into
/// the picture's digit family. Only the digits produced here are mapped — a
/// separator that happens to be an ASCII digit is picture text and stays as
/// written.
fn make_string(value: f64, dp: usize, fc: &FmtChars) -> Vec<char> {
    format!("{:.dp$}", value.abs())
        .chars()
        .map(|c| {
            if c.is_ascii_digit() {
                char::from_u32(fc.zero_digit as u32 + (c as u32 - '0' as u32)).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

/// Bullet 5: split the value into a mantissa and, when the picture has an
/// exponent part, the exponent that normalises it into the picture's window.
fn mantissa_and_exponent(adjusted: f64, sp: &SubPicture) -> (f64, Option<i32>) {
    if sp.min_exp == 0 {
        return (adjusted, None);
    }
    let scaling = i32::try_from(sp.scaling_factor).unwrap_or(i32::MAX);
    let max_mantissa = 10f64.powi(scaling);
    let min_mantissa = 10f64.powi(scaling.saturating_sub(1));
    let mut mantissa = adjusted;
    let mut exponent: i32 = 0;
    // "If N is zero, set M to zero and E to zero" — and the comparisons are on
    // magnitudes, so a negative mantissa terminates too (jsonata-js #785).
    if mantissa != 0.0 {
        while mantissa.abs() < min_mantissa {
            mantissa *= 10.0;
            exponent -= 1;
        }
        while mantissa.abs() > max_mantissa {
            mantissa /= 10.0;
            exponent += 1;
        }
    }
    (mantissa, Some(exponent))
}

/// Bullet 10: the integer-part grouping separators.
fn group_integer_part(sv: &mut Vec<char>, sp: &SubPicture, fc: &FmtChars, decimal_pos: isize) {
    if sp.regular_grouping > 0 {
        let interval = as_isize(sp.regular_grouping);
        // `Math.floor((decimalPos - 1) / regularGrouping)`; a missing decimal
        // separator leaves this negative and the loop simply does not run.
        let groups = (decimal_pos - 1).div_euclid(interval);
        for group in 1..=groups {
            let at = js_split_point(decimal_pos - group * interval, sv.len());
            sv.insert(at, fc.grouping_sep);
        }
        return;
    }
    // Irregular positions are applied literally, left to right, each one
    // shifting the decimal separator along by the separator just inserted.
    for (inserted, &pos) in (decimal_pos..).zip(sp.int_grp_pos.iter()) {
        let at = js_split_point(inserted - as_isize(pos), sv.len());
        sv.insert(at, fc.grouping_sep);
    }
}

/// Bullets 5–13: the digits, the separators and the exponent, without the
/// prefix and suffix.
fn format_sub_picture(adjusted: f64, sp: &SubPicture, fc: &FmtChars) -> String {
    let (mantissa, exponent) = mantissa_and_exponent(adjusted, sp);

    // Bullets 6 and 7: round to the picture's precision half-to-even — over
    // the decimal digits, not the binary value, which is what makes
    // $formatNumber(1.115, "0.00") "1.12" — then render the magnitude.
    let rounded = super::numeric::bankers_round(mantissa, i32::try_from(sp.max_frac).unwrap_or(0));
    let mut sv = make_string(rounded, sp.max_frac, fc);
    match sv.iter().position(|&c| c == '.') {
        Some(i) => sv[i] = fc.decimal_sep,
        None => sv.push(fc.decimal_sep),
    }
    // Strip every leading and trailing zero-digit. The decimal separator
    // stops both runs, so this trims the integer part on the left and the
    // fraction on the right; bullets 8 and 9 pad back to the minima.
    let leading = sv.iter().take_while(|&&c| c == fc.zero_digit).count();
    sv.drain(..leading);
    while sv.last() == Some(&fc.zero_digit) {
        sv.pop();
    }

    // Bullets 8 and 9.
    let decimal_pos = index_of(&sv, fc.decimal_sep);
    let pad_left = as_isize(sp.min_int) - decimal_pos;
    let pad_right = as_isize(sp.min_frac) - (as_isize(sv.len()) - decimal_pos - 1);
    for _ in 0..pad_left.max(0) {
        sv.insert(0, fc.zero_digit);
    }
    for _ in 0..pad_right.max(0) {
        sv.push(fc.zero_digit);
    }

    // Bullet 10.
    let decimal_pos = index_of(&sv, fc.decimal_sep);
    group_integer_part(&mut sv, sp, fc, decimal_pos);

    // Bullet 11. The decimal position is *not* re-read between separators, so
    // each offset is measured against the string as it was — which is exactly
    // the shift the previous insertion introduced.
    let decimal_pos = index_of(&sv, fc.decimal_sep);
    for &pos in &sp.frac_grp_pos {
        let at = js_split_point(as_isize(pos) + decimal_pos + 1, sv.len());
        sv.insert(at, fc.grouping_sep);
    }

    // Bullet 12: drop the decimal separator again when the picture never
    // asked for one, or when nothing followed it.
    if !sp.has_decimal || index_of(&sv, fc.decimal_sep) == as_isize(sv.len()) - 1 {
        sv.pop();
    }

    // Bullet 13.
    if let Some(exponent) = exponent {
        let mut digits = make_string(f64::from(exponent), 0, fc);
        for _ in digits.len()..sp.min_exp {
            digits.insert(0, fc.zero_digit);
        }
        // Only reached when the picture had an exponent part, which implies a
        // separator character was configured; the fallback keeps this total.
        sv.push(fc.exponent_sep.unwrap_or(DEFAULT_EXPONENT_SEP));
        if exponent < 0 {
            sv.push(MINUS_SIGN);
        }
        sv.extend(digits);
    }

    sv.into_iter().collect()
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

/// Split a picture and analyse both sub-pictures: the positive one and the
/// one negative values take, which is the second sub-picture when the picture
/// carries a pattern separator and otherwise a copy with a minus sign glued
/// to the prefix.
pub(crate) fn prepare_sub_pictures(
    picture: &str,
    fc: &FmtChars,
) -> Result<(SubPicture, SubPicture), JsonataError> {
    let pics = split_on_pattern_sep(picture, &fc.pattern_sep);
    if pics.len() > 2 {
        return Err(JsonataError::new(
            "D3080",
            "$formatNumber: picture has more than one pattern separator",
        ));
    }

    let pos_pic = parse_sub_picture(&pics[0], fc)?;
    let neg_pic = if pics.len() == 2 {
        parse_sub_picture(&pics[1], fc)?
    } else {
        let mut np = pos_pic.clone();
        np.prefix = format!("{MINUS_SIGN}{}", pos_pic.prefix);
        np
    };
    Ok((pos_pic, neg_pic))
}

/// Bullets 2, 3 and 14: pick the sub-picture, apply the scaling factor, and
/// wrap the formatted digits in the prefix and suffix.
///
/// `-0.0 < 0.0` is false, so negative zero formats through the positive
/// sub-picture, exactly as jsonata-js branching on `value >= 0` does; its
/// sign disappears in `make_string`, which formats the magnitude.
pub(crate) fn format_number_value(
    n: f64,
    pos_pic: &SubPicture,
    neg_pic: &SubPicture,
    fc: &FmtChars,
) -> String {
    let sp = if n < 0.0 { neg_pic } else { pos_pic };
    let adjusted = match sp.scale {
        1 => n * 100.0,
        2 => n * 1000.0,
        _ => n,
    };
    let inner = format_sub_picture(adjusted, sp, fc);
    format!("{}{inner}{}", sp.prefix, sp.suffix)
}

fn format_number_picture(
    n: f64,
    picture: &str,
    opts: &[(&str, &str)],
) -> Result<String, JsonataError> {
    let fc = FmtChars::from_opts(opts);
    let (pos_pic, neg_pic) = prepare_sub_pictures(picture, &fc)?;
    Ok(format_number_value(n, &pos_pic, &neg_pic, &fc))
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

    /// A grouping separator with no digit places to its right records
    /// position 0, which the reference emits immediately before the decimal
    /// separator; jsntrs used to drop it. Reaching that position at all needs
    /// a picture whose decimal separator is also the exponent separator, so
    /// that the D3087/D3088 rules look at a character that is not there.
    /// Expected value verified against jsonata 2.2.2 (jsntrs-tx4); the
    /// expansion is a bounded walk over the recorded positions now, so the
    /// advance-by-zero loop of jsntrs-spm cannot come back.
    #[test]
    fn zero_grouping_position_is_emitted() {
        let dot = r#"{"exponent-separator": "."}"#;
        assert_eq!(fmt_opts(1.3, "#9,%. ", dot), Ok("130,%. ".to_string()));
    }

    /// The GCD rule jsonata-js uses to decide whether the integer part's
    /// grouping is regular, and the "every gap equals the smallest position"
    /// rule jsntrs carried before the port, are the same function
    /// (jsntrs-4fr). Proof, over the positions `P = {p₁ … pₙ}` a valid
    /// picture can produce:
    ///
    /// - *The positions are distinct.* Two grouping separators in the integer
    ///   part are either adjacent — D3089 rejects that anywhere in the
    ///   sub-picture — or separated by at least one character, and inside the
    ///   integer part the only characters validation leaves are digit-family,
    ///   the digit placeholder and the grouping separator itself (D3086
    ///   rejects passive characters in the active region, a second decimal
    ///   separator is D3081, the exponent separator ends the mantissa, and
    ///   the pattern separator has already split the picture). So any two
    ///   separators have a different number of digit places to their right.
    /// - *GCD-regular ⟹ equal-gap-regular, same interval.* If the reference
    ///   returns `f > 0` then `{f, 2f, …, nf} ⊆ P`; those are n distinct
    ///   values and `|P| = n`, so `P = {f, 2f, …, nf}` and `min P = f`. Sorted
    ///   ascending every gap is `f`, which is the first element.
    /// - *Equal-gap-regular ⟹ GCD-regular, same interval.* If the ascending
    ///   list has every gap equal to `q₁` then `qᵢ = i·q₁`, so `gcd(P) = q₁`
    ///   and every multiple up to `n·q₁` is present.
    /// - *The degenerate cases agree.* Empty: both 0. `0 ∈ P` with `n ≥ 2`:
    ///   the GCD is that of the non-zero elements, so the n multiples the
    ///   loop wants are all non-zero and `P` has at most n−1 non-zero
    ///   elements — irregular; the equal-gap rule guards `primary == 0`
    ///   out. `P = {0}`: gcd 0, and the reference returns the factor 0.
    ///   Duplicates (unreachable, but both functions are total): fewer than n
    ///   distinct values cannot cover n distinct multiples, and a repeat
    ///   makes a gap of 0 ≠ primary.
    ///
    /// Exhaustively checked below over every position set drawn from 0..=8,
    /// plus the duplicate cases the picture grammar cannot reach.
    #[test]
    fn regular_grouping_matches_equal_gap_rule() {
        /// The pre-port rule: sorted ascending, regular when the smallest
        /// position is non-zero and every gap equals it.
        fn equal_gap_rule(positions: &[usize]) -> usize {
            let mut sorted = positions.to_vec();
            sorted.sort_unstable();
            let Some(&primary) = sorted.first() else {
                return 0;
            };
            if primary == 0 {
                return 0;
            }
            if sorted.windows(2).all(|w| w[1] - w[0] == primary) {
                primary
            } else {
                0
            }
        }

        for mask in 0u32..512 {
            let positions: Vec<usize> = (0..9).filter(|i| mask & (1 << i) != 0).collect();
            assert_eq!(
                regular_grouping(&positions),
                equal_gap_rule(&positions),
                "{positions:?}"
            );
        }
        for positions in [
            vec![],
            vec![2, 2],
            vec![1, 1],
            vec![2, 2, 4],
            vec![1, 2, 2],
            vec![0, 0],
            vec![3, 3, 3],
        ] {
            assert_eq!(
                regular_grouping(&positions),
                equal_gap_rule(&positions),
                "{positions:?}"
            );
        }
    }

    /// The two rules answering the same on the pictures that separate the
    /// candidate implementations, end to end. Positions {4,2} are regular,
    /// {2,4,8} — the set the issue proposed as a separator — is not, under
    /// both rules; expected values verified against jsonata 2.2.2
    /// (jsntrs-4fr).
    #[test]
    fn grouping_regularity_agrees_on_the_candidate_separators() {
        assert_eq!(fmt(1_234_567_890.0, "####,##,##"), "12,34,56,78,90");
        assert_eq!(fmt(1_234_567_890.0, "#,####,##,##"), "12,3456,78,90");
        assert_eq!(fmt(1_234_567_890.0, "##,####,##"), "1234,5678,90");
        assert_eq!(fmt(1_234_567_890.0, "#,##,####"), "1234,56,7890");
        assert_eq!(fmt(1_234_567.0, "##,##,##"), "1,23,45,67");
        assert_eq!(fmt(123_456.0, "#,##,#"), "123,45,6");
    }

    /// Irregular grouping positions are applied literally and wrap around the
    /// ends of the string, `String.prototype.slice` style: "#,###,#" asks for
    /// separators 4 and 1 digit places from the right, and a number with
    /// fewer digits than that gets them anyway. Expected values verified
    /// against jsonata 2.2.2 (jsntrs-tx4).
    #[test]
    fn grouping_positions_past_the_number_wrap_around() {
        assert_eq!(fmt(7.0, "#,###,#"), ",,7");
        assert_eq!(fmt(1234.5678, "#,###,#"), ",123,5");
        assert_eq!(fmt(1234.5678, "9,9,99.99"), "1,2,34.57");
    }
}
