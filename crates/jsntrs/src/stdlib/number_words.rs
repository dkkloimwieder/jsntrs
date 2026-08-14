//! Shared roman-numeral, alphabetic, and number-to-words machinery.
//!
//! Single home for the spelled-out-number tables and conversions used by
//! `$formatInteger` (format_integer.rs), `$parseInteger` (parse_integer.rs),
//! and datetime picture formatting/parsing (datetime.rs). The two word-number
//! *parsers* stay with their callers — `$parseInteger` consumes a whole
//! string strictly while datetime scans a lenient prefix — but both build on
//! the tables and value helpers here.

/// Cardinal words for 0–19. Index 0 is empty: zero is special-cased by
/// [`int_to_words`], and the parsers treat it separately too.
pub(crate) const ONES: [&str; 20] = [
    "",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];

/// Cardinal words for the tens 20–90 (indices 2–9).
pub(crate) const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

const SCALES: &[(&str, i64)] = &[
    ("trillion", 1_000_000_000_000),
    ("billion", 1_000_000_000),
    ("million", 1_000_000),
    ("thousand", 1_000),
];

// ── Picture strings ──────────────────────────────────────────────────────────

/// Split an XPath integer picture into (format token, modifier).
///
/// The modifier follows a `;`; its default is `"c"` (cardinal).
pub(crate) fn split_picture_modifier(picture: &str) -> (&str, &str) {
    if let Some(idx) = picture.find(';') {
        (&picture[..idx], &picture[idx + 1..])
    } else {
        (picture, "c")
    }
}

// ── Unicode decimal digit families ───────────────────────────────────────────

/// Zero code points of the Unicode decimal digit families recognized in
/// picture strings (XPath `format-integer` digit-family support).
const UNICODE_ZEROS: &[char] = &[
    '\u{0660}', '\u{06F0}', '\u{07C0}', '\u{0966}', '\u{09E6}', '\u{0A66}', '\u{0AE6}', '\u{0B66}',
    '\u{0BE6}', '\u{0C66}', '\u{0CE6}', '\u{0D66}', '\u{0DE6}', '\u{0E50}', '\u{0ED0}', '\u{0F20}',
    '\u{1040}', '\u{1090}', '\u{17E0}', '\u{1810}', '\u{1946}', '\u{19D0}', '\u{1A80}', '\u{1A90}',
    '\u{1B50}', '\u{1BB0}', '\u{1C40}', '\u{1C50}', '\u{A620}', '\u{A8D0}', '\u{A900}', '\u{A9D0}',
    '\u{A9F0}', '\u{AA50}', '\u{ABF0}', '\u{FF10}',
];

/// The zero of `c`'s digit family, if `c` is a non-ASCII decimal digit.
pub(crate) fn unicode_digit_zero(c: char) -> Option<char> {
    let c_u32 = c as u32;
    UNICODE_ZEROS
        .iter()
        .find(|&&z| (z as u32..=z as u32 + 9).contains(&c_u32))
        .copied()
}

pub(crate) fn is_unicode_digit(c: char) -> bool {
    unicode_digit_zero(c).is_some()
}

// ── Roman numerals ───────────────────────────────────────────────────────────

/// Value of a single roman numeral character, case-insensitive.
pub(crate) fn roman_value(c: char) -> Option<i64> {
    match c.to_ascii_uppercase() {
        'I' => Some(1),
        'V' => Some(5),
        'X' => Some(10),
        'L' => Some(50),
        'C' => Some(100),
        'D' => Some(500),
        'M' => Some(1000),
        _ => None,
    }
}

