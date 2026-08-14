mod format;
mod sequence;

pub use format::format_float;
pub use sequence::Sequence;

use std::rc::Rc;

use compact_str::CompactString;
use serde_json::Number;

use crate::error::{JsonataError, JsonataResult};

/// Object map used in Value::Object. Uses IndexMap to preserve insertion
/// order (JSONata behavioral invariant #8). CompactString keys inline ≤24
/// bytes — covers all common JSON field names with zero heap allocation.
/// foldhash instead of SipHash: key lookup dominates path evaluation on
/// long field names (~16ns → ~11ns per get), and insertion order — the
/// only order JSONata semantics depend on — is hasher-independent.
pub type ObjectMap = indexmap::IndexMap<CompactString, Value, foldhash::fast::RandomState>;

/// Ordering comparison operators accepted by [`Value::compare`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareOp {
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

impl CompareOp {
    /// Operator source text, as used in JSONata error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            CompareOp::Lt => "<",
            CompareOp::Le => "<=",
            CompareOp::Gt => ">",
            CompareOp::Ge => ">=",
        }
    }
}

impl std::fmt::Display for CompareOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Core value type for JSONata evaluation.
///
/// Heap-allocated variants (String, Array, Object) are wrapped in `Rc` for
/// O(1) clone via reference counting. This eliminates the deep-copy overhead
/// that dominated the profile (62% of CPU was malloc/free/clone/drop).
///
/// `Value` is deliberately **`!Send`**: `Rc` (not `Arc`) keeps clone and
/// drop free of atomic operations, and keeps the enum at 32 bytes —
/// both measured as load-bearing for evaluation throughput. Share a compiled
/// [`Expression`](crate::Expression) across threads (it is `Send + Sync`)
/// and let each thread parse or build its own input `Value`.
///
/// Mutation requires `Rc::make_mut()` for copy-on-write semantics.
/// `Undefined` and `Null` are distinct enum variants preserving JSONata semantics.
///
/// The enum is `#[non_exhaustive]`: the hidden variants are internal to the
/// evaluator and never escape the public API, so user matches need a
/// wildcard arm only for forward compatibility.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Value {
    /// JSONata undefined — missing value, no representation in JSON.
    Undefined,
    /// JSON null — explicit null value.
    Null,
    /// Boolean.
    Bool(bool),
    /// Number. JSONata numbers are IEEE-754 doubles.
    Number(f64),
    /// String, stored inline when 24 bytes or shorter.
    String(CompactString),
    /// Array, shared by reference count.
    Array(Rc<[Value]>),
    /// Object with insertion-ordered keys, shared by reference count.
    Object(Rc<ObjectMap>),
    /// Internal sequence used during evaluation. Collapsed at the public
    /// API boundary — user code never observes this variant.
    /// Boxed to keep Value from growing beyond 32 bytes.
    #[doc(hidden)]
    #[non_exhaustive]
    Sequence(Box<Sequence>),
    /// Function value (built-in, lambda, partial application).
    /// Boxed to keep Value from growing beyond 32 bytes.
    #[doc(hidden)]
    #[non_exhaustive]
    Function(Box<crate::evaluator::FunctionValue>),
    /// Tail-call sentinel for TCO trampoline. Internal only.
    #[doc(hidden)]
    #[non_exhaustive]
    TailCall(Box<crate::evaluator::TailCall>),
}

