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
            "T0410",
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

/// The option values, and the single characters they configure.
///
/// Every one of these is a *string* in jsonata-js, and the reference uses
/// each in two ways that a `char` cannot express. It tests picture characters
/// with `activeChars.indexOf(ch)`, an array of the option values, so a
/// character is active only when some option is exactly that one character —
/// an empty or multi-character value makes the corresponding character
/// passive, and `{"decimal-separator": "ab"}` turns the `.` of `"0.0"` into
/// a D3086. And it searches, splits and *emits* the value as a string, so a
/// multi-character separator lands in the output whole
/// (`$formatNumber(7, "0ab", {"decimal-separator": "ab"})` is `"7abab"`).
/// jsntrs carries both (jsntrs-2px); the `_char` fields cache the
/// single-character reading.
#[derive(Clone)]
pub(crate) struct FmtChars {
    decimal_sep: CompactString,
    decimal_char: Option<char>,
    grouping_sep: CompactString,
    grouping_char: Option<char>,
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
    percent: CompactString,
    per_mille: CompactString,
    /// The whole `zero-digit` value: the zero-stripping compares against it
    /// as a string (so a multi-character one never strips) and the padding
    /// writes it whole, one copy per missing digit.
    zero_digit: CompactString,
    zero_char: Option<char>,
    /// `properties['zero-digit'].charCodeAt(0)`: the base of the decimal
    /// digit family. `None` for an empty value, where the reference's
    /// `charCodeAt(0)` is `NaN` and the family loop produces nothing — no
    /// character is then a digit, and `makeString` maps every digit to
    /// `undefined`, which `join` drops.
    zero_base: Option<char>,
    digit: char,
    /// Split on as a string, like `String.prototype.split`: a
    /// multi-character value splits on the whole run (and leaves `;` an
    /// ordinary passive character), and an empty one splits between every
    /// character, so `{"pattern-separator": ""}` makes `"000"` three
    /// sub-pictures and a D3080.
    pattern_sep: CompactString,
    /// The `minus-sign` property: "the character used as a minus sign in the
    /// formatted number if there is no subpicture for formatting negative
    /// numbers" (F&O 4.7.1). It is written in front of the negative
    /// sub-picture's prefix (4.7.4) and in front of a negative exponent
    /// (4.7.5 bullet 13b), and nowhere else — it never appears in the picture
    /// string, so it is emitted whole rather than matched (jsntrs-12g).
    minus_sign: CompactString,
    /// The `exponent-separator`, kept as the string it is emitted as.
    /// `exponent_char` is what the picture scan matches: a multi-character
    /// value matches no single character, and jsntrs deliberately does not
    /// substring-search it the way the reference does (jsntrs-p0v.27), so
    /// `"0.0EE"` with `{"exponent-separator": "EE"}` formats here and is a
    /// D3093 there. An empty value is different: it matches at *every*
    /// position, which `locate_exponent` honours.
    exponent_sep: CompactString,
    exponent_char: Option<char>,
}

impl Default for FmtChars {
    fn default() -> Self {
        FmtChars {
            decimal_sep: CompactString::const_new("."),
            decimal_char: Some('.'),
            grouping_sep: CompactString::const_new(","),
            grouping_char: Some(','),
            percent: CompactString::const_new("%"),
            per_mille: CompactString::const_new("\u{2030}"), // ‰
            zero_digit: CompactString::const_new("0"),
            zero_char: Some('0'),
            zero_base: Some('0'),
            digit: '#',
            minus_sign: CompactString::const_new("-"),
            pattern_sep: CompactString::const_new(";"),
            exponent_sep: CompactString::const_new("e"),
            exponent_char: Some('e'),
        }
    }
}