pub(crate) fn to_roman(mut n: i64, upper: bool) -> String {
    const VALS: &[(i64, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    if n <= 0 {
        return String::new();
    }
    let mut result = String::new();
    for &(v, sym) in VALS {
        while n >= v {
            result.push_str(sym);
            n -= v;
        }
    }
    if upper { result } else { result.to_lowercase() }
}

// ── Alphabetic sequences (a, b, …, z, aa, ab, …) ─────────────────────────────

pub(crate) fn to_alphabetic(mut n: i64, base: char) -> String {
    if n <= 0 {
        return String::new();
    }
    let mut result: Vec<char> = Vec::new();
    while n > 0 {
        n -= 1;
        result.push(char::from_u32(base as u32 + (n % 26) as u32).unwrap_or(base));
        n /= 26;
    }
    result.reverse();
    result.into_iter().collect()
}

// ── Number to words ──────────────────────────────────────────────────────────

pub(crate) fn int_to_words(n: i64) -> String {
    if n == 0 {
        return "zero".to_string();
    }
    if n < 0 {
        return format!("minus {}", int_to_words(-n));
    }
    to_words(n)
}

pub(crate) fn int_to_words_ordinal(n: i64) -> String {
    apply_ordinal_word(&int_to_words(n))
}

fn below_thousand(n: i64) -> String {
    if n == 0 {
        return String::new();
    }
    if n < 20 {
        return ONES[n as usize].to_string();
    }
    if n < 100 {
        if n % 10 == 0 {
            return TENS[(n / 10) as usize].to_string();
        }
        return format!("{}-{}", TENS[(n / 10) as usize], ONES[(n % 10) as usize]);
    }
    // n < 1000
    let rem = n % 100;
    if rem == 0 {
        return format!("{} hundred", ONES[(n / 100) as usize]);
    }
    format!(
        "{} hundred and {}",
        ONES[(n / 100) as usize],
        below_thousand(rem)
    )
}

fn to_words(n: i64) -> String {
    if n == 0 {
        return String::new();
    }
    if n < 1000 {
        return below_thousand(n);
    }
    for &(name, val) in SCALES {
        if n < val {
            continue;
        }
        let q = n / val;
        let rem = n % val;
        let q_word = to_words(q);
        let mut result = format!("{q_word} {name}");
        if rem > 0 {
            let rem_word = to_words(rem);
            if rem < 100 {
                result.push_str(" and ");
                result.push_str(&rem_word);
            } else {
                result.push_str(", ");
                result.push_str(&rem_word);
            }
        }
        return result;
    }
    below_thousand(n)
}

// ── Ordinals ─────────────────────────────────────────────────────────────────

/// English ordinal suffix for a number: 1 → "st", 2 → "nd", 11 → "th", …
pub(crate) fn ordinal_suffix(n: i64) -> &'static str {
    let abs = n.unsigned_abs();
    let mod100 = abs % 100;
    let mod10 = abs % 10;
    if (11..=13).contains(&mod100) {
        return "th";
    }
    match mod10 {
        1 => "st",
        2 => "nd",
        3 => "rd",
        _ => "th",
    }
}

/// Turn a cardinal word phrase into its ordinal ("forty-two" → "forty-second").
pub(crate) fn apply_ordinal_word(word: &str) -> String {
    const ORDINALS: &[(&str, &str)] = &[
        ("one", "first"),
        ("two", "second"),
        ("three", "third"),
        ("four", "fourth"),
        ("five", "fifth"),
        ("six", "sixth"),
        ("seven", "seventh"),
        ("eight", "eighth"),
        ("nine", "ninth"),
        ("ten", "tenth"),
        ("eleven", "eleventh"),
        ("twelve", "twelfth"),
        ("thirteen", "thirteenth"),
        ("fourteen", "fourteenth"),
        ("fifteen", "fifteenth"),
        ("sixteen", "sixteenth"),
        ("seventeen", "seventeenth"),
        ("eighteen", "eighteenth"),
        ("nineteen", "nineteenth"),
        ("twenty", "twentieth"),
        ("thirty", "thirtieth"),
        ("forty", "fortieth"),
        ("fifty", "fiftieth"),
        ("sixty", "sixtieth"),
        ("seventy", "seventieth"),
        ("eighty", "eightieth"),
        ("ninety", "ninetieth"),
        ("hundred", "hundredth"),
        ("thousand", "thousandth"),
        ("million", "millionth"),
        ("billion", "billionth"),
        ("trillion", "trillionth"),
    ];
    // Replace only the last word; separators are single-byte (' ' or '-').
    let (prefix, sep, last) = if let Some(pos) = word.rfind([' ', '-']) {
        (&word[..pos], &word[pos..=pos], &word[pos + 1..])
    } else {
        ("", "", word)
    };
    for &(cardinal, ordinal) in ORDINALS {
        if last == cardinal {
            return format!("{prefix}{sep}{ordinal}");
        }
    }
    if let Some(stem) = last.strip_suffix('y') {
        return format!("{prefix}{sep}{stem}ieth");
    }
    format!("{prefix}{sep}{last}th")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words() {
        assert_eq!(int_to_words(0), "zero");
        assert_eq!(int_to_words(-3), "minus three");
        assert_eq!(int_to_words(42), "forty-two");
        assert_eq!(
            int_to_words(1984),
            "one thousand, nine hundred and eighty-four"
        );
        assert_eq!(int_to_words(2018), "two thousand and eighteen");
        assert_eq!(int_to_words_ordinal(42), "forty-second");
        assert_eq!(int_to_words_ordinal(20), "twentieth");
    }

    #[test]
    fn roman() {
        assert_eq!(to_roman(2018, true), "MMXVIII");
        assert_eq!(to_roman(1984, true), "MCMLXXXIV");
        assert_eq!(to_roman(3, false), "iii");
        assert_eq!(to_roman(0, true), "");
        assert_eq!(roman_value('m'), Some(1000));
        assert_eq!(roman_value('q'), None);
    }

    #[test]
    fn alphabetic() {
        assert_eq!(to_alphabetic(1, 'a'), "a");
        assert_eq!(to_alphabetic(26, 'a'), "z");
        assert_eq!(to_alphabetic(27, 'A'), "AA");
        assert_eq!(to_alphabetic(0, 'a'), "");
    }

    #[test]
    fn ordinal_suffixes() {
        assert_eq!(ordinal_suffix(1), "st");
        assert_eq!(ordinal_suffix(2), "nd");
        assert_eq!(ordinal_suffix(3), "rd");
        assert_eq!(ordinal_suffix(4), "th");
        assert_eq!(ordinal_suffix(11), "th");
        assert_eq!(ordinal_suffix(12), "th");
        assert_eq!(ordinal_suffix(13), "th");
        assert_eq!(ordinal_suffix(21), "st");
    }

    #[test]
    fn picture_modifier() {
        assert_eq!(split_picture_modifier("w;o"), ("w", "o"));
        assert_eq!(split_picture_modifier("999"), ("999", "c"));
    }

    #[test]
    fn digit_families() {
        assert_eq!(unicode_digit_zero('٣'), Some('\u{0660}')); // Arabic-Indic 3
        assert_eq!(unicode_digit_zero('7'), None); // ASCII is not in the table
        assert!(is_unicode_digit('٠'));
        assert!(!is_unicode_digit('x'));
    }
}