impl Value {
    /// Returns true if this is `Value::Undefined` (a missing value).
    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined)
    }

    /// Returns true if this is `Value::Null` (an explicit JSON null).
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Returns true if this is a number (including NaN and infinities).
    pub fn is_number(&self) -> bool {
        matches!(self, Value::Number(_))
    }

    /// Returns true if this is a string.
    pub fn is_string(&self) -> bool {
        matches!(self, Value::String(_))
    }

    /// Returns true if this is a boolean.
    pub fn is_bool(&self) -> bool {
        matches!(self, Value::Bool(_))
    }

    /// Returns true if this is an array.
    pub fn is_array(&self) -> bool {
        matches!(self, Value::Array(_))
    }

    /// Returns true if this is an object.
    pub fn is_object(&self) -> bool {
        matches!(self, Value::Object(_))
    }

    /// Returns true if this is an internal evaluation sequence.
    ///
    /// Always false for values obtained from the public API, which
    /// collapses sequences before returning.
    pub fn is_sequence(&self) -> bool {
        matches!(self, Value::Sequence(_))
    }

    /// Returns true if this is a function value (built-in or lambda).
    pub fn is_function(&self) -> bool {
        matches!(self, Value::Function(_))
    }

    /// Returns true if the value is a finite number (not NaN or Inf).
    pub fn is_numeric(&self) -> bool {
        match self {
            Value::Number(n) => n.is_finite(),
            _ => false,
        }
    }

    /// Extract as f64 if this is a Number variant.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Borrow as `&str` if this is a String variant.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Extract as bool if this is a Bool variant.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Borrow as a slice if this is an Array variant.
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Borrow the ordered key/value map if this is an Object variant.
    pub fn as_object(&self) -> Option<&ObjectMap> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Coerce a value to an array. Arrays pass through (Rc clone), scalars
    /// are wrapped in a single-element array. Used by HOF functions that
    /// accept both arrays and scalars as their first argument.
    pub fn coerce_to_array(&self) -> Rc<[Value]> {
        match self {
            Value::Array(a) => Rc::clone(a),
            other => Rc::from(vec![other.clone()]),
        }
    }

    /// Extract a `FunctionValue` or return a typed error.
    /// `func_name` is used in the error message (e.g. "$map").
    ///
    /// # Errors
    /// Returns `T0410` if the value is not a function.
    pub fn require_function(
        &self,
        func_name: &str,
    ) -> JsonataResult<Box<crate::evaluator::FunctionValue>> {
        match self {
            Value::Function(f) => Ok(f.clone()),
            _ => Err(JsonataError::new(
                "T0410",
                format!("{func_name}: argument is not a function"),
            )),
        }
    }

    /// Validates that this is a finite number.
    ///
    /// # Errors
    /// Returns `D1001` if the value is `Inf` or `NaN`.
    pub fn check_numeric(&self) -> Result<(), JsonataError> {
        if let Value::Number(n) = self
            && (n.is_infinite() || n.is_nan())
        {
            return Err(JsonataError::with_code("D1001").with_value(format_float(*n)));
        }
        Ok(())
    }

    // ── Boolean coercion ─────────────────────────────────────────────

    /// Implements JSONata boolean casting rules.
    ///
    /// - Undefined/Null → false
    /// - Bool → value
    /// - String → non-empty
    /// - Number → non-zero
    /// - Object → non-empty
    /// - Array: len 0 → false, len 1 → recurse, len > 1 → any truthy
    /// - Sequence → collapse then recurse
    pub fn to_boolean(&self) -> bool {
        match self {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::String(s) => !s.is_empty(),
            Value::Number(n) => *n != 0.0,
            Value::Object(m) => !m.is_empty(),
            Value::Array(arr) => match arr.len() {
                0 => false,
                1 => arr[0].to_boolean(),
                _ => arr.iter().any(Value::to_boolean),
            },
            Value::Sequence(seq) => seq.collapse().to_boolean(),
            Value::Function(_) | Value::TailCall(_) => false,
        }
    }

    // ── Equality ─────────────────────────────────────────────────────

    /// Implements JSONata structural equality.
    ///
    /// Critical invariant: `undefined = undefined` returns `false`.
    /// `null = null` returns `true`.
    pub fn deep_equal(&self, other: &Value) -> bool {
        // Undefined on either side → false (even undefined = undefined)
        if self.is_undefined() || other.is_undefined() {
            return false;
        }
        // Null matches only null
        if self.is_null() || other.is_null() {
            return self.is_null() && other.is_null();
        }
        match (self, other) {
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.deep_equal(y))
            }
            (Value::Object(a), Value::Object(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(k, va)| b.get(k).is_some_and(|vb| va.deep_equal(vb)))
            }
            _ => false,
        }
    }

    // ── Comparison ───────────────────────────────────────────────────

    /// Compares two values for ordering. Returns -1, 0, or 1.
    /// Undefined sorts after non-undefined.
    ///
    /// # Errors
    /// Returns a `JsonataError` if the values are of incompatible types.
    pub fn compare_order(&self, other: &Value) -> JsonataResult<i8> {
        match (self, other) {
            (Value::Undefined, Value::Undefined) => Ok(0),
            (Value::Undefined, _) => Ok(1),
            (_, Value::Undefined) => Ok(-1),
            (Value::Number(a), Value::Number(b)) => Ok(a.partial_cmp(b).map_or(0, |o| o as i8)),
            (Value::String(a), Value::String(b)) => Ok(a.cmp(b) as i8),
            (Value::Number(_), Value::String(_)) | (Value::String(_), Value::Number(_)) => Err(
                JsonataError::new("T2007", "cannot compare string and number values"),
            ),
            _ => Err(JsonataError::new(
                "T2008",
                "cannot compare values of incompatible types".to_string(),
            )),
        }
    }

    /// Relational comparison (<, <=, >, >=).
    /// Returns Undefined if either operand is undefined.
    ///
    /// # Errors
    /// Returns a `JsonataError` if the operands are not numbers or strings.
    pub fn compare(&self, other: &Value, op: CompareOp) -> JsonataResult {
        // Validate left operand type
        if !self.is_undefined() && !self.is_number() && !self.is_string() {
            return Err(JsonataError::new(
                "T2010",
                format!("the operands of the \"{op}\" operator must be numbers or strings"),
            ));
        }
        // Undefined propagation
        if self.is_undefined() || other.is_undefined() {
            return Ok(Value::Undefined);
        }
        let ord = match (self, other) {
            (Value::Number(a), Value::Number(b)) => a.partial_cmp(b),
            (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
            (Value::Number(_), Value::String(_)) | (Value::String(_), Value::Number(_)) => {
                return Err(JsonataError::new(
                    "T2009",
                    format!(
                        "the operands of the \"{op}\" operator must be both numbers or both strings"
                    ),
                ));
            }
            _ => {
                return Err(JsonataError::new(
                    "T2010",
                    format!("the operands of the \"{op}\" operator must be numbers or strings"),
                ));
            }
        };
        // None only for NaN operands: every NaN comparison is false (IEEE).
        let result = ord.is_some_and(|o| match op {
            CompareOp::Lt => o.is_lt(),
            CompareOp::Le => o.is_le(),
            CompareOp::Gt => o.is_gt(),
            CompareOp::Ge => o.is_ge(),
        });
        Ok(Value::Bool(result))
    }

    // ── Stringify ────────────────────────────────────────────────────

    /// Check if a value (recursively) contains any non-finite numbers (Inf/NaN).
    pub fn contains_non_finite(&self) -> bool {
        match self {
            Value::Number(n) => n.is_infinite() || n.is_nan(),
            Value::Array(arr) => arr.iter().any(Value::contains_non_finite),
            Value::Object(obj) => obj.values().any(Value::contains_non_finite),
            Value::Sequence(seq) => seq.values.iter().any(Value::contains_non_finite),
            _ => false,
        }
    }

    /// Append the stringified form of this value to `buf`.
    ///
    /// Strings append verbatim (unquoted), booleans as `true`/`false`,
    /// numbers via `format_float`; undefined and functions append nothing;
    /// objects and arrays append their compact JSON.
    ///
    /// # Errors
    /// Returns `D1001` if the value contains non-finite numbers.
    pub fn stringify_into(&self, buf: &mut String) -> JsonataResult<()> {
        match self {
            Value::Undefined | Value::Function(_) | Value::TailCall(_) => Ok(()),
            Value::String(s) => {
                buf.push_str(s);
                Ok(())
            }
            Value::Number(n) => {
                buf.push_str(&format_float(*n));
                Ok(())
            }
            Value::Bool(true) => {
                buf.push_str("true");
                Ok(())
            }
            Value::Bool(false) => {
                buf.push_str("false");
                Ok(())
            }
            other => {
                if other.contains_non_finite() {
                    return Err(JsonataError::new("D1001", "Number out of range"));
                }
                buf.push_str(&other.to_json_string());
                Ok(())
            }
        }
    }

    /// # Errors
    /// Returns `D1001` if the value contains non-finite numbers.
    pub fn stringify(&self, prettify: bool) -> JsonataResult<String> {
        match self {
            Value::Undefined => Ok(String::new()),
            Value::String(s) => Ok(s.to_string()),
            Value::Number(n) => Ok(format_float(*n)),
            Value::Bool(true) => Ok("true".into()),
            Value::Bool(false) => Ok("false".into()),
            Value::Function(_) => Ok(String::new()),
            Value::TailCall(_) => Ok(String::new()),
            other => {
                if other.contains_non_finite() {
                    return Err(JsonataError::new("D1001", "Number out of range"));
                }
                if prettify {
                    // Pretty-print still uses serde_json for indentation
                    let json_val = other.to_json();
                    serde_json::to_string_pretty(&json_val)
                        .map_err(|e| JsonataError::new("", format!("cannot stringify value: {e}")))
                } else {
                    Ok(other.to_json_string())
                }
            }
        }
    }

    /// Check if a value is "contained in" another (for `in` operator).
    pub fn contained_in(&self, arr: &Value) -> bool {
        match arr {
            Value::Array(items) => items.iter().any(|item| self.deep_equal(item)),
            Value::Sequence(seq) => seq.values.iter().any(|item| self.deep_equal(item)),
            other => self.deep_equal(other),
        }
    }

    // ── JSON conversion ──────────────────────────────────────────────

    /// Convert from serde_json::Value, preserving number precision where possible.
    pub fn from_json(v: serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => {
                // With arbitrary_precision, n.as_f64() parses the string repr
                Value::Number(n.as_f64().unwrap_or(f64::NAN))
            }
            serde_json::Value::String(s) => Value::String(CompactString::from(s)),
            serde_json::Value::Array(arr) => {
                let vec: Vec<Value> = arr.into_iter().map(Value::from_json).collect();
                Value::Array(Rc::from(vec))
            }
            serde_json::Value::Object(obj) => {
                // serde_json with preserve_order uses indexmap internally
                Value::Object(Rc::new(
                    obj.into_iter()
                        .map(|(k, v)| (CompactString::from(k), Value::from_json(v)))
                        .collect(),
                ))
            }
        }
    }

    /// Convert to serde_json::Value for serialization.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Undefined | Value::Null => serde_json::Value::Null,
            Value::Bool(b) => serde_json::Value::Bool(*b),
            Value::Number(n) => {
                if n.is_nan() || n.is_infinite() {
                    // NaN/Inf → null in JSON (matches JS behavior)
                    serde_json::Value::Null
                } else {
                    // ryu-js: exact ECMAScript Number.toString(). JSON output
                    // must round-trip (like JS JSON.stringify / Go json.Marshal);
                    // format_float's 'g'15 is only for $string() casting.
                    let s = ryu_js::Buffer::new().format_finite(*n).to_owned();
                    Number::from_string_unchecked(s).into()
                }
            }
            Value::String(s) => serde_json::Value::String(s.to_string()),
            Value::Array(arr) => serde_json::Value::Array(arr.iter().map(Value::to_json).collect()),
            Value::Object(obj) => serde_json::Value::Object(
                obj.iter()
                    .map(|(k, v)| (k.to_string(), v.to_json()))
                    .collect(),
            ),
            Value::Sequence(seq) => seq.collapse().to_json(),
            // Functions serialize as empty string in JSONata (matches Go's sanitizeForJSON).
            Value::Function(_) | Value::TailCall(_) => serde_json::Value::String(String::new()),
        }
    }

    /// Serialize directly to a byte buffer, skipping the serde_json::Value intermediate.
    /// Single pass, no intermediate tree allocation.
    pub fn write_json(&self, buf: &mut Vec<u8>) {
        match self {
            Value::Undefined | Value::Null => buf.extend_from_slice(b"null"),
            Value::Bool(true) => buf.extend_from_slice(b"true"),
            Value::Bool(false) => buf.extend_from_slice(b"false"),
            Value::Number(n) => {
                if n.is_nan() || n.is_infinite() {
                    buf.extend_from_slice(b"null");
                } else {
                    // ryu-js, not format_float: JSON output must round-trip
                    // (see to_json).
                    buf.extend_from_slice(ryu_js::Buffer::new().format_finite(*n).as_bytes());
                }
            }
            Value::String(s) => {
                buf.push(b'"');
                write_escaped_str(s.as_bytes(), buf);
                buf.push(b'"');
            }
            Value::Array(arr) => {
                buf.push(b'[');
                for (i, item) in arr.iter().enumerate() {
                    if i > 0 {
                        buf.push(b',');
                    }
                    item.write_json(buf);
                }
                buf.push(b']');
            }
            Value::Object(obj) => {
                buf.push(b'{');
                for (i, (k, v)) in obj.iter().enumerate() {
                    if i > 0 {
                        buf.push(b',');
                    }
                    buf.push(b'"');
                    write_escaped_str(k.as_bytes(), buf);
                    buf.push(b'"');
                    buf.push(b':');
                    v.write_json(buf);
                }
                buf.push(b'}');
            }
            Value::Sequence(seq) => seq.collapse().write_json(buf),
            Value::Function(_) | Value::TailCall(_) => buf.extend_from_slice(b"\"\""),
        }
    }

    /// Serialize to a JSON string using the direct-to-bytes path.
    pub fn to_json_string(&self) -> String {
        let mut buf = Vec::with_capacity(256);
        self.write_json(&mut buf);
        // SAFETY: write_json only produces valid UTF-8 (JSON is a subset of UTF-8)
        unsafe { String::from_utf8_unchecked(buf) }
    }

    /// Decode a JSON string into a Value, preserving object key order.
    ///
    /// Uses simd-json for SIMD-accelerated tokenization with a direct serde
    /// Visitor — no intermediate value tree.
    ///
    /// # Errors
    /// Returns `D0000` if the input is not valid JSON; the backend
    /// parser's diagnostic is embedded in the message.
    pub fn from_json_str(s: &str) -> JsonataResult<Self> {
        let mut buf = s.as_bytes().to_vec();
        simd_json::serde::from_slice(&mut buf)
            .map_err(|e| JsonataError::new("D0000", format!("JSON parse error: {e}")))
    }

    /// Decode a JSON byte slice into a Value (direct deserialization).
    ///
    /// # Errors
    /// Returns `D0000` if the input is not valid JSON; the backend
    /// parser's diagnostic is embedded in the message.
    pub fn from_json_bytes(b: &[u8]) -> JsonataResult<Self> {
        serde_json::from_slice(b)
            .map_err(|e| JsonataError::new("D0000", format!("JSON parse error: {e}")))
    }

    /// Decode a mutable byte slice using SIMD-accelerated parsing.
    ///
    /// This is the fastest path — no copy needed. The buffer is modified
    /// in-place by simd-json for SIMD alignment.
    ///
    /// # Errors
    /// Returns `D0000` if the input is not valid JSON; the backend
    /// parser's diagnostic is embedded in the message.
    pub fn from_json_bytes_mut(b: &mut [u8]) -> JsonataResult<Self> {
        simd_json::serde::from_slice(b)
            .map_err(|e| JsonataError::new("D0000", format!("JSON parse error: {e}")))
    }
}