/// The single character a value configures, or `None` when it is empty or
/// longer: `activeChars.indexOf(ch)` only ever matches a one-character entry.
fn single_char(value: &str) -> Option<char> {
    let mut chars = value.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// `charAt(i) === value`. An out-of-range index yields the empty string in
/// JS, so it matches only an empty option value.
fn char_is(ch: Option<char>, value: &str) -> bool {
    match ch {
        Some(c) => single_char(value) == Some(c),
        None => value.is_empty(),
    }
}

impl FmtChars {
    fn from_opts(opts: &[(&str, &str)]) -> Self {
        let mut fc = FmtChars::default();
        for &(key, val) in opts {
            match key {
                "decimal-separator" => {
                    fc.decimal_sep = CompactString::new(val);
                    fc.decimal_char = single_char(val);
                }
                "grouping-separator" => {
                    fc.grouping_sep = CompactString::new(val);
                    fc.grouping_char = single_char(val);
                }
                "percent" => fc.percent = CompactString::new(val),
                "per-mille" => fc.per_mille = CompactString::new(val),
                "zero-digit" => {
                    fc.zero_digit = CompactString::new(val);
                    fc.zero_char = single_char(val);
                    fc.zero_base = val.chars().next();
                }
                // `digit` stays a character: jsonata-js also substring-searches
                // it (D3090/D3091 and the exponent's minimum-integer-size
                // rule), so a multi-character value belongs with the picture
                // validation rework rather than here (jsntrs-2px).
                "digit" if val.chars().count() == 1 => {
                    fc.digit = val.chars().next().unwrap_or(fc.digit);
                }
                "minus-sign" => fc.minus_sign = CompactString::new(val),
                "pattern-separator" => fc.pattern_sep = CompactString::new(val),
                "exponent-separator" => {
                    fc.exponent_sep = CompactString::new(val);
                    fc.exponent_char = single_char(val);
                }
                _ => {}
            }
        }
        fc
    }

    fn is_digit_char(&self, c: char) -> bool {
        self.zero_base
            .is_some_and(|zero| c >= zero && (c as u32) < (zero as u32) + 10)
    }

    fn is_active_char(&self, c: char) -> bool {
        self.is_digit_char(c)
            || c == self.digit
            || self.grouping_char == Some(c)
            || self.decimal_char == Some(c)
            || self.exponent_char == Some(c)
    }

    /// Narrower notion of "active", used only when locating the picture's
    /// first and last active character — the prefix and suffix boundaries of
    /// F&O 4.7.4, "The prefix is set to contain all passive characters in the
    /// sub-picture to the left of the leftmost active character" and "The
    /// suffix is set to contain all passive characters to the right of the
    /// rightmost active character in the sub-picture".
    ///
    /// The exponent separator is excluded because 4.7.3 says a character
    /// matching that property "is treated as an exponent-separator-sign if it
    /// is both preceded and followed within the sub-picture by an active
    /// character. **Otherwise, it is treated as a passive character**" — so
    /// every exponent separator that is not the sign is passive, and belongs
    /// in the prefix or the suffix: `"e0.0"` has prefix `"e"`, and the
    /// trailing `e` of `"0.0e"` lands in the suffix. The one that *is* the
    /// sign is active, and this scan wrongly excludes it — but a sign outside
    /// the region it delimits means every character before it (or after it)
    /// is passive or another separator, so the mantissa carries no digit
    /// (D3085) or the exponent part carries none (D3093) and the picture is
    /// an error whichever way the region was cut. `locate_exponent` finds the
    /// sign in the whole sub-picture and does not depend on this scan.
    fn is_region_edge_char(&self, c: char) -> bool {
        self.is_active_char(c) && self.exponent_char != Some(c)
    }
}

// ── String matching, `String.prototype.indexOf` style ─────────────────────────

/// First occurrence of `needle` in `hay` at or after `from`, counted in
/// characters. An empty needle matches at `from` (clamped to the length),
/// exactly as `String.prototype.indexOf` reports it.
fn find_from(hay: &[char], needle: &str, from: usize) -> Option<usize> {
    // The one-character case is every default separator and most configured
    // ones; walking it as a plain character scan keeps the picture parse off
    // the general iterator comparison below.
    if let Some(c) = single_char(needle) {
        return hay
            .get(from..)
            .and_then(|tail| tail.iter().position(|&x| x == c))
            .map(|i| i + from);
    }
    let len = needle.chars().count();
    if len == 0 {
        return Some(from.min(hay.len()));
    }
    let last = hay.len().checked_sub(len)?;
    (from..=last).find(|&i| hay[i..i + len].iter().copied().eq(needle.chars()))
}

/// First occurrence of `needle` in `hay`.
fn find_sub(hay: &[char], needle: &str) -> Option<usize> {
    find_from(hay, needle, 0)
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
}

// ── Parsing helpers ───────────────────────────────────────────────────────────

/// Distinct positions at which `needle` occurs in `hay`.
///
/// The rules that use this ask only whether there are none, one, or more than
/// one — jsonata-js phrases "more than one instance" as
/// `indexOf(x) !== lastIndexOf(x)`. An empty needle matches at every position
/// including one past the end, exactly as `String.prototype.indexOf` reports
/// it, so an empty `percent` or `per-mille` option is "more than one instance"
/// for every non-empty picture.
fn count_occurrences(hay: &[char], needle: &str) -> usize {
    if let Some(c) = single_char(needle) {
        return hay.iter().filter(|&&x| x == c).count();
    }
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
        // No active character anywhere, so the sub-picture holds neither an
        // optional digit character nor a member of the decimal digit family —
        // and the mantissa is the whole sub-picture, an exponent separator
        // needing an active character on each side to be a sign (F&O 4.7.3).
        // That is exactly the rule D3085 names: "The mantissa part of a
        // sub-picture ... must contain at least one character that is either
        // an ·optional digit character· or a member of the ·decimal digit
        // family·".
        //
        // jsonata-js 2.2.2 answers D3086 for `"-"`, `"%"` and `"00"` with
        // `{"zero-digit": "ab"}`, and D3093 for `"e"`. D3086's rule is "A
        // sub-picture must not contain a passive character that is preceded
        // by an active character and that is followed by another active
        // character", which a sub-picture with no active character cannot
        // violate; D3093's is about the exponent part of a sub-picture that
        // has an exponent-separator-sign, which this one has not. Declining
        // to port those is jsntrs-g0f.
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

/// Locate the exponent-separator-sign, as an index into the whole sub-picture.
///
/// F&O 4.7.3 classifies the picture's characters once, unconditionally: "the
/// characters assigned to the properties decimal-separator, exponent-separator,
/// grouping-separator, and digit, and pattern-separator, and the members of the
/// ·decimal digit family·, are classified as active characters, and all other
/// characters ... are classified as passive characters". Then it singles one
/// character out: "A character that matches the exponent-separator property is
/// treated as an exponent-separator-sign if it is both preceded and followed
/// within the sub-picture by an active character. Otherwise, it is treated as a
/// passive character." And the reading of "preceded"/"followed" is spelled out:
/// "Note that in these rules the words 'preceded' and 'followed' refer to
/// characters anywhere in the string, they are not to be read as 'immediately
/// preceded' and 'immediately followed'."
///
/// So the test here is literal — *any* active character strictly before the
/// candidate and *any* active character strictly after it, under the first
/// classification, which makes exponent separators themselves active. Reading
/// it any other way is circular: whether a separator counts as active for the
/// test would depend on whether some other separator is the sign. The
/// reclassification ("otherwise ... passive") applies afterwards, to the rules
/// that consume the settled picture — which is why the passive separator of
/// `"0.0e0e"` is suffix text rather than an active non-digit in the exponent
/// part.
///
/// The consequences, and where each is pinned:
///   - `"0.0e"`, `"0e"`, `"9.9999eDog"` — nothing active after the separator,
///     so it is passive suffix text: "1234.6e", "1235e", "12345.6780eDog"
///     (the last is W3C QT3 fn/format-number.xml numberformat113). jsonata-js
///     takes a separator sitting immediately after the active region as
///     opening an empty — and therefore invalid — exponent part and answers
///     D3093 for all three.
///   - `"0.0ee"`, `"0ee"` — the *second* separator is an active character, so
///     the first is a sign, and its exponent part `"e"` breaks "If a
///     sub-picture contains a character treated as an exponent-separator-sign
///     then this must be followed by one or more characters that are members
///     of the ·decimal digit family·": D3093.
///   - `"ee0.0"` — symmetrically, the *first* separator is the active
///     character that precedes the second, so the second is a sign; the
///     mantissa `"e"` has no digit (D3085) and the exponent part `"0.0"`
///     holds an active non-digit (D3093), and D3093 is the higher rule.
///
/// jsonata-js keeps the separator's index in the whole sub-picture and then
/// slices the *active part* with it, so every picture with both a prefix and
/// an exponent splits at the wrong offset there: `"$0.0e0"` is a spurious
/// D3093 (its exponent part comes out empty), and `"-0E.0"` with
/// `exponent-separator` `E` is accepted, the `.` having been swallowed into
/// the mantissa. jsntrs stays XPath-correct and does not replicate the
/// off-by-prefix bug (jsntrs-p0v.23).
///
/// An *empty* `exponent-separator` matches at the very start of the search, as
/// `String.prototype.indexOf("")` does, so the mantissa comes out empty and
/// every picture is a D3085 or a D3093 — the reference answers the same way,
/// and only the picture-relative offset differs (jsntrs-2px). A
/// multi-character value matches nothing: jsonata-js substring-searches it,
/// jsntrs deliberately does not (jsntrs-p0v.27).
fn locate_exponent(runes: &[char], start: usize, fc: &FmtChars) -> Option<usize> {
    if fc.exponent_sep.is_empty() {
        return Some(start);
    }
    let sep = fc.exponent_char?;
    (0..runes.len()).find(|&i| {
        runes[i] == sep
            && runes[..i].iter().any(|&c| fc.is_active_char(c))
            && runes[i + 1..].iter().any(|&c| fc.is_active_char(c))
    })
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

    if count_occurrences(p.picture, &fc.decimal_sep) > 1 {
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
    // The characters either side are read with `charAt`, which yields one
    // character (or the empty string past the ends), so a multi-character
    // grouping separator never matches here however it is spelled in the
    // picture.
    match find_sub(p.picture, &fc.decimal_sep) {
        Some(d) => {
            let before = d.checked_sub(1).map(|i| p.picture[i]);
            let after = p.picture.get(d + 1).copied();
            if char_is(before, &fc.grouping_sep) || char_is(after, &fc.grouping_sep) {
                worst.note("D3087");
            }
        }
        None => {
            if char_is(p.integer.last().copied(), &fc.grouping_sep) {
                worst.note("D3088");
            }
        }
    }

    // Two adjacent separators, searched as the doubled string — so an empty
    // `grouping-separator` makes every picture a D3089.
    let doubled = format!("{}{}", fc.grouping_sep, fc.grouping_sep);
    if find_sub(p.picture, &doubled).is_some() {
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

/// Grouping positions for one part, counted in digit places (F&O 4.7.4).
///
/// `to_left` counts the places to the left of each separator — the
/// fractional-part rule, "the total number of ·optional digit character· and
/// ·decimal digit family· characters that appear within the fractional part of
/// the sub-picture and to the left of the grouping-separator character";
/// otherwise the places from the separator rightwards, which is the
/// integer-part rule.
///
/// Each part is scanned for its *own* separators. jsonata-js
/// `getGroupingPositions` closes over `parts.integerPart` for the "next
/// separator" search, so its fractional scan finds only the fraction's first
/// separator and then walks the integer part's separator indices; `"0.0,0,0"`
/// formats 1234.5678 as "1234.5,68" there, one separator rather than two. The
/// spec asks for one position per separator in the part, and the W3C test
/// suite pins it: `format-number(12345.6789012345, '#.#,##,#')` is
/// "12345.6,78,9" (QT3 numberformat157) — jsntrs-0kg.
fn grouping_positions(part: &[char], to_left: bool, fc: &FmtChars) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut at = find_sub(part, &fc.grouping_sep);
    while let Some(i) = at {
        let counted = if to_left { &part[..i] } else { &part[i..] };
        positions.push(count_places(counted, fc));
        // An empty separator matches at every position including the end, so
        // the reference loops here forever; it never arrives, because such a
        // picture is a D3089 before analysis runs. Requiring progress keeps
        // this total whatever validation lets through.
        at = find_from(part, &fc.grouping_sep, i + 1).filter(|&next| next > i);
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
/// F&O 4.7.4: "The grouping is defined to be regular if the following
/// conditions apply: There is an least one grouping-separator in the integer
/// part of the sub-picture. There is a positive integer G (the grouping size)
/// such that the position of every grouping-separator in the integer part of
/// the sub-picture is a positive integer multiple of G. Every position in the
/// integer part of the sub-picture that is a positive integer multiple of G is
/// occupied by a grouping-separator." Only `G = gcd(positions)` can satisfy the
/// second condition and leave the third one satisfiable — a smaller divisor
/// would itself be an unoccupied position — so the gcd is the one candidate.
///
/// The third condition is what makes `places` load-bearing: the positions that
/// *exist* in the sub-picture run from 1 to one short of its digit-place count,
/// a separator further left than that being the leading position and outside
/// the sequence. jsonata-js asks instead for the first `positions.len()`
/// multiples, which forces the set to be exactly `{f, 2f, …, nf}` and calls
/// `"####,##"` regular; the W3C QT3 suite pins both directions —
/// `format-number(642120, '####,##')` is "6421,20" (numberformat310) while
/// `format-number(642120, '##,##')` is "64,21,20" (numberformat312), and
/// `'#,#,#'` and `'##,#,#'` differ the same way (numberformat318/319).
fn regular_grouping(positions: &[usize], places: usize) -> usize {
    let Some(factor) = positions.iter().copied().reduce(gcd) else {
        return 0;
    };
    if factor == 0 {
        return 0;
    }
    let mut position = factor;
    while position < places {
        if !positions.contains(&position) {
            return 0;
        }
        position += factor;
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
    sp.int_grp_pos = grouping_positions(integer, false, fc);
    sp.regular_grouping = regular_grouping(&sp.int_grp_pos, count_places(integer, fc));
    sp.frac_grp_pos = grouping_positions(fraction, true, fc);

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
    let sign = locate_exponent(&runes, start, fc);
    // F&O 4.7.3: "The mantissa part of the sub-picture is defined as the part
    // that appears to the left of the exponent-separator-sign if there is one,
    // or the entire sub-picture otherwise. The exponent part of the subpicture
    // is defined as the part that appears to the right of the
    // exponent-separator-sign". Both are clipped to the active region, which
    // drops only prefix and suffix characters — passive, so they carry no
    // digit place and no grouping separator, and every rule below reads the
    // clipped part exactly as it reads the full one. A sign in the prefix
    // leaves the mantissa empty and one in the suffix leaves the exponent part
    // empty, which is precisely the rule each of those pictures violates.
    let (mantissa, exponent) = match sign {
        None => (active, None),
        Some(s) => {
            let mantissa_end = s.saturating_sub(start).min(active.len());
            let exponent_start = (s + 1).saturating_sub(start).min(active.len());
            (&active[..mantissa_end], Some(&active[exponent_start..]))
        }
    };
    // The decimal separator is looked for in the mantissa alone: one in the
    // exponent part is a D3093, not a mantissa split. When a single character
    // is configured as both separators the exponent wins, exactly as in
    // jsonata-js — "0.0" with `exponent-separator` "." is mantissa "0",
    // exponent "0", and jsntrs used to panic on the backwards slice range.
    // The fraction starts one character past the separator's *first*
    // character, however long the separator is: jsonata-js slices with
    // `substring(decimalPosition + 1)` regardless.
    let decimal = find_sub(mantissa, &fc.decimal_sep);
    // With no decimal separator in the mantissa the fraction part is the
    // *suffix*, not nothing: jsonata-js `splitParts` writes
    // `fractionalPart = suffix` there. Every rule the fraction feeds counts
    // only active characters, which the suffix scan has already excluded, so
    // the two agree unless an option makes a digit-family character passive
    // (`{"exponent-separator": "5"}`) — carry the reference's definition
    // rather than the coincidence.
    let (integer, fraction) = match decimal {
        // `substring` clamps, so an empty separator matching an empty
        // mantissa leaves both parts empty rather than panicking.
        Some(d) => (&mantissa[..d], mantissa.get(d + 1..).unwrap_or(&[])),
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
/// compare against and do arithmetic on. A separator the strip loops removed
/// leaves -1 behind and the padding arithmetic runs on it unchanged:
/// `$formatNumber(7, "0", {"decimal-separator": "0"})` is "007" because the
/// separator it appended was stripped again as a trailing zero-digit.
fn index_of(hay: &[char], needle: &str) -> isize {
    find_sub(hay, needle).map_or(-1, as_isize)
}

/// Insert `s` at `at`, as the bullets' `slice … join` does.
fn splice_in(sv: &mut Vec<char>, at: usize, s: &str) {
    sv.splice(at..at, s.chars());
}

/// Round the decimal `int_part`.`frac_part` to `dp` fractional digits,
/// half-to-even, and write it back out with exactly `dp` of them.
///
/// Only called when there are digits to drop, so `frac_part` is longer than
/// `dp`. Both parts are ASCII digits.
fn round_decimal_half_to_even(int_part: &str, frac_part: &str, dp: usize) -> String {
    let mut digits: Vec<u8> = int_part
        .bytes()
        .chain(frac_part.bytes().take(dp))
        .map(|b| b - b'0')
        .collect();
    let dropped = &frac_part.as_bytes()[dp..];
    let first = dropped[0] - b'0';
    let round_up = first > 5
        || (first == 5
            && (dropped[1..].iter().any(|&b| b != b'0')
                || digits.last().is_some_and(|d| d % 2 == 1)));
    if round_up {
        let mut carry = 1;
        for digit in digits.iter_mut().rev() {
            *digit += carry;
            if *digit == 10 {
                *digit = 0;
            } else {
                carry = 0;
                break;
            }
        }
        if carry == 1 {
            digits.insert(0, 1);
        }
    }
    let split = digits.len() - dp;
    let mut out = String::with_capacity(digits.len() + 1);
    for (index, digit) in digits.iter().enumerate() {
        if index == split && dp > 0 {
            out.push('.');
        }
        out.push(char::from(b'0' + digit));
    }
    out
}

/// The magnitude at `dp` decimal places, mapped into the picture's digit
/// family.
///
/// The digits are those of the *shortest* decimal that round-trips to `value`,
/// which is what F&O 4.7.5 asks for: the mantissa "is converted (if necessary)
/// to an xs:decimal value … If there are several such values that are
/// numerically equal to the mantissa … the one that is chosen should be one
/// with the smallest possible number of digits not counting leading or
/// trailing zeroes". Rust's `{}` for `f64` is exactly that decimal, in
/// positional notation, and it is the same digit sequence ECMAScript's
/// `Number::toString` produces. Printing the double's *exact* expansion
/// instead (`{:.dp$}`) leaks binary noise once the value is past 2^53:
/// `$formatNumber(1e25, "#####################")` was
/// "10000000000000000905969664" and is now "10000000000000000000000000", the
/// answer the W3C QT3 suite pins (numberformat60m). jsonata-js has a third
/// answer, `toFixed` switching to exponential notation at 1e21 (jsntrs-5kd).
///
/// Only the digits produced here are mapped into the family — a separator that
/// happens to be an ASCII digit is picture text and stays as written. An empty
/// `zero-digit` leaves no family to map into, and the reference's `join` drops
/// every `undefined` it looks up, so the digits disappear:
/// `$formatNumber(7, "#", {"zero-digit": ""})` is "".
fn make_string(value: f64, dp: usize, fc: &FmtChars) -> Vec<char> {
    let magnitude = value.abs();
    // Non-finite values never reach here (D3001 rejects them up front), but
    // "inf"/"NaN" are not digit strings, so keep them off the decimal path.
    let text = if magnitude.is_finite() {
        let shortest = format!("{magnitude}");
        let (int_part, frac_part) = shortest.split_once('.').unwrap_or((&shortest, ""));
        if frac_part.len() > dp {
            round_decimal_half_to_even(int_part, frac_part, dp)
        } else if dp == 0 {
            int_part.to_string()
        } else {
            format!(
                "{int_part}.{frac_part}{:0<width$}",
                "",
                width = dp - frac_part.len()
            )
        }
    } else {
        format!("{magnitude:.dp$}")
    };
    text.chars()
        .filter_map(|c| {
            if c.is_ascii_digit() {
                let zero = fc.zero_base?;
                char::from_u32(zero as u32 + (c as u32 - '0' as u32))
            } else {
                Some(c)
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
///
/// F&O 4.7.5: "For each integer N in the integer-part-grouping-positions list,
/// a grouping-separator character is inserted into the string immediately
/// after that digit that appears in the integer part of the number and has N
/// digits between it and the decimal-separator character, **if there is such a
/// digit**." A position at or past the number's digit count therefore places
/// nothing. jsonata-js reaches the same offsets through
/// `String.prototype.slice`, which wraps a negative index round to the end
/// instead, so `$formatNumber(7, "#,###,#")` is ",,7" there; the W3C test
/// suite pins the skip (`format-number(897, ',##0')` is "897", QT3
/// numberformat320) — jsntrs-0kg.
fn group_integer_part(sv: &mut Vec<char>, sp: &SubPicture, fc: &FmtChars, decimal_pos: isize) {
    if sp.regular_grouping > 0 {
        let interval = as_isize(sp.regular_grouping);
        // The extrapolated multiples of the interval that a digit exists for;
        // a missing decimal separator leaves this negative and the loop simply
        // does not run.
        let groups = (decimal_pos - 1).div_euclid(interval);
        for group in 1..=groups {
            // `group * interval <= decimal_pos - 1`, so this indexes a digit
            // already written and never needs clamping.
            let at = usize::try_from(decimal_pos - group * interval).unwrap_or(0);
            splice_in(sv, at, &fc.grouping_sep);
        }
        return;
    }
    // Irregular positions are applied literally, left to right, each insertion
    // shifting the ones after it along by the separator just written.
    let sep_len = as_isize(fc.grouping_sep.chars().count());
    let mut inserted = 0;
    for &pos in &sp.int_grp_pos {
        if as_isize(pos) >= decimal_pos {
            continue; // no digit that far from the decimal separator
        }
        let at = usize::try_from(decimal_pos + inserted - as_isize(pos)).unwrap_or(0);
        splice_in(sv, at, &fc.grouping_sep);
        inserted += sep_len;
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
        // `replace`, so a multi-character separator takes the place of the
        // one character `toFixed` wrote.
        Some(i) => {
            sv.splice(i..=i, fc.decimal_sep.chars());
        }
        None => sv.extend(fc.decimal_sep.chars()),
    }
    // Strip every leading and trailing zero-digit. The comparison is against
    // the whole option value, so a multi-character `zero-digit` never strips.
    // The decimal separator otherwise stops both runs, so this trims the
    // integer part on the left and the fraction on the right; bullets 8 and 9
    // pad back to the minima.
    let leading = sv
        .iter()
        .take_while(|&&c| char_is(Some(c), &fc.zero_digit))
        .count();
    sv.drain(..leading);
    while char_is(sv.last().copied(), &fc.zero_digit) && !sv.is_empty() {
        sv.pop();
    }

    // Bullets 8 and 9. The pad counts are in characters and each unit writes
    // the whole `zero-digit` value.
    let decimal_pos = index_of(&sv, &fc.decimal_sep);
    let pad_left = as_isize(sp.min_int) - decimal_pos;
    let pad_right = as_isize(sp.min_frac) - (as_isize(sv.len()) - decimal_pos - 1);
    for _ in 0..pad_left.max(0) {
        splice_in(&mut sv, 0, &fc.zero_digit);
    }
    for _ in 0..pad_right.max(0) {
        sv.extend(fc.zero_digit.chars());
    }

    // Bullet 10.
    let decimal_pos = index_of(&sv, &fc.decimal_sep);
    group_integer_part(&mut sv, sp, fc, decimal_pos);

    // Bullet 11, the mirror of bullet 10: "a grouping-separator character is
    // inserted into the string immediately *before* that digit that appears in
    // the fractional part of the number and has N digits between it and the
    // decimal-separator character, if there is such a digit" (F&O 4.7.5). Each
    // insertion shifts the ones after it along, and a position at or past the
    // fraction's digit count places nothing.
    let sep_len = fc.decimal_sep.chars().count();
    let decimal_pos = index_of(&sv, &fc.decimal_sep);
    if let Ok(dp) = usize::try_from(decimal_pos) {
        let frac_start = dp + sep_len;
        let frac_digits = sv.len().saturating_sub(frac_start);
        let grp_len = fc.grouping_sep.chars().count();
        let mut inserted = 0;
        for &pos in &sp.frac_grp_pos {
            if pos >= frac_digits {
                continue; // no digit that far from the decimal separator
            }
            splice_in(&mut sv, frac_start + pos + inserted, &fc.grouping_sep);
            inserted += grp_len;
        }
    }

    // Bullet 12: "If there is no decimal-separator character in the
    // sub-picture, or if there are no digits to the right of the
    // decimal-separator character in the string, then the decimal-separator
    // character is removed from the string (it will be the rightmost character
    // in the string)" (F&O 4.7.5). What goes is the separator, and only when
    // nothing follows it — jsonata-js drops the last character instead
    // (`substring(0, length - 1)`), which eats a *digit* when a picture with no
    // separator of its own gained a fractional digit from the 4.7.4 exponent
    // adjustment: `$formatNumber(1234.5678, "#e0")` is "0.e4" there and
    // "0.1e4" here, the shape the W3C test suite pins
    // (`format-number(0.2, '#e0')` is "0.2e0", QT3 numberformat231) —
    // jsntrs-0kg.
    if let Ok(dp) = usize::try_from(index_of(&sv, &fc.decimal_sep))
        && dp + sep_len == sv.len()
    {
        sv.drain(dp..);
    }

    // Bullet 13.
    if let Some(exponent) = exponent {
        let mut digits = make_string(f64::from(exponent), 0, fc);
        for _ in digits.len()..sp.min_exp {
            splice_in(&mut digits, 0, &fc.zero_digit);
        }
        sv.extend(fc.exponent_sep.chars());
        if exponent < 0 {
            sv.extend(fc.minus_sign.chars());
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
/// carries a pattern separator and otherwise a copy with the `minus-sign`
/// character glued to the prefix — F&O 4.7.4, "If the picture string contains
/// only one sub-picture, the prefix for the negative sub-picture is set by
/// concatenating the minus-sign character and the prefix for the positive
/// sub-picture (if any), in that order."
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
        np.prefix = format!("{}{}", fc.minus_sign, pos_pic.prefix);
        np
    };
    Ok((pos_pic, neg_pic))
}

/// Bullets 2, 3 and 14: pick the sub-picture, apply the scaling factor, and
/// wrap the formatted digits in the prefix and suffix.
///
/// F&O 4.7.5: "the positive sub-picture and its associated variables are used
/// if the input number is positive, and the negative sub-picture and its
/// associated variables are used if it is negative. **For xs:double and
/// xs:float, negative zero is taken as negative, positive zero as positive.**
/// For xs:decimal and xs:integer, the positive sub-picture is used for zero."
/// A JSONata number is an IEEE 754 double, and a zero that carries a sign bit
/// can only be one — xs:decimal has no negative zero to distinguish — so the
/// first sentence is the one that applies and `-0.0` takes the negative
/// sub-picture. The W3C QT3 suite pins it: `format-number(-0.0e0, '0.0e0')`
/// is "-0.0e0" (fn/format-number.xml numberformat322) against "0.0e0" for
/// `0.0e0` (numberformat321). jsonata-js branches on `value >= 0` and gives
/// negative zero the positive sub-picture; jsntrs followed it (jsntrs-p0v.26)
/// and no longer does (jsntrs-204).
///
/// Only the *sub-picture choice* moves. The digits themselves are unaffected:
/// `make_string` formats `value.abs()`, so the minus sign in the output is the
/// negative prefix (F&O 4.7.4, "the prefix for the negative sub-picture is set
/// by concatenating the minus-sign character and the prefix for the positive
/// sub-picture"), never a stray sign inside the digit string. And this is not
/// the number-output layer: `$string(-0)` and JSON both still print "0"
/// (invariant 5, jsntrs-p0v.5).
pub(crate) fn format_number_value(
    n: f64,
    pos_pic: &SubPicture,
    neg_pic: &SubPicture,
    fc: &FmtChars,
) -> String {
    // `n < 0.0` is false for -0.0; the sign bit is the whole question here.
    // Non-finite values never reach this function (D3001 rejects them, and the
    // fast path hands them back), so `is_sign_negative` has no NaN to trip on.
    let sp = if n.is_sign_negative() {
        neg_pic
    } else {
        pos_pic
    };
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

    /// F&O 4.7.5 picks the sub-picture by sign: "the positive sub-picture and
    /// its associated variables are used if the input number is positive, and
    /// the negative sub-picture ... if it is negative. For xs:double and
    /// xs:float, negative zero is taken as negative, positive zero as
    /// positive." A JSONata number is a double, so `-0.0` takes the negative
    /// sub-picture — W3C QT3 fn/format-number.xml numberformat322,
    /// `format-number(-0.0e0, '0.0e0')` is "-0.0e0", against numberformat321's
    /// "0.0e0" for `0.0e0`. jsonata-js branches on `value >= 0` and formats
    /// negative zero through the positive sub-picture; jsntrs followed it
    /// (jsntrs-p0v.26) until jsntrs-204.
    ///
    /// What the digits do is unchanged, and was the real bug p0v.26 fixed: the
    /// sign `format!("{:.p$}")` wrote used to reach the picture machinery, so
    /// jsntrs answered "-0.00" from the *digit string* and with grouping put
    /// the minus in a group — "9,9,99.99" gave "0,0,-0.00". `make_string`
    /// formats the magnitude; every minus below is the negative prefix.
    #[test]
    fn negative_zero_takes_the_negative_sub_picture() {
        assert_eq!(fmt(-0.0, "0.00"), "-0.00");
        assert_eq!(fmt(-0.0, "0.0"), "-0.0");
        assert_eq!(fmt(-0.0, "9,9,99.99"), "-0,0,00.00");
        assert_eq!(fmt(-0.0, "#,##0.00"), "-0.00");
        assert_eq!(fmt(-0.0, "0.00%"), "-0.00%");
        assert_eq!(fmt(-0.0, "‰0.00"), "-‰0.00");
        // With two sub-pictures the second one is used as written, minus sign
        // and all: 4.7.4 only prepends `minus-sign` when the picture has a
        // single sub-picture.
        assert_eq!(fmt(-0.0, "0.00;(0.00)"), "(0.00)");
        assert_eq!(fmt(-0.0, "0.00;-0.00"), "-0.00");
        // numberformat322/321 themselves.
        assert_eq!(fmt(-0.0, "0.0e0"), "-0.0e0");
        assert_eq!(fmt(0.0, "0.0e0"), "0.0e0");
        assert_eq!(fmt(-0.0, "00.000e0"), "-00.000e0");
        // Positive zero is unaffected.
        assert_eq!(fmt(0.0, "0.00"), "0.00");
        assert_eq!(fmt(0.0, "0.00;(0.00)"), "0.00");
        // A number that really is negative still takes the negative picture.
        assert_eq!(fmt(-0.000_000_001, "0.00"), "-0.00");
        assert_eq!(fmt(-0.4, "0"), "-0");
        assert_eq!(fmt(-1.0, "0.00;(0.00)"), "(1.00)");
        // The minus lives in the prefix, so a `minus-sign` option moves it and
        // the digits never carry one.
        assert_eq!(
            fmt_opts(-0.0, "9,9,99.99", r#"{"minus-sign": "@"}"#),
            Ok("@0,0,00.00".to_string())
        );
    }

    /// The number-output layer is a different thing from which sub-picture
    /// `$formatNumber` selects: `$string(-0)` and the JSON writer still print
    /// "0" (invariant 5, jsntrs-p0v.5), and `$formatNumber` reads the sign bit
    /// only to choose between the two sub-pictures.
    #[test]
    fn negative_zero_still_stringifies_as_zero() {
        assert_eq!(Value::Number(-0.0).stringify(false).unwrap(), "0");
        let mut buf = Vec::new();
        Value::Number(-0.0).write_json(&mut buf);
        assert_eq!(String::from_utf8(buf).unwrap(), "0");
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

    /// `decimal-separator` and `grouping-separator` are strings, matched two
    /// ways: a picture character is that separator only when the option is
    /// exactly that one character, and the value is searched for and emitted
    /// whole everywhere else. So a multi-character value turns the ordinary
    /// separator into a passive character (D3086) and lands in the output as
    /// a run. Expected values verified against jsonata 2.2.2 (jsntrs-2px);
    /// jsntrs used to ignore any value that was not a single character and
    /// carry on with the default, formatting `"0.0"` as "7.0".
    #[test]
    fn separator_options_are_matched_and_emitted_as_strings() {
        let ab = r#"{"decimal-separator": "ab"}"#;
        assert_eq!(fmt_opts(7.0, "0.0", ab), Err("D3086"));
        assert_eq!(fmt_opts(7.0, "0ab0", ab), Err("D3086"));
        // The value is appended by bullet 7 whether the picture mentions it or
        // not, and bullet 12 removes it again when no digit follows it. The
        // multi-character values here are outside XPath, which gives every
        // decimal-format property a single character; what the answers pin is
        // that bullet 12 removes *the separator* and not one character of it,
        // which is why jsonata-js keeps a stray "a" ("07a", "8a") and leaves
        // "7abab" standing (jsntrs-0kg).
        assert_eq!(fmt_opts(7.0, "00", ab), Ok("07".to_string()));
        assert_eq!(fmt_opts(7.0, "0ab", ab), Ok("7ab".to_string()));
        assert_eq!(fmt_opts(7.5, "0", ab), Ok("8".to_string()));
        // An empty value occurs everywhere, so the picture holds more than
        // one instance of it.
        assert_eq!(
            fmt_opts(7.0, "00", r#"{"decimal-separator": ""}"#),
            Err("D3081")
        );
        // A separator that is also a digit is stripped again as a trailing
        // zero-digit, leaving the padding to work from indexOf's -1.
        assert_eq!(
            fmt_opts(7.0, "0", r#"{"decimal-separator": "0"}"#),
            Ok("007".to_string())
        );

        let gs = r#"{"grouping-separator": "ab"}"#;
        assert_eq!(fmt_opts(1234.0, "#,##0", gs), Err("D3086"));
        assert_eq!(fmt_opts(1234.0, "#ab##0", gs), Err("D3086"));
        assert_eq!(fmt_opts(1234.0, "0000", gs), Ok("1234".to_string()));
        // The suffix "ab" is the picture's fractional part, so it records a
        // grouping position — which places nothing, the fraction having no
        // digits at all. jsonata-js slices at the end of the string instead
        // and answers "1234.aab" (jsntrs-0kg).
        assert_eq!(fmt_opts(1234.0, "0000ab", gs), Ok("1234ab".to_string()));
        // Two adjacent separators are searched as the doubled string, which
        // an empty value always matches.
        assert_eq!(
            fmt_opts(1234.0, "0000", r#"{"grouping-separator": ""}"#),
            Err("D3089")
        );
        // A single-character value still behaves as it always did.
        assert_eq!(
            fmt_opts(1_234_567.0, "#'###", r#"{"grouping-separator": "'"}"#),
            Ok("1'234'567".to_string())
        );
    }

    /// `zero-digit` gives the digit family its base with `charCodeAt(0)`, but
    /// pads and strips as a whole string: a multi-character value never
    /// strips a zero and writes all of itself per padded digit, and an empty
    /// one leaves no family at all — nothing in the picture is a digit, and
    /// the digits that survive validation map to nothing. Expected values
    /// verified against jsonata 2.2.2 (jsntrs-2px), which does *not* throw
    /// the TypeError the issue reported: that was an older jsonata.
    #[test]
    fn zero_digit_takes_its_family_from_the_first_character() {
        let ab = r#"{"zero-digit": "ab"}"#;
        assert_eq!(fmt_opts(7.0, "#", ab), Ok("h".to_string()));
        assert_eq!(fmt_opts(0.5, "#.#", ab), Ok("a.f".to_string()));
        assert_eq!(fmt_opts(7.0, "aaa", ab), Ok("ababh".to_string()));
        assert_eq!(fmt_opts(7.0, "aa.aa", ab), Ok("abh.aa".to_string()));
        assert_eq!(
            fmt_opts(1_234_567.0, "a,aaa", ab),
            Ok("b,cde,fgh".to_string())
        );
        // '0' is not in the family any more, so it is passive text.
        assert_eq!(fmt_opts(7.5, "0.0", ab), Err("D3085"));

        let empty = r#"{"zero-digit": ""}"#;
        assert_eq!(fmt_opts(7.0, "#", empty), Ok(String::new()));
        assert_eq!(fmt_opts(7.0, "#.#", empty), Ok(String::new()));
    }

    /// An empty `exponent-separator` matches at the start of the search, so
    /// the mantissa comes out empty and every picture is an error — D3085
    /// when the rest of the active region is digit-family, D3093 otherwise.
    /// Expected codes verified against jsonata 2.2.2 (jsntrs-2px); jsntrs
    /// used to treat the option as "no separator" and format normally.
    #[test]
    fn an_empty_exponent_separator_empties_the_mantissa() {
        let empty = r#"{"exponent-separator": ""}"#;
        assert_eq!(fmt_opts(7.0, "0", empty), Err("D3093"));
        assert_eq!(fmt_opts(7.0, "00", empty), Err("D3085"));
        assert_eq!(fmt_opts(1234.5678, "0.0e0", empty), Err("D3093"));
        assert_eq!(fmt_opts(1234.5678, "#,##0.00", empty), Err("D3093"));
        // A prefix is where the two part company: jsonata-js measures the
        // match from the start of the sub-picture and then indexes the
        // active part with it, so "$00" is a D3093 there and a D3085 here —
        // the same off-by-prefix deviation as jsntrs-p0v.23.
        assert_eq!(fmt_opts(7.0, "$00", empty), Err("D3085"));
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

    /// F&O 4.7.1 gives the decimal format a `minus-sign` property, "the
    /// character used as a minus sign in the formatted number if there is no
    /// subpicture for formatting negative numbers"; 4.7.4 concatenates it with
    /// the positive prefix to make the negative one, and 4.7.5 bullet 13(b)
    /// writes it in front of a negative exponent. jsntrs wrote the constant
    /// "-" in both places and never read the option (jsntrs-12g).
    #[test]
    fn minus_sign_option_is_honoured() {
        let at = r#"{"minus-sign": "@"}"#;
        assert_eq!(fmt_opts(-7.0, "0", at), Ok("@7".to_string()));
        assert_eq!(fmt_opts(-7.0, "$0.00", at), Ok("@$7.00".to_string()));
        assert_eq!(
            fmt_opts(0.000_012_345, "0.0e0", at),
            Ok("1.2e@5".to_string())
        );
        // "if there is no subpicture for formatting negative numbers": with
        // one, the property is not used for the mantissa.
        assert_eq!(fmt_opts(-7.0, "0;(0)", at), Ok("(7)".to_string()));
        // The default is still "-" — for both writers.
        assert_eq!(fmt(-7.0, "0"), "-7");
        assert_eq!(fmt(0.000_012_345, "0.0e0"), "1.2e-5");
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

    /// F&O 4.7.3: an exponent separator "is treated as an
    /// exponent-separator-sign if it is both preceded and followed within the
    /// sub-picture by an active character. Otherwise, it is treated as a
    /// passive character" — and exponent separators are themselves active
    /// characters, so the test is answered by scanning the sub-picture for one
    /// on each side, separators included. jsonata-js reads a separator sitting
    /// immediately after the active region as opening an (empty, and therefore
    /// invalid) exponent part and answers D3093 for `"0.0e"`, `"0e"`,
    /// `"e0.0e"` and `"9.9999eDog"`; the W3C QT3 suite pins the last of those
    /// as "12345.6780eDog" (fn/format-number.xml numberformat113)
    /// — jsntrs-g0f.
    #[test]
    fn a_separator_with_no_active_character_after_it_is_passive() {
        assert_eq!(fmt(1234.5678, "e0.0"), "e1234.6");
        assert_eq!(fmt(0.234, "e0.0"), "e0.2");
        // Still a sign inside the mantissa, and still emitted.
        assert_eq!(fmt(1234.5678, "0.0e0"), "1.2e3");
        // The trailing separator is followed by nothing active, so it is
        // suffix text and the sign is the first one.
        assert_eq!(fmt(1234.5678, "0.0e0e"), "1.2e3e");
        // A trailing separator is suffix text, with or without more after it.
        assert_eq!(fmt(12_345.678, "9.9999eDog"), "12345.6780eDog");
        assert_eq!(fmt(1234.5678, "0.0e"), "1234.6e");
        assert_eq!(fmt(1234.5678, "e0.0e"), "e1234.6e");
        assert_eq!(fmt(1234.5678, "0e"), "1235e");
        // Two separators in a row: each is the other's active neighbour, so
        // the first is the sign and its exponent part holds no digit-family
        // character. Reading the "followed by" test against the active region
        // instead — whose edges exclude exponent separators — made these two
        // format as "1234.6ee" and "1235ee", the regression that kept the
        // wave-6 attempt at this fix out of the tree.
        assert_eq!(
            fmt_args(&[Value::Number(1234.5678), Value::String("0.0ee".into())]),
            Err("D3093")
        );
        assert_eq!(
            fmt_args(&[Value::Number(1234.5678), Value::String("0ee".into())]),
            Err("D3093")
        );
        // Symmetrically at the front: the second separator is preceded by the
        // first, so it is the sign; the mantissa "e" has no digit (D3085) and
        // the exponent part "0.0" an active non-digit (D3093).
        assert_eq!(
            fmt_args(&[Value::Number(1234.5678), Value::String("ee0.0".into())]),
            Err("D3093")
        );
        // A picture of nothing but separators has no mantissa digit at all.
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
        assert_eq!(
            fmt_opts(1234.5678, "0.0E", e_sep),
            Ok("1234.6E".to_string())
        );
        assert_eq!(
            fmt_opts(1234.5678, "e0.0", e_sep),
            Ok("e1234.6".to_string())
        );
    }

    /// A sub-picture with no active character at all is a D3085: its mantissa
    /// is the whole sub-picture — an exponent separator needs an active
    /// character on each side to be a sign — and it holds neither an optional
    /// digit character nor a member of the decimal digit family, which is the
    /// rule D3085 names. jsonata-js 2.2.2 reports D3086, whose rule is about a
    /// passive character *between* two active ones and cannot be violated by a
    /// picture with no active character, and D3093 for `"e"`, whose rule is
    /// about the exponent part of a sub-picture that has an
    /// exponent-separator-sign. Deliberately not ported (jsntrs-g0f).
    #[test]
    fn a_picture_with_no_active_character_is_d3085() {
        for picture in ["", "-", "-$", "%", "e"] {
            assert_eq!(
                fmt_args(&[Value::Number(7.0), Value::String(picture.into())]),
                Err("D3085"),
                "{picture:?}"
            );
        }
        // `e` is passive here too: the option moved the separator to `.`.
        assert_eq!(
            fmt_opts(7.0, "e", r#"{"exponent-separator": "."}"#),
            Err("D3085")
        );
        // No character of "00" is in the `ab…` digit family, so none is active.
        assert_eq!(fmt_opts(7.0, "00", r#"{"zero-digit": "ab"}"#), Err("D3085"));
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

    /// The three 4.7.4 conditions, on the position sets a picture can
    /// produce. The third one — "Every position in the integer part of the
    /// sub-picture that is a positive integer multiple of G is occupied by a
    /// grouping-separator" — is why the digit-place count is an argument: the
    /// positions that exist in a sub-picture with `places` digit places run
    /// from 1 to `places - 1`, the leading position `places` being allowed but
    /// not required (`",##0"` is regular with G = 3).
    ///
    /// The positions are distinct, so a set is all the function needs: two
    /// grouping separators in the integer part are either adjacent — D3089
    /// rejects that anywhere in the sub-picture — or separated by at least one
    /// character, and inside the integer part the only characters validation
    /// leaves are digit-family, the digit placeholder and the grouping
    /// separator itself (D3086 rejects passive characters in the active
    /// region, a second decimal separator is D3081, the exponent separator
    /// ends the mantissa, and the pattern separator has already split the
    /// picture). So any two separators have a different number of digit places
    /// to their right.
    #[test]
    fn regular_grouping_follows_the_three_conditions() {
        // No separator at all: condition 1 fails.
        assert_eq!(regular_grouping(&[], 4), 0);
        // A separator with no digit place to its right gives position 0, which
        // is not a positive multiple of anything.
        assert_eq!(regular_grouping(&[0], 3), 0);
        // Every interior multiple of the gcd is occupied.
        assert_eq!(regular_grouping(&[2], 4), 2); // "##,##"
        assert_eq!(regular_grouping(&[3], 4), 3); // "#,###"
        assert_eq!(regular_grouping(&[4, 2], 6), 2); // "##,##,##"
        assert_eq!(regular_grouping(&[2, 1], 3), 1); // "#,#,#"
        assert_eq!(regular_grouping(&[3], 3), 3); // ",##0" — leading position
        // Interior multiples that no separator occupies.
        assert_eq!(regular_grouping(&[2], 6), 0); // "####,##"
        assert_eq!(regular_grouping(&[2], 5), 0); // "###,##"
        assert_eq!(regular_grouping(&[2, 1], 4), 0); // "##,#,#"
        assert_eq!(regular_grouping(&[4, 2], 8), 0); // "####,##,##"
        assert_eq!(regular_grouping(&[3, 2], 4), 0); // "9,9,99"
    }

    /// The same rule end to end, on the W3C QT3 suite's own regularity
    /// pictures (`fn/format-number.xml` numberformat310-320, "Single grouping
    /// separator in pattern: regular or not?"). jsonata-js asks only for the
    /// first `positions.len()` multiples of the gcd, so it calls "####,##" and
    /// "##,#,#" regular and answers "64,21,20" and "6,4,2,1,2,0" for the first
    /// two of these.
    #[test]
    fn grouping_regularity_matches_the_w3c_suite() {
        for (picture, expected) in [
            ("####,##", "6421,20"),   // numberformat310
            ("###,##", "6421,20"),    // numberformat311
            ("##,##", "64,21,20"),    // numberformat312
            ("#,##", "64,21,20"),     // numberformat313
            ("0000,00", "6421,20"),   // numberformat314
            ("000,00", "6421,20"),    // numberformat315
            ("00,00", "64,21,20"),    // numberformat316
            ("0,00", "64,21,20"),     // numberformat317
            ("#,#,#", "6,4,2,1,2,0"), // numberformat318
            ("##,#,#", "6421,2,0"),   // numberformat319
        ] {
            assert_eq!(fmt(642_120.0, picture), expected, "{picture}");
        }
        // numberformat320: a leading grouping separator is a position like any
        // other, and extrapolates.
        assert_eq!(fmt(3_000_000.0, ",##0"), "3,000,000");
    }

    /// End to end on the pictures that separated the candidate implementations
    /// of the *old* rule (jsntrs-4fr). Only "####,##,##" moves: its positions
    /// {4,2} leave the interior position 6 of an eight-place integer part
    /// unoccupied, so 4.7.4's third condition fails and the two positions are
    /// applied literally.
    #[test]
    fn grouping_regularity_agrees_on_the_candidate_separators() {
        assert_eq!(fmt(1_234_567_890.0, "####,##,##"), "123456,78,90");
        assert_eq!(fmt(1_234_567_890.0, "#,####,##,##"), "12,3456,78,90");
        assert_eq!(fmt(1_234_567_890.0, "##,####,##"), "1234,5678,90");
        assert_eq!(fmt(1_234_567_890.0, "#,##,####"), "1234,56,7890");
        assert_eq!(fmt(1_234_567.0, "##,##,##"), "1,23,45,67");
        assert_eq!(fmt(123_456.0, "#,##,#"), "123,45,6");
    }

    /// Irregular grouping positions are applied literally, and a position with
    /// no digit that far from the decimal separator places nothing: F&O 4.7.5
    /// inserts a separator "immediately after that digit that appears in the
    /// integer part of the number and has N digits between it and the
    /// decimal-separator character, *if there is such a digit*". "#,###,#"
    /// asks for separators 4 and 1 digit places from the right, so a
    /// four-digit number takes only the second. jsonata-js reaches the
    /// insertion point with `String.prototype.slice`, whose negative index
    /// wraps round to the end of the string, and answers ",,7" and ",123,5"
    /// (jsntrs-0kg); the W3C test suite pins the skip in QT3 numberformat320,
    /// `format-number(897, ',##0')` = "897".
    #[test]
    fn grouping_positions_past_the_number_are_skipped() {
        assert_eq!(fmt(7.0, "#,###,#"), "7");
        assert_eq!(fmt(1234.5678, "#,###,#"), "123,5");
        assert_eq!(fmt(1234.5678, "9,9,99.99"), "1,2,34.57");
        assert_eq!(fmt(897.0, ",##0"), "897");
        assert_eq!(fmt(2001.0, ",##0"), "2,001");
    }

    /// The fractional part gets one separator per grouping character in it,
    /// counted from the decimal separator outwards, and the same "if there is
    /// such a digit" guard applies. Expected values from the W3C test suite:
    /// QT3 numberformat157 `format-number(12345.6789012345, '#.#,##,#')` =
    /// "12345.6,78,9" and numberformat158 `'#.##,##,##'` = "12345.67,89,01".
    /// jsonata-js walks the *integer* part looking for the fraction's later
    /// separators, so it emits at most the fraction's first position and
    /// answers "12345.6,789" (jsntrs-0kg).
    #[test]
    fn fractional_grouping_places_one_separator_per_position() {
        assert_eq!(fmt(12_345.678_901_234_5, "#.#,##,#"), "12345.6,78,9");
        assert_eq!(fmt(12_345.678_901_234_5, "#.##,##,##"), "12345.67,89,01");
        assert_eq!(fmt(1234.5678, "0.0,0,0"), "1234.5,6,8");
        // No digit two places into a one-digit fraction: nothing is placed.
        assert_eq!(fmt(1234.5678, "#0.0,"), "1234.6");
    }

    /// The 4.7.4 exponent adjustment gives a picture with no decimal separator
    /// of its own a fractional digit, and bullet 12 then removes the separator
    /// rather than the digit: "the decimal-separator character is removed from
    /// the string". jsonata-js takes `substring(0, length - 1)` and answers
    /// "0.e4". Expected values from the W3C test suite, QT3 numberformat231
    /// (`format-number(0.2, '#e0')` = "0.2e0") and numberformat232
    /// (`format-number(1.2, '#e0')` = "0.1e1") — jsntrs-0kg.
    #[test]
    fn a_picture_without_a_decimal_separator_keeps_its_exponent_digit() {
        assert_eq!(fmt(0.2, "#e0"), "0.2e0");
        assert_eq!(fmt(1.2, "#e0"), "0.1e1");
        assert_eq!(fmt(1234.5678, "#e0"), "0.1e4");
        assert_eq!(fmt(1.3, "#e00"), "0.1e01");
        // The 4.7.4 note's own example, which has a separator and keeps it.
        assert_eq!(fmt(0.123, "#.e9"), "0.1e0");
        // With no fractional digit the separator still goes.
        assert_eq!(fmt(99.5, "#."), "100");
        assert_eq!(fmt(12_345.678, "999e9"), "123e2");
    }
}
