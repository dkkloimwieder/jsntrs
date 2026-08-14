mod token;

pub use token::{Token, TokenType};

use crate::error::JsonataError;

/// Lexer tokenizes a JSONata expression string.
///
/// Direct port of Go `internal/lexer/lexer.go`.
pub struct Lexer {
    src: Vec<u8>,
    pos: usize,
}

impl Lexer {
    /// Create a lexer over a JSONata source string.
    pub fn new(src: &str) -> Self {
        Self {
            src: src.as_bytes().to_vec(),
            pos: 0,
        }
    }

    /// Returns the next token.
    ///
    /// `infix` = true means we are after a value (closing bracket, identifier, etc.).
    /// `infix` = false means we are in prefix position; a `/` starts a regex literal.
    ///
    /// # Errors
    /// Returns a `JsonataError` with an S0xxx code for malformed tokens.
    pub fn next(&mut self, infix: bool) -> Result<Token, JsonataError> {
        // Iterative, not recursive: consecutive comments (or runs of
        // unrecognised characters) must not grow the stack.
        loop {
            // Skip whitespace
            while self.pos < self.src.len() {
                match self.src[self.pos] {
                    b' ' | b'\t' | b'\n' | b'\r' | 0x0B => self.pos += 1,
                    _ => break,
                }
            }

            if self.pos >= self.src.len() {
                return Ok(Token::eof());
            }

            let start_pos = self.pos;
            let ch = self.src[self.pos];

            // Block comments: /* ... */
            if ch == b'/' && self.peek(1) == Some(b'*') {
                self.pos += 2;
                let mut closed = false;
                while self.pos < self.src.len() {
                    if self.src[self.pos] == b'*' && self.peek(1) == Some(b'/') {
                        self.pos += 2;
                        closed = true;
                        break;
                    }
                    self.pos += 1;
                }
                if !closed {
                    return Err(lex_error("S0106", "unclosed block comment"));
                }
                continue;
            }

            // Regex literal — only in prefix position
            if ch == b'/' && !infix {
                return self.scan_regex(start_pos);
            }

            // Two-character operators — checked before single-character
            if let Some(tok) = self.try_two_char(start_pos) {
                return Ok(tok);
            }

            // Single-character operators
            if let Some(tt) = single_char_op(ch) {
                self.pos += 1;
                return Ok(Token::new(tt, String::from(ch as char), start_pos));
            }

            // String literals
            if ch == b'"' || ch == b'\'' {
                return self.scan_string(ch, start_pos);
            }

            // Number literals
            if ch.is_ascii_digit() {
                return self.scan_number(start_pos);
            }

            // Backtick names
            if ch == b'`' {
                self.pos += 1;
                let name_start = self.pos;
                while self.pos < self.src.len() && self.src[self.pos] != b'`' {
                    self.pos += 1;
                }
                if self.pos >= self.src.len() {
                    return Err(lex_error("S0105", "unterminated backtick name"));
                }
                let name = std::str::from_utf8(&self.src[name_start..self.pos])
                    .unwrap_or("")
                    .to_owned();
                self.pos += 1;
                return Ok(Token::new(TokenType::Name, name, start_pos));
            }

            // Identifiers, keywords, and variables
            let id_start = self.pos;
            while self.pos < self.src.len() && !is_stop_char(self.src[self.pos]) {
                self.pos += 1;
            }
            let id = std::str::from_utf8(&self.src[id_start..self.pos])
                .unwrap_or("")
                .to_owned();

            if id.is_empty() {
                // Unrecognised character — skip and retry
                self.pos += char_len_at(&self.src, self.pos);
                continue;
            }

            if let Some(rest) = id.strip_prefix('$') {
                if rest == "$" {
                    // $$ → variable named "$"
                    return Ok(Token::new(TokenType::Variable, "$", start_pos));
                }
                // bare $ → variable named ""; $foo → variable named "foo"
                return Ok(Token::new(TokenType::Variable, rest, start_pos));
            }

            return match id.as_str() {
                "or" => Ok(Token::new(TokenType::Or, id, start_pos)),
                "in" => Ok(Token::new(TokenType::In, id, start_pos)),
                "and" => Ok(Token::new(TokenType::And, id, start_pos)),
                "true" => Ok(Token::bool_value(true, start_pos)),
                "false" => Ok(Token::bool_value(false, start_pos)),
                "null" => Ok(Token::null_value(start_pos)),
                _ => Ok(Token::new(TokenType::Name, id, start_pos)),
            };
        }
    }