// ── Direct serde::Deserialize for Value ─────────────────────────────
//
// Produces jsntrs::Value in a single pass, avoiding the intermediate
// serde_json::Value tree + conversion walk.

impl<'de> serde::Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(ValueVisitor)
    }
}

struct ValueVisitor;

impl<'de> serde::de::Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("any valid JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v as f64))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v as f64))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
        Ok(Value::Number(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(CompactString::from(v)))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(CompactString::from(v)))
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut vec = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(elem) = seq.next_element()? {
            vec.push(elem);
        }
        Ok(Value::Array(Rc::from(vec)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut obj = ObjectMap::with_capacity_and_hasher(
            map.size_hint().unwrap_or(0),
            foldhash::fast::RandomState::default(),
        );
        while let Some(key) = map.next_key::<CompactString>()? {
            let val: Value = map.next_value()?;
            obj.insert(key, val);
        }
        Ok(Value::Object(Rc::new(obj)))
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.deep_equal(other)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Number(n)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Number(n as f64)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(CompactString::from(s))
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(CompactString::from(s))
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(v: Vec<T>) -> Self {
        let vec: Vec<Value> = v.into_iter().map(Into::into).collect();
        Value::Array(Rc::from(vec))
    }
}

/// Compact JSON text (same output as [`Value::to_json_string`]), except
/// `Undefined`, which displays as the empty string — JSONata's absent
/// result is not the same value as `null`.
impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if matches!(self, Value::Undefined) {
            return Ok(());
        }
        f.write_str(&self.to_json_string())
    }
}

