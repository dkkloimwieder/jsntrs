// Reachable only through the `internals`-gated #[doc(hidden)] re-exports
// for in-repo tooling; not part of the documented public API. missing_docs
// only applies when the items are publicly reachable, hence the cfg_attr.
#![cfg_attr(feature = "internals", expect(missing_docs))]

/// Token type identifying the category of a lexed token.
///
/// Binding power comments indicate Pratt parser precedence (used in Phase 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenType {
    // Terminal tokens
    EOF,
    Name,     // bare identifier (not a keyword)
    Variable, // $name — value is text after $; "" for bare $
    String,   // "…" or '…'
    Number,   // numeric literal
    Value,    // true | false | null
    Regex,    // /pattern/flags

    // Single-character operators
    Dot,       // .  bp=75
    LBracket,  // [  bp=80
    RBracket,  // ]  bp=0
    LBrace,    // {  bp=70
    RBrace,    // }  bp=0
    LParen,    // (  bp=80
    RParen,    // )  bp=0
    Comma,     // ,  bp=0
    At,        // @  bp=80
    Hash,      // #  bp=80
    Semicolon, // ;  bp=80
    Colon,     // :  bp=80
    Question,  // ?  bp=20
    Plus,      // +  bp=50
    Minus,     // -  bp=50
    Star,      // *  bp=60
    Slash,     // /  bp=60
    Percent,   // %  bp=60
    Pipe,      // |  bp=20
    Equals,    // =  bp=40
    LT,        // <  bp=40
    GT,        // >  bp=40
    Caret,     // ^  bp=40
    Amp,       // &  bp=50
    Bang,      // !  bp=0
    Tilde,     // ~  bp=0

    // Multi-character operators
    StarStar, // **  bp=60
    DotDot,   // ..  bp=20
    Assign,   // :=  bp=10
    NE,       // !=  bp=40
    LE,       // <=  bp=40
    GE,       // >=  bp=40
    Chain,    // ~>  bp=40
    Elvis,    // ?:  bp=40
    Coalesce, // ??  bp=40

    // Keywords
    And, // and  bp=30
    Or,  // or   bp=25
    In,  // in   bp=40
}

/// A single lexed token.
#[derive(Debug, Clone)]
pub struct Token {
    pub typ: TokenType,
    pub value: String,
    pub num_val: f64,
    // The parser re-derives literal values from `typ`/`value`; these two
    // fields exist for token-stream consumers and are only read by tests
    // and `internals` tooling.
    #[cfg_attr(
        not(feature = "internals"),
        expect(dead_code, reason = "read only by tests and internals consumers")
    )]
    pub bool_val: bool,
    #[cfg_attr(
        not(feature = "internals"),
        expect(dead_code, reason = "read only by tests and internals consumers")
    )]
    pub is_null: bool,
    pub regex_pat: String,
    pub regex_flg: String,
    pub pos: usize,
}

impl Default for Token {
    fn default() -> Self {
        Self::eof()
    }
}

impl Token {
    pub fn eof() -> Self {
        Self {
            typ: TokenType::EOF,
            value: String::new(),
            num_val: 0.0,
            bool_val: false,
            is_null: false,
            regex_pat: String::new(),
            regex_flg: String::new(),
            pos: 0,
        }
    }

    pub fn new(typ: TokenType, value: impl Into<String>, pos: usize) -> Self {
        Self {
            typ,
            value: value.into(),
            num_val: 0.0,
            bool_val: false,
            is_null: false,
            regex_pat: String::new(),
            regex_flg: String::new(),
            pos,
        }
    }

    pub fn number(value: impl Into<String>, num_val: f64, pos: usize) -> Self {
        Self {
            typ: TokenType::Number,
            value: value.into(),
            num_val,
            bool_val: false,
            is_null: false,
            regex_pat: String::new(),
            regex_flg: String::new(),
            pos,
        }
    }

    pub fn regex(pattern: impl Into<String>, flags: impl Into<String>, pos: usize) -> Self {
        Self {
            typ: TokenType::Regex,
            value: String::new(),
            num_val: 0.0,
            bool_val: false,
            is_null: false,
            regex_pat: pattern.into(),
            regex_flg: flags.into(),
            pos,
        }
    }

    pub fn bool_value(val: bool, pos: usize) -> Self {
        Self {
            typ: TokenType::Value,
            value: if val { "true" } else { "false" }.into(),
            num_val: 0.0,
            bool_val: val,
            is_null: false,
            regex_pat: String::new(),
            regex_flg: String::new(),
            pos,
        }
    }

    pub fn null_value(pos: usize) -> Self {
        Self {
            typ: TokenType::Value,
            value: "null".into(),
            num_val: 0.0,
            bool_val: false,
            is_null: true,
            regex_pat: String::new(),
            regex_flg: String::new(),
            pos,
        }
    }
}
