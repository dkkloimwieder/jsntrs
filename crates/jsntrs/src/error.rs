use std::fmt;

/// Structured error type matching JSONata spec error codes.
///
/// Code prefixes:
/// - S0xxx: Syntax errors (lexer/parser)
/// - T0xxx: Type errors (function arguments)
/// - T1xxx: Type errors (function-specific)
/// - T2xxx: Type errors (operators)
/// - D1xxx: Domain errors (numeric)
/// - D2xxx: Domain errors (general)
/// - D3xxx: Domain errors (function-specific)
/// - U1001: Stack overflow
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