/// Write a JSON-escaped string to a byte buffer.
/// Handles the JSON spec escapes: `\"`, `\\`, `\n`, `\r`, `\t`, `\b`, `\f`,
/// and `\uXXXX` for control characters below 0x20.
fn write_escaped_str(src: &[u8], buf: &mut Vec<u8>) {
    let mut start = 0;
    for (i, &b) in src.iter().enumerate() {
        let escape = match b {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0x08 => b"\\b",
            0x0C => b"\\f",
            0x00..=0x1F => {
                buf.extend_from_slice(&src[start..i]);
                start = i + 1;
                buf.extend_from_slice(b"\\u00");
                let hi = b >> 4;
                let lo = b & 0x0F;
                buf.push(if hi < 10 { b'0' + hi } else { b'a' + hi - 10 });
                buf.push(if lo < 10 { b'0' + lo } else { b'a' + lo - 10 });
                continue;
            }
            _ => {
                continue;
            }
        };
        buf.extend_from_slice(&src[start..i]);
        buf.extend_from_slice(escape);
        start = i + 1;
    }
    buf.extend_from_slice(&src[start..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── write_json correctness ─────────────────────────────────────────

    /// Verify write_json matches serde_json output for all Value types and edge cases.
    #[test]
    fn json_constructors_return_jsonata_errors() {
        // M-DONT-LEAK-TYPES: all three return D0000 with the backend
        // diagnostic embedded, not simd-json/serde_json error types.
        assert_eq!(Value::from_json_str("{nope").unwrap_err().code, "D0000");
        assert_eq!(Value::from_json_bytes(b"{nope").unwrap_err().code, "D0000");
        let mut buf = b"{nope".to_vec();
        assert_eq!(
            Value::from_json_bytes_mut(&mut buf).unwrap_err().code,
            "D0000"
        );
    }

    #[test]
    fn display_is_compact_json_except_undefined() {
        let v = Value::from_json_str(r#"{"a": [1, "x", null, true]}"#).unwrap();
        assert_eq!(v.to_string(), r#"{"a":[1,"x",null,true]}"#);
        assert_eq!(Value::Undefined.to_string(), "");
        assert_eq!(Value::Null.to_string(), "null");
    }

    #[test]
    fn write_json_matches_serde_json() {
        let cases: Vec<Value> = vec![
            Value::Null,
            Value::Undefined,
            Value::Bool(true),
            Value::Bool(false),
            Value::Number(0.0),
            Value::Number(42.0),
            Value::Number(-3.25),
            Value::Number(1e20),
            Value::Number(f64::NAN),
            Value::Number(f64::INFINITY),
            Value::String("hello".into()),
            Value::String("".into()),
            Value::String("quote\"here".into()),
            Value::String("back\\slash".into()),
            Value::String("new\nline".into()),
            Value::String("tab\there".into()),
            Value::String("\x00\x01\x1f".into()), // control chars
            Value::String("unicode: \u{00e9}\u{1f600}".into()), // é and emoji
            Value::Array(Rc::from(vec![
                Value::Number(1.0),
                Value::String("two".into()),
            ])),
            Value::Array(Rc::from(vec![])),
            Value::Object(Rc::new(ObjectMap::default())),
        ];

        for val in &cases {
            let expected = serde_json::to_string(&val.to_json()).unwrap();
            let got = val.to_json_string();
            assert_eq!(expected, got, "mismatch for {val:?}");
        }

        // Nested object
        let mut obj = ObjectMap::default();
        obj.insert("key".into(), Value::String("val".into()));
        obj.insert("num".into(), Value::Number(99.0));
        obj.insert(
            "arr".into(),
            Value::Array(Rc::from(vec![Value::Bool(true)])),
        );
        let nested = Value::Object(Rc::new(obj));
        let expected = serde_json::to_string(&nested.to_json()).unwrap();
        assert_eq!(expected, nested.to_json_string());
    }

    /// JSON output must round-trip: numbers needing more than 15 significant
    /// digits keep full precision (ryu-js), unlike `$string()`'s 'g'15
    /// casting. Regression test for gnata-1jc, where `$sum` bench results
    /// printed 1 ULP off the js/Go reference output.
    #[test]
    fn json_number_output_round_trips() {
        // 25.1 * 3 * (1 - 0.1) in f64 — shortest form needs 16 digits.
        let n = 67.770_000_000_000_01_f64;
        let v = Value::Number(n);
        assert_eq!(v.to_json_string(), "67.77000000000001");
        assert_eq!(
            serde_json::to_string(&v.to_json()).unwrap(),
            "67.77000000000001"
        );
        let back = Value::from_json_str(&v.to_json_string()).unwrap();
        assert_eq!(back.as_f64().map(f64::to_bits), Some(n.to_bits()));
        // $string() casting intentionally stays at 15 significant digits.
        assert_eq!(format_float(n), "67.77");
    }

    // ── Size validation ───────────────────────────────────────────────

    #[test]
    fn value_size_is_compact() {
        let size = std::mem::size_of::<Value>();
        // CompactString is 24 bytes inline, so Value is 32 bytes
        // (discriminant + 24-byte String variant + alignment).
        // Tradeoff: 2x size vs eliminating 90%+ of string heap allocs.
        assert!(size <= 32, "Value should be ≤32 bytes, got {size}");
    }

    // ── Undefined/Null distinction ───────────────────────────────────

    #[test]
    fn undefined_equals_undefined_is_false() {
        // Critical invariant: undefined = undefined → false
        assert!(!Value::Undefined.deep_equal(&Value::Undefined));
    }

    #[test]
    fn null_equals_null_is_true() {
        assert!(Value::Null.deep_equal(&Value::Null));
    }

    #[test]
    fn null_not_equal_undefined() {
        assert!(!Value::Null.deep_equal(&Value::Undefined));
        assert!(!Value::Undefined.deep_equal(&Value::Null));
    }

    // ── Boolean coercion ─────────────────────────────────────────────

    #[test]
    fn boolean_coercion() {
        assert!(!Value::Undefined.to_boolean());
        assert!(!Value::Null.to_boolean());
        assert!(Value::Bool(true).to_boolean());
        assert!(!Value::Bool(false).to_boolean());
        assert!(Value::String("hello".into()).to_boolean());
        assert!(!Value::String("".into()).to_boolean());
        // "0" is truthy, "" is falsy, "false" is truthy
        assert!(Value::String("0".into()).to_boolean());
        assert!(Value::String("false".into()).to_boolean());
        assert!(Value::Number(1.0).to_boolean());
        assert!(!Value::Number(0.0).to_boolean());
    }

    #[test]
    fn boolean_array_coercion() {
        // Empty array → false
        assert!(!Value::Array(Rc::from(vec![])).to_boolean());
        // Single element → recurse
        assert!(Value::Array(Rc::from(vec![Value::Bool(true)])).to_boolean());
        assert!(!Value::Array(Rc::from(vec![Value::Bool(false)])).to_boolean());
        // Multiple → any truthy
        assert!(Value::Array(Rc::from(vec![Value::Bool(false), Value::Bool(true)])).to_boolean());
        assert!(!Value::Array(Rc::from(vec![Value::Bool(false), Value::Bool(false)])).to_boolean());
    }

    // ── Deep equality ────────────────────────────────────────────────

    #[test]
    fn deep_equal_numbers() {
        assert!(Value::Number(42.0).deep_equal(&Value::Number(42.0)));
        assert!(!Value::Number(42.0).deep_equal(&Value::Number(43.0)));
    }

    #[test]
    fn deep_equal_arrays() {
        let a = Value::Array(Rc::from(vec![Value::Number(1.0), Value::Number(2.0)]));
        let b = Value::Array(Rc::from(vec![Value::Number(1.0), Value::Number(2.0)]));
        let c = Value::Array(Rc::from(vec![Value::Number(1.0), Value::Number(3.0)]));
        assert!(a.deep_equal(&b));
        assert!(!a.deep_equal(&c));
    }

    #[test]
    fn deep_equal_objects() {
        let mut a = ObjectMap::default();
        a.insert(CompactString::from("x"), Value::Number(1.0));
        a.insert(CompactString::from("y"), Value::Number(2.0));

        let mut b = ObjectMap::default();
        b.insert(CompactString::from("y"), Value::Number(2.0));
        b.insert(CompactString::from("x"), Value::Number(1.0));

        // Order-independent comparison
        assert!(Value::Object(Rc::new(a)).deep_equal(&Value::Object(Rc::new(b))));
    }

    #[test]
    fn deep_equal_type_mismatch() {
        assert!(!Value::Number(1.0).deep_equal(&Value::String("1".into())));
        assert!(!Value::Bool(true).deep_equal(&Value::Number(1.0)));
    }

    // ── JSON round-trip ──────────────────────────────────────────────

    #[test]
    fn json_round_trip() {
        let json = r#"{"name":"test","values":[1,2,3],"active":true,"data":null}"#;
        let val = Value::from_json_str(json).unwrap();
        assert!(val.is_object());
        let obj = val.as_object().unwrap();
        assert_eq!(obj.get("name"), Some(&Value::String("test".into())));
        assert!(obj.get("data").unwrap().is_null());
    }

    #[test]
    fn json_preserves_key_order() {
        let json = r#"{"z":1,"a":2,"m":3}"#;
        let val = Value::from_json_str(json).unwrap();
        let obj = val.as_object().unwrap();
        let keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec!["z", "a", "m"]);
    }

    // ── Comparison ───────────────────────────────────────────────────

    #[test]
    fn compare_numbers() {
        let r = Value::Number(1.0)
            .compare(&Value::Number(2.0), CompareOp::Lt)
            .unwrap();
        assert_eq!(r, Value::Bool(true));
    }

    #[test]
    fn compare_undefined_propagates() {
        let r = Value::Number(1.0)
            .compare(&Value::Undefined, CompareOp::Lt)
            .unwrap();
        assert!(r.is_undefined());
    }

    #[test]
    fn compare_type_mismatch_error() {
        let r = Value::Number(1.0).compare(&Value::String("a".into()), CompareOp::Lt);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, "T2009");
    }

    // ── Stringify ────────────────────────────────────────────────────

    #[test]
    fn stringify_values() {
        assert_eq!(Value::Undefined.stringify(false).unwrap(), "");
        assert_eq!(Value::Number(42.0).stringify(false).unwrap(), "42");
        assert_eq!(Value::Bool(true).stringify(false).unwrap(), "true");
        assert_eq!(Value::String("hi".into()).stringify(false).unwrap(), "hi");
    }
}
