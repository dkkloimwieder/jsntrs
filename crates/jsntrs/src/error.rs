//! `JsonataError`: the crate's single error type, its spec code, and the
//! `Display` rendering used everywhere a message is shown to a caller.

use std::fmt;

/// Structured error type matching JSONata spec error codes.
///
/// Code prefixes:
/// - S0xxx: Syntax errors (lexer/parser)
/// - T0xxx: Type errors (function arguments)
/// - T1xxx: Type errors (function-specific)
/// - T2xxx: Type errors (operators)
/// - D0000: not a JSONata error — malformed JSON *input*, and internal
///   invariant violations that should be unreachable. Never produced by
///   evaluating a well-formed input (`docs/behaviors.md` §2.4).
/// - D1xxx: Domain errors (numeric)
/// - D2xxx: Domain errors (general)
/// - D3xxx: Domain errors (function-specific)
/// - U1001: Stack overflow
///
/// The language documentation publishes no error-code page, so which of
/// these are *specified* and which are inherited from an implementation is
/// itself a question — `docs/behaviors.md` §2.0 answers it code by code.
#[derive(Debug, Clone)]
pub struct JsonataError {
    /// JSONata spec error code, e.g. `"S0201"` or `"T2010"`.
    pub code: &'static str,
    /// Source token the error is attached to, if any (parse errors).
    pub token: String,
    /// Offending value rendered as text, if the error carries one.
    pub value: Option<String>,
    /// Human-readable description of the error.
    pub message: String,
    /// Byte offset into the source expression, for parse errors.
    pub position: Option<usize>,
}

impl JsonataError {
    /// Create an error with a spec code and message.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            token: String::new(),
            value: None,
            message: message.into(),
            position: None,
        }
    }

    /// Create an error carrying only a spec code, with no message.
    pub fn with_code(code: &'static str) -> Self {
        Self {
            code,
            token: String::new(),
            value: None,
            message: String::new(),
            position: None,
        }
    }

    /// Attach the source token the error refers to.
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = token.into();
        self
    }

    /// Attach the source token *only if none is set yet*.
    ///
    /// The reference implementation attributes an error to the enclosing
    /// call site with `if (!err.token) { err.token = procName; }` (jsonata
    /// 2.2.2 `jsonata.js:4948`), so the innermost site that already named a
    /// token keeps it: `$map([1], function($x){ $x + 'a' })` stays attached
    /// to `+`, while `$map([1], function($x){ 1 ~> 2 })` — whose `T2006`
    /// names nothing — comes out attached to `map`. Contrast
    /// [`Self::with_token`], which overwrites the way `evaluateBinary`'s
    /// `err.token = op` does.
    #[must_use]
    pub(crate) fn or_token(mut self, token: &str) -> Self {
        if self.token.is_empty() {
            self.token.push_str(token);
        }
        self
    }

    /// Attach the offending value, rendered as text.
    #[must_use]
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// Attach the source byte offset the error refers to.
    #[must_use]
    pub fn with_position(mut self, position: usize) -> Self {
        self.position = Some(position);
        self
    }
}

impl fmt::Display for JsonataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (!self.message.is_empty(), !self.code.is_empty()) {
            (true, true) => write!(f, "{}: {}", self.code, self.message),
            (true, false) => write!(f, "{}", self.message),
            (false, true) => write!(f, "{}", self.code),
            (false, false) => write!(f, "unknown error"),
        }
    }
}

impl std::error::Error for JsonataError {}

/// Result type alias used throughout evaluation.
pub type JsonataResult<T = super::Value> = Result<T, JsonataError>;
