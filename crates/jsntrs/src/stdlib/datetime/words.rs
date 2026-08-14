//! Lenient prefix parser for spelled-out numbers in datetime pictures.
//!
//! Unlike `$parseInteger`'s strict whole-string parser, this consumes the
//! longest word-number prefix (ordinal-aware, incl. 'nineteen hundred'
//! style years) and reports how many bytes it used.

use crate::stdlib::number_words::{ONES, TENS};

pub(super) fn parse_word_number_from_string(s: &str) -> (usize, i64) {
    let s_lower = s.to_lowercase();
    parse_complex_number(&s_lower, &ONES, &TENS)
}

pub(super) struct WordParser<'a> {
    s: &'a str,
    pos: usize,
    ones: &'a [&'a str; 20],
    tens: &'a [&'a str; 10],
}

impl WordParser<'_> {
    fn skip_sep(&mut self) {
        while self.pos < self.s.len() {
            let c = self.s[self.pos..].chars().next();
            if c == Some(' ') || c == Some(',') {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.s[self.pos..].starts_with("and ") {
            self.pos += 4;
        }
    }

    fn try_word_or_ordinal(&mut self, word: &str, ordinal: &str) -> bool {
        let save = self.pos;
        self.skip_sep();
        for candidate in &[word, ordinal] {
            if candidate.is_empty() {
                continue;
            }
            if !self.s[self.pos..].starts_with(candidate) {
                continue;
            }
            let after = &self.s[self.pos + candidate.len()..];
            if after.is_empty() || !after.chars().next().is_some_and(char::is_alphabetic) {
                self.pos += candidate.len();
                return true;
            }
        }
        self.pos = save;
        false
    }

    fn try_word(&mut self, word: &str) -> bool {
        self.try_word_or_ordinal(word, "")
    }

    fn parse_sub100(&mut self) -> Option<i64> {
        let save = self.pos;
        // Teens/ones (19 down to 10).
        let ones_ordinals = [
            "zeroth",
            "first",
            "second",
            "third",
            "fourth",
            "fifth",
            "sixth",
            "seventh",
            "eighth",
            "ninth",
            "tenth",
            "eleventh",
            "twelfth",
            "thirteenth",
            "fourteenth",
            "fifteenth",
            "sixteenth",
            "seventeenth",
            "eighteenth",
            "nineteenth",
        ];
        for i in (10..=19usize).rev() {
            let ord = if i < ones_ordinals.len() {
                ones_ordinals[i]
            } else {
                ""
            };
            if self.try_word_or_ordinal(self.ones[i], ord) {
                return Some(i as i64);
            }
        }
        // Tens (ninety down to twenty).
        let tens_ordinals = [
            "",
            "",
            "twentieth",
            "thirtieth",
            "fortieth",
            "fiftieth",
            "sixtieth",
            "seventieth",
            "eightieth",
            "ninetieth",
        ];
        for i in (2..=9usize).rev() {
            let ord = if i < tens_ordinals.len() {
                tens_ordinals[i]
            } else {
                ""
            };
            if self.try_word_or_ordinal(self.tens[i], ord) {
                let v = i as i64 * 10;
                let dash_save = self.pos;
                if self.pos < self.s.len() && self.s[self.pos..].starts_with('-') {
                    self.pos += 1;
                }
                let ones_ordinals2 = [
                    "", "first", "second", "third", "fourth", "fifth", "sixth", "seventh",
                    "eighth", "ninth",
                ];
                for j in (1..=9usize).rev() {
                    let o2 = if j < ones_ordinals2.len() {
                        ones_ordinals2[j]
                    } else {
                        ""
                    };
                    if self.try_word_or_ordinal(self.ones[j], o2) {
                        return Some(v + j as i64);
                    }
                }
                self.pos = dash_save;
                return Some(v);
            }
        }
        // Ones (nine down to one).
        for i in (1..=9usize).rev() {
            let ord = if i < ones_ordinals.len() {
                ones_ordinals[i]
            } else {
                ""
            };
            if self.try_word_or_ordinal(self.ones[i], ord) {
                return Some(i as i64);
            }
        }
        self.pos = save;
        None
    }

    fn parse_sub1000(&mut self) -> Option<i64> {
        let save = self.pos;
        for i in (1..=9usize).rev() {
            if self.try_word(self.ones[i]) {
                if self.try_word_or_ordinal("hundred", "hundredth") {
                    let v = i as i64 * 100;
                    let rem = self.parse_sub100().unwrap_or(0);
                    return Some(v + rem);
                }
                self.pos = save;
                break;
            }
        }
        self.parse_sub100()
    }
}

pub(super) fn parse_complex_number(s: &str, ones: &[&str; 20], tens: &[&str; 10]) -> (usize, i64) {
    let mut p = WordParser {
        s,
        pos: 0,
        ones,
        tens,
    };
    let save = p.pos;

    // Try "nineteen hundred" style.
    for i in (1..=19usize).rev() {
        if p.try_word(p.ones[i]) {
            if p.try_word_or_ordinal("hundred", "hundredth") {
                let total = i as i64 * 100;
                let rem = p.parse_sub100().unwrap_or(0);
                return (p.pos, total + rem);
            }
            p.pos = save;
            break;
        }
    }

    // Try "X thousand, Y hundred and Z".
    if let Some(thousand_part) = p.parse_sub1000() {
        if p.try_word_or_ordinal("thousand", "thousandth") {
            let mut total = thousand_part * 1000;
            if let Some(rest) = p.parse_sub1000() {
                total += rest;
            }
            return (p.pos, total);
        }
        return (p.pos, thousand_part);
    }

    (0, 0)
}