    fn peek(&self, offset: usize) -> Option<u8> {
        self.src.get(self.pos + offset).copied()
    }

    fn try_two_char(&mut self, start_pos: usize) -> Option<Token> {
        if self.pos + 1 >= self.src.len() {
            return None;
        }
        let a = self.src[self.pos];
        let b = self.src[self.pos + 1];
        let tt = match (a, b) {
            (b'.', b'.') => TokenType::DotDot,
            (b':', b'=') => TokenType::Assign,
            (b'!', b'=') => TokenType::NE,
            (b'>', b'=') => TokenType::GE,
            (b'<', b'=') => TokenType::LE,
            (b'*', b'*') => TokenType::StarStar,
            (b'~', b'>') => TokenType::Chain,
            (b'?', b':') => TokenType::Elvis,
            (b'?', b'?') => TokenType::Coalesce,
            _ => return None,
        };
        let value = std::str::from_utf8(&self.src[self.pos..self.pos + 2])
            .unwrap_or("")
            .to_owned();
        self.pos += 2;
        Some(Token::new(tt, value, start_pos))
    }

    /// Scans a regex literal: `/pattern/` plus optional `i`/`m` flags.
    ///
    /// The closing `/` is the first one at bracket depth 0 that is not part of
    /// a `\`-escape. Escapes are consumed as a unit, so an escaped bracket
    /// (`\(`, `\[`, …) never moves the depth counter, while a bracket after an
    /// escaped backslash (`/\\(x)/`) still does.
    fn scan_regex(&mut self, start_pos: usize) -> Result<Token, JsonataError> {
        self.pos += 1; // consume opening '/'
        let pat_start = self.pos;
        let mut depth: i32 = 0;

        while self.pos < self.src.len() {
            match self.src[self.pos] {
                // A backslash escapes the next character: the pair is plain
                // pattern text and takes part in neither the bracket-depth
                // bookkeeping nor the search for the closing '/'. Skipping the
                // whole escaped character keeps the walk on char boundaries
                // when a multi-byte character follows the backslash.
                b'\\' => {
                    self.pos += 1;
                    self.pos += char_len_at(&self.src, self.pos);
                }
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    self.pos += 1;
                }
                b')' | b']' | b'}' => {
                    depth -= 1;
                    self.pos += 1;
                }
                b'/' if depth == 0 => {
                    // Unescaped closing '/' — escaped ones were consumed above.
                    let pattern = std::str::from_utf8(&self.src[pat_start..self.pos])
                        .unwrap_or("")
                        .to_owned();
                    if pattern.is_empty() {
                        return Err(lex_error("S0301", "empty regex pattern"));
                    }
                    self.pos += 1; // consume closing '/'

                    // Collect flags: only 'i' and 'm' are valid
                    let mut flags = String::new();
                    while self.pos < self.src.len() && (self.src[self.pos] as char).is_alphabetic()
                    {
                        match self.src[self.pos] {
                            b'i' | b'm' => {
                                flags.push(self.src[self.pos] as char);
                                self.pos += 1;
                            }
                            _ => {
                                return Err(lex_error("S0302", "invalid regex flag"));
                            }
                        }
                    }
                    flags.push('g');

                    return Ok(Token::regex(pattern, flags, start_pos));
                }
                _ => {
                    self.pos += 1;
                }
            }
        }
        Err(lex_error("S0302", "unterminated regex"))
    }

    fn scan_string(&mut self, quote: u8, start_pos: usize) -> Result<Token, JsonataError> {
        self.pos += 1; // consume opening quote
        let mut result = String::new();

        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            if c == quote {
                self.pos += 1;
                return Ok(Token::new(TokenType::String, result, start_pos));
            }
            if c != b'\\' {
                // Regular character — handle UTF-8
                let len = char_len_at(&self.src, self.pos);
                let s =
                    std::str::from_utf8(&self.src[self.pos..self.pos + len]).unwrap_or("\u{FFFD}");
                result.push_str(s);
                self.pos += len;
                continue;
            }
            // Escape sequence
            self.pos += 1;
            if self.pos >= self.src.len() {
                return Err(lex_error("S0101", "unterminated string literal"));
            }
            let esc = self.src[self.pos];
            self.pos += 1;

            match esc {
                b'"' => result.push('"'),
                b'\'' => result.push('\''),
                b'\\' => result.push('\\'),
                b'/' => result.push('/'),
                b'b' => result.push('\u{08}'),
                b'f' => result.push('\u{0C}'),
                b'n' => result.push('\n'),
                b'r' => result.push('\r'),
                b't' => result.push('\t'),
                b'u' => {
                    if self.pos + 4 > self.src.len() {
                        return Err(lex_error("S0104", "invalid unicode escape: too short"));
                    }
                    let hex = std::str::from_utf8(&self.src[self.pos..self.pos + 4])
                        .map_err(|_| lex_error("S0104", "invalid unicode escape"))?;
                    let r = u32::from_str_radix(hex, 16).map_err(|_| {
                        lex_error("S0104", &format!("invalid unicode escape: \\u{hex}"))
                    })?;
                    self.pos += 4;

                    // Handle UTF-16 surrogate pairs
                    if (0xD800..=0xDBFF).contains(&r)
                        && self.pos + 6 <= self.src.len()
                        && self.src[self.pos] == b'\\'
                        && self.src[self.pos + 1] == b'u'
                        && let Ok(hex2) = std::str::from_utf8(&self.src[self.pos + 2..self.pos + 6])
                        && let Ok(r2) = u32::from_str_radix(hex2, 16)
                        && (0xDC00..=0xDFFF).contains(&r2)
                    {
                        let cp = 0x10000 + (r - 0xD800) * 0x400 + (r2 - 0xDC00);
                        if let Some(ch) = char::from_u32(cp) {
                            result.push(ch);
                            self.pos += 6;
                            continue;
                        }
                    }
                    if let Some(ch) = char::from_u32(r) {
                        result.push(ch);
                    } else {
                        return Err(lex_error(
                            "D3140",
                            &format!("invalid Unicode codepoint: \\u{r:04X}"),
                        ));
                    }
                }
                _ => {
                    return Err(lex_error(
                        "S0103",
                        &format!("invalid escape sequence: \\{}", esc as char),
                    ));
                }
            }
        }
        Err(lex_error("S0101", "unterminated string literal"))
    }

    fn scan_number(&mut self, start_pos: usize) -> Result<Token, JsonataError> {
        let num_start = self.pos;

        // Integer part: 0 or [1-9][0-9]*
        if self.src[self.pos] == b'0' {
            self.pos += 1;
        } else {
            while self.pos < self.src.len() && self.src[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }

        // Fractional part
        if self.pos < self.src.len()
            && self.src[self.pos] == b'.'
            && self.pos + 1 < self.src.len()
            && self.src[self.pos + 1].is_ascii_digit()
        {
            self.pos += 1;
            while self.pos < self.src.len() && self.src[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }

        // Exponent part
        if self.pos < self.src.len() && matches!(self.src[self.pos], b'e' | b'E') {
            self.pos += 1;
            if self.pos < self.src.len() && matches!(self.src[self.pos], b'+' | b'-') {
                self.pos += 1;
            }
            while self.pos < self.src.len() && self.src[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }

        let num_str = std::str::from_utf8(&self.src[num_start..self.pos])
            .unwrap_or("0")
            .to_owned();
        let f: f64 = num_str
            .parse()
            .map_err(|_| lex_error("S0102", &format!("invalid number literal: {num_str}")))?;
        if f.is_infinite() || f.is_nan() {
            return Err(lex_error(
                "S0102",
                &format!("invalid number literal: {num_str}"),
            ));
        }
        Ok(Token::number(num_str, f, start_pos))
    }
}

fn is_stop_char(ch: u8) -> bool {
    matches!(
        ch,
        b' ' | b'\t'
            | b'\n'
            | b'\r'
            | 0x0B
            | b'.'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'('
            | b')'
            | b','
            | b'@'
            | b'#'
            | b';'
            | b':'
            | b'?'
            | b'+'
            | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'|'
            | b'='
            | b'<'
            | b'>'
            | b'^'
            | b'&'
            | b'!'
            | b'~'
    )
}

fn single_char_op(ch: u8) -> Option<TokenType> {
    match ch {
        b'.' => Some(TokenType::Dot),
        b'[' => Some(TokenType::LBracket),
        b']' => Some(TokenType::RBracket),
        b'{' => Some(TokenType::LBrace),
        b'}' => Some(TokenType::RBrace),
        b'(' => Some(TokenType::LParen),
        b')' => Some(TokenType::RParen),
        b',' => Some(TokenType::Comma),
        b'@' => Some(TokenType::At),
        b'#' => Some(TokenType::Hash),
        b';' => Some(TokenType::Semicolon),
        b':' => Some(TokenType::Colon),
        b'?' => Some(TokenType::Question),
        b'+' => Some(TokenType::Plus),
        b'-' => Some(TokenType::Minus),
        b'*' => Some(TokenType::Star),
        b'/' => Some(TokenType::Slash),
        b'%' => Some(TokenType::Percent),
        b'|' => Some(TokenType::Pipe),
        b'=' => Some(TokenType::Equals),
        b'<' => Some(TokenType::LT),
        b'>' => Some(TokenType::GT),
        b'^' => Some(TokenType::Caret),
        b'&' => Some(TokenType::Amp),
        b'!' => Some(TokenType::Bang),
        b'~' => Some(TokenType::Tilde),
        _ => None,
    }
}

/// Returns the byte length of the UTF-8 character at position `pos`.
fn char_len_at(src: &[u8], pos: usize) -> usize {
    if pos >= src.len() {
        return 0;
    }
    let b = src[pos];
    if b < 0x80 {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

fn lex_error(code: &'static str, msg: &str) -> JsonataError {
    JsonataError::new(code, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_all(src: &str) -> Vec<Token> {
        let mut lexer = Lexer::new(src);
        let mut tokens = Vec::new();
        let mut infix = false;
        loop {
            let tok = lexer.next(infix).unwrap();
            if tok.typ == TokenType::EOF {
                break;
            }
            // After values/closing brackets, next token is in infix position
            infix = matches!(
                tok.typ,
                TokenType::Name
                    | TokenType::Variable
                    | TokenType::String
                    | TokenType::Number
                    | TokenType::Value
                    | TokenType::RBracket
                    | TokenType::RBrace
                    | TokenType::RParen
            );
            tokens.push(tok);
        }
        tokens
    }

    #[test]
    fn simple_name() {
        let toks = lex_all("foo");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].typ, TokenType::Name);
        assert_eq!(toks[0].value, "foo");
    }

    #[test]
    fn dotted_path() {
        let toks = lex_all("a.b.c");
        assert_eq!(toks.len(), 5);
        assert_eq!(toks[0].typ, TokenType::Name);
        assert_eq!(toks[1].typ, TokenType::Dot);
        assert_eq!(toks[2].typ, TokenType::Name);
        assert_eq!(toks[3].typ, TokenType::Dot);
        assert_eq!(toks[4].typ, TokenType::Name);
    }

    #[test]
    fn number_literal() {
        let toks = lex_all("42");
        assert_eq!(toks[0].typ, TokenType::Number);
        assert_eq!(toks[0].num_val, 42.0);
    }

    #[test]
    fn number_with_decimal() {
        let toks = lex_all("3.25");
        assert_eq!(toks[0].typ, TokenType::Number);
        assert_eq!(toks[0].num_val, 3.25);
    }

    #[test]
    fn number_with_exponent() {
        let toks = lex_all("1e10");
        assert_eq!(toks[0].typ, TokenType::Number);
        assert_eq!(toks[0].num_val, 1e10);
    }

    #[test]
    fn string_double_quote() {
        let toks = lex_all(r#""hello""#);
        assert_eq!(toks[0].typ, TokenType::String);
        assert_eq!(toks[0].value, "hello");
    }

    #[test]
    fn string_single_quote() {
        let toks = lex_all("'world'");
        assert_eq!(toks[0].typ, TokenType::String);
        assert_eq!(toks[0].value, "world");
    }

    #[test]
    fn string_escape_sequences() {
        let toks = lex_all(r#""a\nb\tc""#);
        assert_eq!(toks[0].value, "a\nb\tc");
    }

    #[test]
    fn string_unicode_escape() {
        let toks = lex_all(r#""\u0041""#);
        assert_eq!(toks[0].value, "A");
    }

    #[test]
    fn string_surrogate_pair() {
        // U+1F600 (grinning face) = \uD83D\uDE00
        let toks = lex_all(r#""\uD83D\uDE00""#);
        assert_eq!(toks[0].value, "\u{1F600}");
    }

    #[test]
    fn variable_bare() {
        let toks = lex_all("$");
        assert_eq!(toks[0].typ, TokenType::Variable);
        assert_eq!(toks[0].value, "");
    }

    #[test]
    fn variable_named() {
        let toks = lex_all("$foo");
        assert_eq!(toks[0].typ, TokenType::Variable);
        assert_eq!(toks[0].value, "foo");
    }

    #[test]
    fn variable_double_dollar() {
        let toks = lex_all("$$");
        assert_eq!(toks[0].typ, TokenType::Variable);
        assert_eq!(toks[0].value, "$");
    }

    #[test]
    fn keywords() {
        let toks = lex_all("true false null");
        assert_eq!(toks[0].typ, TokenType::Value);
        assert!(toks[0].bool_val);
        assert_eq!(toks[1].typ, TokenType::Value);
        assert!(!toks[1].bool_val);
        assert_eq!(toks[2].typ, TokenType::Value);
        assert!(toks[2].is_null);
    }

    #[test]
    fn operators_two_char() {
        let toks = lex_all(".. := != >= <= ** ~> ?: ??");
        let types: Vec<_> = toks.iter().map(|t| t.typ).collect();
        assert_eq!(
            types,
            vec![
                TokenType::DotDot,
                TokenType::Assign,
                TokenType::NE,
                TokenType::GE,
                TokenType::LE,
                TokenType::StarStar,
                TokenType::Chain,
                TokenType::Elvis,
                TokenType::Coalesce,
            ]
        );
    }

    #[test]
    fn and_or_in() {
        let toks = lex_all("a and b or c in d");
        assert_eq!(toks[1].typ, TokenType::And);
        assert_eq!(toks[3].typ, TokenType::Or);
        assert_eq!(toks[5].typ, TokenType::In);
    }

    #[test]
    fn regex_simple() {
        // In prefix position, / starts a regex
        let mut lexer = Lexer::new("/abc/i");
        let tok = lexer.next(false).unwrap();
        assert_eq!(tok.typ, TokenType::Regex);
        assert_eq!(tok.regex_pat, "abc");
        assert_eq!(tok.regex_flg, "ig");
    }

    fn lex_regex(src: &str) -> Result<Token, JsonataError> {
        Lexer::new(src).next(false)
    }

    #[test]
    fn regex_escaped_brackets_do_not_count_toward_depth() {
        // Each of these is unbalanced only because the bracket is escaped.
        for (src, pat) in [
            (r"/\(/", r"\("),
            (r"/\)/", r"\)"),
            (r"/\[/", r"\["),
            (r"/\]/", r"\]"),
            (r"/\{/", r"\{"),
            (r"/\}/", r"\}"),
            (r"/a\(b/", r"a\(b"),
        ] {
            let tok = lex_regex(src).unwrap_or_else(|e| panic!("{src} failed to lex: {e:?}"));
            assert_eq!(tok.typ, TokenType::Regex, "{src}");
            assert_eq!(tok.regex_pat, pat, "{src}");
            assert_eq!(tok.regex_flg, "g", "{src}");
        }
    }

    #[test]
    fn regex_escaped_dash_in_character_class() {
        let tok = lex_regex(r"/[\-]/").unwrap();
        assert_eq!(tok.regex_pat, r"[\-]");
    }

    #[test]
    fn regex_escaped_backslash_does_not_escape_following_bracket() {
        // `\\` is a literal backslash; the `(` after it opens a real group,
        // so the `)` is needed to get back to depth 0.
        let tok = lex_regex(r"/\\(x)/").unwrap();
        assert_eq!(tok.regex_pat, r"\\(x)");
        let tok = lex_regex(r"/\\{2}/").unwrap();
        assert_eq!(tok.regex_pat, r"\\{2}");
        assert_eq!(lex_regex(r"/\\(x/").unwrap_err().code, "S0302");
        // Same rule seen from the other side: the closer is live too, so a
        // lone one is unbalanced. (jsonata-js accepts `/\\}/` because its
        // depth guard only looks at the single character before the bracket;
        // see the deviation note in docs/spec.md 2.5.)
        assert_eq!(lex_regex(r"/\\}/").unwrap_err().code, "S0302");
    }

    #[test]
    fn regex_escaped_slash_is_not_a_terminator() {
        let tok = lex_regex(r"/a\/b/").unwrap();
        assert_eq!(tok.regex_pat, r"a\/b");
        // An escaped backslash before the '/' leaves the '/' unescaped.
        let tok = lex_regex(r"/a\\/").unwrap();
        assert_eq!(tok.regex_pat, r"a\\");
    }

    #[test]
    fn regex_unescaped_bracket_still_needs_balancing() {
        assert_eq!(lex_regex("/a(b/").unwrap_err().code, "S0302");
        assert_eq!(lex_regex("/]/").unwrap_err().code, "S0302");
        assert_eq!(lex_regex("/abc").unwrap_err().code, "S0302");
        // A '/' inside a character class does not terminate the literal.
        let tok = lex_regex("/[/]/").unwrap();
        assert_eq!(tok.regex_pat, "[/]");
    }

    #[test]
    fn regex_escaped_multibyte_char_keeps_scan_aligned() {
        let tok = lex_regex("/\\é(x)/i").unwrap();
        assert_eq!(tok.regex_pat, "\\é(x)");
        assert_eq!(tok.regex_flg, "ig");
        // Trailing backslash: no character to escape, so the literal is unterminated.
        assert_eq!(lex_regex("/ab\\").unwrap_err().code, "S0302");
    }

    #[test]
    fn slash_as_division() {
        // After a number (infix position), / is division
        let toks = lex_all("5/2");
        assert_eq!(toks[0].typ, TokenType::Number);
        assert_eq!(toks[1].typ, TokenType::Slash);
        assert_eq!(toks[2].typ, TokenType::Number);
    }

    #[test]
    fn block_comment() {
        let toks = lex_all("a /* comment */ b");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[0].value, "a");
        assert_eq!(toks[1].value, "b");
    }

    #[test]
    fn many_consecutive_comments_lex_iteratively() {
        // 100k comments before a token — the recursive lexer overflowed here.
        let src = format!("{}42", "/**/".repeat(100_000));
        let toks = lex_all(&src);
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].typ, TokenType::Number);
        assert_eq!(toks[0].value, "42");
    }

    #[test]
    fn backtick_name() {
        let toks = lex_all("`hello world`");
        assert_eq!(toks[0].typ, TokenType::Name);
        assert_eq!(toks[0].value, "hello world");
    }

    #[test]
    fn complex_expression() {
        let toks = lex_all("Account.Order.Product.Price");
        assert_eq!(toks.len(), 7);
        for (i, tok) in toks.iter().enumerate() {
            if i % 2 == 0 {
                assert_eq!(tok.typ, TokenType::Name);
            } else {
                assert_eq!(tok.typ, TokenType::Dot);
            }
        }
    }

    #[test]
    fn function_call() {
        let toks = lex_all("$sum(Account.Order.Price)");
        assert_eq!(toks[0].typ, TokenType::Variable);
        assert_eq!(toks[0].value, "sum");
        assert_eq!(toks[1].typ, TokenType::LParen);
    }

    #[test]
    fn array_subscript() {
        let toks = lex_all("items[0]");
        assert_eq!(toks[0].typ, TokenType::Name);
        assert_eq!(toks[1].typ, TokenType::LBracket);
        assert_eq!(toks[2].typ, TokenType::Number);
        assert_eq!(toks[3].typ, TokenType::RBracket);
    }

    #[test]
    fn error_unterminated_string() {
        let mut lexer = Lexer::new("\"hello");
        let result = lexer.next(false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "S0101");
    }

    #[test]
    fn error_unterminated_backtick() {
        let mut lexer = Lexer::new("`hello");
        let result = lexer.next(false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "S0105");
    }

    #[test]
    fn error_unclosed_comment() {
        let mut lexer = Lexer::new("/* comment");
        let result = lexer.next(false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "S0106");
    }

    #[test]
    fn whitespace_handling() {
        let toks = lex_all("  a  +  b  ");
        assert_eq!(toks.len(), 3);
        assert_eq!(toks[0].value, "a");
        assert_eq!(toks[1].typ, TokenType::Plus);
        assert_eq!(toks[2].value, "b");
    }

    #[test]
    fn number_zero_dot_field() {
        // "0.foo" should lex as number 0, dot, name foo
        let toks = lex_all("0.foo");
        assert_eq!(toks[0].typ, TokenType::Number);
        assert_eq!(toks[0].num_val, 0.0);
        assert_eq!(toks[1].typ, TokenType::Dot);
        assert_eq!(toks[2].typ, TokenType::Name);
    }
}
