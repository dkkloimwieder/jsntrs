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

    // ── Boolean coercion ─────────────────────────────────────────────

    /// Implements JSONata boolean casting rules.
    ///
    /// - Undefined/Null → false
    /// - Bool → value
    /// - String → non-empty
    /// - Number → non-zero, but `Inf` is **D1001** and `NaN` is false
    /// - Object → non-empty
    /// - Array: len 0 → false, len 1 → recurse, len > 1 → any truthy
    /// - Sequence → collapse then recurse
    ///
    /// # Errors
    /// Returns `D1001` if the value is — or contains — an infinity. The
    /// reference reaches its number branch through `utils.isNumeric`, which
    /// throws on a non-finite number rather than answering the question
    /// (jsntrs-p0v.25).
    pub fn to_boolean(&self) -> JsonataResult<bool> {
        Ok(match self {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::String(s) => !s.is_empty(),
            Value::Number(n) => {
                if n.is_infinite() {
                    return Err(JsonataError::with_code("D1001").with_value(format_float(*n)));
                }
                // `isNumeric(NaN)` is false without throwing, so the
                // reference's `boolean()` walks past its number branch and
                // every branch after it — a NaN is falsy, not "non-zero".
                !n.is_nan() && *n != 0.0
            }
            Value::Object(m) => !m.is_empty(),
            Value::Array(arr) => match arr.len() {
                0 => false,
                1 => arr[0].to_boolean()?,
                // The reference filters the whole array instead of
                // short-circuiting, so an infinity anywhere in it raises
                // D1001 even when an earlier element is already truthy.
                _ => {
                    let mut any = false;
                    for item in arr.iter() {
                        any |= item.to_boolean()?;
                    }
                    any
                }
            },
            Value::Sequence(seq) => seq.collapse().to_boolean()?,
            Value::Function(_) | Value::TailCall(_) => false,
        })
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

    /// Whether [`Value::string_cast`] would rewrite anything in this tree —
    /// i.e. whether it holds a non-integral number anywhere.
    ///
    /// A cheap no-allocation pre-pass, so the common all-integer container
    /// serializes straight out of the shared tree.
    fn needs_string_cast(&self) -> bool {
        match self {
            Value::Number(n) => n.is_finite() && n.fract() != 0.0,
            Value::Array(arr) => arr.iter().any(Value::needs_string_cast),
            Value::Object(obj) => obj.values().any(Value::needs_string_cast),
            Value::Sequence(seq) => seq.values.iter().any(Value::needs_string_cast),
            _ => false,
        }
    }

    /// jsonata-js's `$string` replacer as a value transform: every
    /// non-integral number becomes `Number(val.toPrecision(15))`, integers
    /// keep their exact digits (jsonata 2.2.2, `string()` in
    /// `src/functions.js`).
    ///
    /// Running the replacer over the *values* and then handing the result to
    /// the ordinary exact JSON writer is exactly what `JSON.stringify(arg,
    /// replacer, space)` does, and it keeps the two number-output layers
    /// apart: `write_json` still emits round-tripping `ryu-js` digits, and
    /// only the `$string` cast path calls this first (jsntrs-wvq).
    ///
    /// Cheap to call on an unchanged subtree only if guarded by
    /// [`Value::needs_string_cast`]; on its own it rebuilds containers.
    fn string_cast(&self) -> Value {
        match self {
            Value::Number(n) => Value::Number(format::string_cast_number(*n)),
            Value::Array(arr) => Value::Array(arr.iter().map(Value::string_cast).collect()),
            Value::Object(obj) => Value::Object(Rc::new(
                obj.iter()
                    .map(|(k, v)| (k.clone(), v.string_cast()))
                    .collect(),
            )),
            // Matches the writers, which serialize a sequence as its
            // collapsed form; a Sequence must never reach user-visible
            // output uncollapsed.
            Value::Sequence(seq) => seq.collapse().string_cast(),
            other => other.clone(),
        }
    }

    /// A bare non-finite number cannot be stringified: **D3001**.
    ///
    /// The rule is the reference's `string()` (jsonata 2.2.2
    /// `jsonata.js:1484-1490`): a *bare* `Infinity`/`NaN` argument throws
    /// D3001, while a composite that merely *contains* one goes through
    /// `JSON.stringify`, whose replacer calls `isNumeric` and throws
    /// **D1001** instead (`jsonata.js:7497`). Both halves apply to `$string`
    /// and to `&`, which is defined in terms of it — hence the split
    /// between this guard and [`Value::contains_non_finite`] below.
    fn non_finite_guard(&self) -> JsonataResult<()> {
        match self {
            Value::Number(n) if !n.is_finite() => Err(JsonataError::new(
                "D3001",
                "cannot stringify Infinity or NaN",
            )),
            _ => Ok(()),
        }
    }

    /// Append the stringified form of this value to `buf`.
    ///
    /// Strings append verbatim (unquoted), booleans as `true`/`false`,
    /// numbers via `format_float`; undefined and functions append nothing;
    /// objects and arrays append their compact JSON with every non-integral
    /// member g15-cast (see [`Value::string_cast`]).
    ///
    /// # Errors
    /// Returns `D3001` for a bare non-finite number and `D1001` for a
    /// composite value containing one — see [`Value::non_finite_guard`].
    pub fn stringify_into(&self, buf: &mut String) -> JsonataResult<()> {
        match self {
            Value::Undefined | Value::Function(_) | Value::TailCall(_) => Ok(()),
            Value::String(s) => {
                buf.push_str(s);
                Ok(())
            }
            Value::Number(n) => {
                self.non_finite_guard()?;
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
                let cast = other.needs_string_cast().then(|| other.string_cast());
                buf.push_str(&cast.as_ref().unwrap_or(other).to_json_string());
                Ok(())
            }
        }
    }

    /// The `$string` cast of this value.
    ///
    /// Containers go through [`Value::string_cast`] first — both branches,
    /// compact and prettified — so a non-integral member serializes with the
    /// 15 significant digits jsonata-js's replacer leaves it, while the
    /// writers themselves stay the exact round-tripping JSON layer.
    ///
    /// # Errors
    /// Returns `D3001` for a bare non-finite number and `D1001` for a
    /// composite value containing one — see [`Value::non_finite_guard`].
    pub fn stringify(&self, prettify: bool) -> JsonataResult<String> {
        match self {
            Value::Undefined => Ok(String::new()),
            Value::String(s) => Ok(s.to_string()),
            Value::Number(n) => {
                self.non_finite_guard()?;
                Ok(format_float(*n))
            }
            Value::Bool(true) => Ok("true".into()),
            Value::Bool(false) => Ok("false".into()),
            Value::Function(_) => Ok(String::new()),
            Value::TailCall(_) => Ok(String::new()),
            other => {
                if other.contains_non_finite() {
                    return Err(JsonataError::new("D1001", "Number out of range"));
                }
                let cast = other.needs_string_cast().then(|| other.string_cast());
                let target = cast.as_ref().unwrap_or(other);
                if prettify {
                    // Pretty-print still uses serde_json for indentation
                    let json_val = target.to_json();
                    serde_json::to_string_pretty(&json_val)
                        .map_err(|e| JsonataError::new("", format!("cannot stringify value: {e}")))
                } else {
                    Ok(target.to_json_string())
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
                // With arbitrary_precision, n.as_f64() parses the string repr —
                // but it discards a non-finite result, because serde_json has no
                // way to serialize one back. `JSON.parse("1e400")` is `Infinity`
                // and JSONata computes with it (D1001 only where the spec says
                // so), so re-read the raw text when as_f64 declines.
                Value::Number(
                    n.as_f64()
                        .or_else(|| parse_number_token(&n.to_string()))
                        .unwrap_or(f64::NAN),
                )
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
    /// Visitor — no intermediate value tree. Number literals `JSON.parse`
    /// accepts but simd-json refuses are recovered by a retry (see
    /// [`Self::from_json_bytes`]).
    ///
    /// # Errors
    /// Returns `D0000` if the input is not valid JSON; the backend
    /// parser's diagnostic is embedded in the message.
    pub fn from_json_str(s: &str) -> JsonataResult<Self> {
        Self::from_json_bytes(s.as_bytes())
    }

    /// Decode a JSON byte slice into a Value, preserving object key order.
    ///
    /// Same parser and semantics as [`Self::from_json_str`]: the bytes are
    /// copied into a scratch buffer because simd-json rewrites its input in
    /// place. Use [`Self::from_json_bytes_mut`] to skip the copy.
    ///
    /// simd-json is stricter than `JSON.parse` about two number literals:
    /// integers past `u64` range and exponents that overflow `f64`
    /// (`1e400`). Because the caller's bytes survive the copy, a document
    /// simd-json rejects is re-parsed leniently through serde_json before the
    /// error is reported, so both widen to the value JavaScript would
    /// produce — the nearest `f64`, and `Infinity`.
    ///
    /// # Errors
    /// Returns `D0000` if the input is not valid JSON (or not valid UTF-8);
    /// the backend parser's diagnostic is embedded in the message.
    pub fn from_json_bytes(b: &[u8]) -> JsonataResult<Self> {
        let mut buf = b.to_vec();
        // `DeValue<false>`: simd-json hands every number to the visitor as a
        // number, so object keys are taken literally (see the visitor below).
        match simd_json::serde::from_slice::<DeValue<false>>(&mut buf) {
            Ok(wrapped) => Ok(wrapped.0),
            Err(e) => retry_lenient(b).ok_or_else(|| json_parse_error(&e)),
        }
    }

    /// Decode a mutable byte slice using SIMD-accelerated parsing.
    ///
    /// This is the fastest path — no copy needed. The buffer is modified
    /// in-place by simd-json for SIMD alignment.
    ///
    /// Strict where [`Self::from_json_bytes`] is lenient: simd-json unescapes
    /// strings into the caller's buffer as it goes, so by the time an
    /// out-of-range number literal is reached the original document is gone
    /// and there is nothing left to re-parse. Callers that want `JSON.parse`
    /// acceptance of oversized integers and overflowing exponents must pay
    /// the copy and use [`Self::from_json_bytes`].
    ///
    /// # Errors
    /// Returns `D0000` if the input is not valid JSON; the backend
    /// parser's diagnostic is embedded in the message.
    pub fn from_json_bytes_mut(b: &mut [u8]) -> JsonataResult<Self> {
        simd_json::serde::from_slice::<DeValue<false>>(b)
            .map(|wrapped| wrapped.0)
            .map_err(|e| json_parse_error(&e))
    }
}

/// Wrap a simd-json failure in the `D0000` this crate reports.
///
/// simd-json's `Display` prints its `ErrorType` with `{:?}`, so the catch-all
/// it raises for input its two-stage parser cannot even tokenize — a leading
/// UTF-8 BOM is the everyday case — reaches users as the Rust variant name
/// `InternalError(TapeError)`. Give that one plain wording and keep
/// simd-json's own text for the diagnoses that already read as JSON problems
/// (`InvalidNumber`, `UnterminatedString`, …).
fn json_parse_error(e: &simd_json::Error) -> JsonataError {
    if matches!(e.error(), simd_json::ErrorType::InternalError(_)) {
        let msg = match e.character() {
            Some(c) => format!("malformed JSON at character {} ('{c}')", e.index()),
            None => format!("malformed JSON at character {}", e.index()),
        };
        return JsonataError::new("D0000", format!("JSON parse error: {msg}"));
    }
    JsonataError::new("D0000", format!("JSON parse error: {e}"))
}

/// Re-parse a document simd-json rejected, accepting the number literals
/// `JSON.parse` accepts.
///
/// simd-json refuses integers past `u64` range and any literal whose value
/// overflows to infinity (`numberparse/correct.rs` errors on `is_infinite`).
/// serde_json is built here with `arbitrary_precision`, which hands every
/// number to the visitor as raw text instead; [`parse_number_token`] widens
/// it to the nearest `f64`, keeping `Infinity` exactly as `JSON.parse` does.
///
/// Runs only after simd-json has already failed, so the happy path never
/// touches it, and returns `None` for genuinely malformed input so the
/// caller can report simd-json's more precise diagnostic. One wrinkle comes
/// with the lenient parser: unlike the strict pass it reads a lone
/// `{"$serde_json::private::Number": "<number>"}` object as that number
/// (serde_json's own `Value` does the same). Reaching it takes a document
/// that pairs such an object with an out-of-range literal elsewhere.
fn retry_lenient(original: &[u8]) -> Option<Value> {
    serde_json::from_slice::<Value>(original).ok()
}

// ── Direct serde::Deserialize for Value ─────────────────────────────
//
// Produces jsntrs::Value in a single pass, avoiding the intermediate
// serde_json::Value tree + conversion walk.
//
// The visitor runs in two modes, picked by its const parameter:
//
// * `false` — the crate's own simd-json entry points (`from_json_str`,
//   `from_json_bytes`, `from_json_bytes_mut`). Every JSON number arrives as
//   `visit_i64`/`visit_u64`/`visit_f64`, so object keys are taken literally.
// * `true` — the public `Deserialize` impl, which any serde data format may
//   drive. serde_json is built here with `arbitrary_precision`, and that
//   feature makes `deserialize_any` present numbers it cannot hand over as
//   i64/u64 (fractions, exponents, oversized integers) as a one-entry map
//   keyed by `NUMBER_TOKEN` whose value is the raw number text. Decoding that
//   shape is what serde_json's own `Value` does; without it,
//   `serde_json::from_slice::<Value>(br#"{"a":1.5}"#)` silently yields
//   `{"a":{"$serde_json::private::Number":"1.5"}}` (jsntrs-ecq.1).

/// serde_json's `arbitrary_precision` escape hatch: the key of the synthetic
/// one-entry map that stands in for a number `deserialize_any` cannot pass
/// through as i64/u64.
const NUMBER_TOKEN: &str = "$serde_json::private::Number";

/// Parse the raw number text serde_json carries under [`NUMBER_TOKEN`].
///
/// Restricted to JSON's number grammar: `f64::from_str` also accepts `inf`,
/// `NaN` and `+1`, spellings no JSON document can produce, and an object that
/// merely happens to use the token as a key must survive as an object.
/// Overflow to infinity is kept — `JSON.parse("1e400")` is `Infinity` too.
fn parse_number_token(text: &str) -> Option<f64> {
    let starts_ok = matches!(text.as_bytes().first(), Some(b'-' | b'0'..=b'9'));
    let chars_ok = text
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E'));
    if !starts_ok || !chars_ok {
        return None;
    }
    text.parse().ok()
}

impl<'de> serde::Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(ValueVisitor::<true>)
    }
}

/// `Value` newtype that carries the visitor mode into nested elements.
struct DeValue<const ARBITRARY_PRECISION: bool>(Value);

impl<'de, const ARBITRARY_PRECISION: bool> serde::Deserialize<'de>
    for DeValue<ARBITRARY_PRECISION>
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer
            .deserialize_any(ValueVisitor::<ARBITRARY_PRECISION>)
            .map(DeValue)
    }
}

struct ValueVisitor<const ARBITRARY_PRECISION: bool>;

impl<'de, const ARBITRARY_PRECISION: bool> serde::de::Visitor<'de>
    for ValueVisitor<ARBITRARY_PRECISION>
{
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
        while let Some(elem) = seq.next_element::<DeValue<ARBITRARY_PRECISION>>()? {
            vec.push(elem.0);
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
        let mut next = map.next_key::<CompactString>()?;
        while let Some(key) = next {
            let val = map.next_value::<DeValue<ARBITRARY_PRECISION>>()?.0;
            next = map.next_key()?;
            // A number in disguise only when this is the whole map: a real
            // document that uses the token as one key among several, or pairs
            // it with anything but JSON number text, stays an object.
            if ARBITRARY_PRECISION
                && obj.is_empty()
                && next.is_none()
                && key == NUMBER_TOKEN
                && let Value::String(text) = &val
                && let Some(n) = parse_number_token(text)
            {
                return Ok(Value::Number(n));
            }
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

    /// jsntrs-ecq.1: `from_json_bytes` used to run through serde_json, whose
    /// `arbitrary_precision` feature turned every number that is not a plain
    /// i64/u64 into a one-entry map keyed by a private token.
    #[test]
    fn from_json_bytes_decodes_every_number_shape() {
        let doc = r#"{"int":2,"frac":1.5,"exp":1e3,"negexp":-1.5e-7,"big":12345678901234567890,"huge":1e308,"neg":-0.25,"arr":[0.1,-2.5e-3]}"#;
        let v = Value::from_json_bytes(doc.as_bytes()).unwrap();
        assert_eq!(
            v.to_json_string(),
            r#"{"int":2,"frac":1.5,"exp":1000,"negexp":-1.5e-7,"big":12345678901234567000,"huge":1e+308,"neg":-0.25,"arr":[0.1,-0.0025]}"#
        );
        // Round-trip: re-parsing the serialized form reproduces the Value.
        let round_tripped = Value::from_json_bytes(v.to_json_string().as_bytes()).unwrap();
        assert_eq!(round_tripped, v);

        // Scalars at the document root, not just object members.
        assert_eq!(Value::from_json_bytes(b"1.5").unwrap(), Value::Number(1.5));
        assert_eq!(
            Value::from_json_bytes(b"-1.5e-7").unwrap(),
            Value::Number(-1.5e-7)
        );
    }

    /// jsntrs-ztg: simd-json refuses two number literals `JSON.parse` accepts
    /// — integers past `u64` range and any literal that overflows to infinity.
    /// The copying constructors retry leniently, so both land on the value
    /// JavaScript produces: `JSON.parse("123456789012345678901")` is
    /// `1.2345678901234568e20` (stringified back as `123456789012345680000`)
    /// and `JSON.parse("1e400")` is `Infinity`, which `JSON.stringify` writes
    /// as `null`.
    #[test]
    fn oversized_number_literals_parse_like_json_parse() {
        let big = Value::from_json_str("123456789012345678901").unwrap();
        assert_eq!(big.as_f64(), Some(1.234_567_890_123_456_8e20));
        assert_eq!(big.to_json_string(), "123456789012345680000");
        // The `$string()` casting layer agrees here: jsonata-js's stringify
        // replacer only rounds *non*-integers to 15 significant digits, so a
        // whole number this large keeps every digit on both layers
        // (jsntrs-p0v.24).
        assert_eq!(
            format_float(1.234_567_890_123_456_8e20),
            "123456789012345680000"
        );

        assert_eq!(
            Value::from_json_str("1e400").unwrap().as_f64(),
            Some(f64::INFINITY)
        );
        assert_eq!(
            Value::from_json_bytes(b"-1e400").unwrap().as_f64(),
            Some(f64::NEG_INFINITY)
        );
        assert_eq!(
            Value::from_json_str("1e400").unwrap().to_json_string(),
            "null"
        );

        // Nested, mixed with literals simd-json handles on the first pass.
        let doc = r#"{"big":123456789012345678901,"inf":[1e400,-1E400],"ok":1.5}"#;
        let nested = Value::from_json_bytes(doc.as_bytes()).unwrap();
        let text = nested.to_json_string();
        assert_eq!(
            text,
            r#"{"big":123456789012345680000,"inf":[null,null],"ok":1.5}"#
        );
        // The serialized form is back inside simd-json's range, so it
        // re-parses on the strict pass (infinities land as null, like
        // JSON.parse(JSON.stringify(…)) in JavaScript).
        assert_eq!(Value::from_json_str(&text).unwrap().to_json_string(), text);

        // Still a retry, not a second parser: malformed input keeps failing.
        assert_eq!(Value::from_json_str("{nope").unwrap_err().code, "D0000");
        assert_eq!(
            Value::from_json_str("[1e400,").unwrap_err().code,
            "D0000",
            "an out-of-range literal must not rescue a truncated document"
        );
    }

    /// The zero-copy constructor stays strict: simd-json unescapes strings
    /// into the caller's buffer as it goes, so by the time it reaches the bad
    /// literal the original document is gone and there is nothing left to
    /// re-parse. Documented divergence — leniency costs the copy.
    #[test]
    fn from_json_bytes_mut_stays_strict_on_oversized_literals() {
        for doc in ["1e400", "123456789012345678901"] {
            let mut buf = doc.as_bytes().to_vec();
            assert_eq!(
                Value::from_json_bytes_mut(&mut buf).unwrap_err().code,
                "D0000",
                "{doc} must still be rejected by the zero-copy constructor"
            );
            assert!(
                Value::from_json_bytes(doc.as_bytes()).is_ok(),
                "{doc} must be accepted by the copying constructor"
            );
        }
    }

    /// A leading UTF-8 BOM is rejected, like `JSON.parse` rejects it — but
    /// the diagnostic has to read as a JSON problem. simd-json's `Display`
    /// prints its error enum with `{:?}`, which leaked the Rust variant name
    /// `InternalError(TapeError)` (jsntrs-ztg).
    #[test]
    fn bom_error_reads_as_a_json_diagnostic() {
        let bom_doc = "\u{feff}{\"a\":1}";
        let mut buf = bom_doc.as_bytes().to_vec();
        let errors = [
            Value::from_json_str(bom_doc).unwrap_err(),
            Value::from_json_bytes(bom_doc.as_bytes()).unwrap_err(),
            Value::from_json_bytes_mut(&mut buf).unwrap_err(),
        ];
        for err in errors {
            assert_eq!(err.code, "D0000");
            assert!(
                !err.message.contains("InternalError") && !err.message.contains("TapeError"),
                "leaked simd-json internals: {}",
                err.message
            );
            assert!(
                err.message.contains("malformed JSON"),
                "unexpected message: {}",
                err.message
            );
        }
    }

    /// The serde_json interop path had the same ceiling from the other side:
    /// `Number::as_f64` discards a non-finite result (serde_json cannot
    /// re-serialize one), so `1e400` arrived as NaN. `JSON.parse` gives
    /// Infinity, and the difference is observable — `Infinity > 1e308` is
    /// true where NaN's comparison is false (jsntrs-ztg).
    #[test]
    fn from_json_keeps_an_overflowing_exponent_as_infinity() {
        let doc: serde_json::Value =
            serde_json::from_str(r#"{"a":1e400,"b":-1e400,"c":123456789012345678901}"#).unwrap();
        let v = Value::from_json(doc);
        let Value::Object(obj) = &v else {
            panic!("expected an object, got {v:?}");
        };
        assert_eq!(obj["a"].as_f64(), Some(f64::INFINITY));
        assert_eq!(obj["b"].as_f64(), Some(f64::NEG_INFINITY));
        assert_eq!(obj["c"].as_f64(), Some(1.234_567_890_123_456_8e20));
    }

    /// All three constructors share one parser, so they must agree — numbers
    /// included, and on objects that literally use serde_json's private
    /// number token as a key (those stay objects, never collapse to numbers).
    ///
    /// Out-of-range literals are the one exception, pinned separately by
    /// `from_json_bytes_mut_stays_strict_on_oversized_literals`.
    #[test]
    fn json_constructors_agree() {
        let docs = [
            r#"{"a":1.5,"b":2,"c":1e3}"#,
            "[0.1,-2.5e-3,1e308,9007199254740993]",
            r#"{"$serde_json::private::Number":"1.5"}"#,
            r#"{"$serde_json::private::Number":"1.5","x":1}"#,
            r#"{"$serde_json::private::Number":{"nested":true}}"#,
            r#"{"outer":{"$serde_json::private::Number":"2.5"}}"#,
        ];
        for doc in docs {
            let via_str = Value::from_json_str(doc).unwrap();
            let via_bytes = Value::from_json_bytes(doc.as_bytes()).unwrap();
            let mut buf = doc.as_bytes().to_vec();
            let via_mut = Value::from_json_bytes_mut(&mut buf).unwrap();
            assert_eq!(via_bytes, via_str, "from_json_bytes disagrees on {doc}");
            assert_eq!(via_mut, via_str, "from_json_bytes_mut disagrees on {doc}");
        }

        // Verbatim round-trip of the token-keyed documents.
        let token_doc = r#"{"$serde_json::private::Number":"1.5"}"#;
        let v = Value::from_json_bytes(token_doc.as_bytes()).unwrap();
        assert!(matches!(v, Value::Object(_)));
        assert_eq!(v.to_json_string(), token_doc);
    }

    /// The public `Deserialize` impl can be driven by serde_json, where
    /// `arbitrary_precision` hides numbers behind the private token map.
    #[test]
    fn serde_json_deserialize_decodes_arbitrary_precision_numbers() {
        let v: Value =
            serde_json::from_str(r#"{"a":1.5,"b":2,"c":1e3,"d":[-1.5e-7],"e":{"f":0.1}}"#).unwrap();
        assert_eq!(
            v.to_json_string(),
            r#"{"a":1.5,"b":2,"c":1000,"d":[-1.5e-7],"e":{"f":0.1}}"#
        );
        let scalar: Value = serde_json::from_slice(b"1.5").unwrap();
        assert_eq!(scalar, Value::Number(1.5));
        // serde_json also routes integers it cannot fit in u64 through the token.
        let oversized: Value = serde_json::from_str("123456789012345678901234567890").unwrap();
        assert_eq!(oversized, Value::Number(1.234_567_890_123_456_8e29));
    }

    /// Driven by serde_json, a one-entry object whose key is the private
    /// token and whose value is JSON number text is indistinguishable from
    /// that number — serde_json's own `Value` decodes it identically. Every
    /// other shape stays an object (serde_json's `Value` errors on those).
    #[test]
    fn serde_json_deserialize_keeps_token_objects_that_are_not_numbers() {
        let text: Value =
            serde_json::from_str(r#"{"$serde_json::private::Number":"hello"}"#).unwrap();
        assert!(matches!(text, Value::Object(_)), "got {text}");
        let extra_key: Value =
            serde_json::from_str(r#"{"$serde_json::private::Number":"1.5","x":1}"#).unwrap();
        assert!(matches!(extra_key, Value::Object(_)), "got {extra_key}");
        let nested: Value =
            serde_json::from_str(r#"{"$serde_json::private::Number":{"a":1}}"#).unwrap();
        assert!(matches!(nested, Value::Object(_)), "got {nested}");
        let not_first: Value =
            serde_json::from_str(r#"{"x":1,"$serde_json::private::Number":"1.5"}"#).unwrap();
        assert!(matches!(not_first, Value::Object(_)), "got {not_first}");

        // The documented ambiguity, and the simd-json entry point that is
        // free of it.
        let ambiguous: Value =
            serde_json::from_str(r#"{"$serde_json::private::Number":"1.5"}"#).unwrap();
        assert_eq!(ambiguous, Value::Number(1.5));
        let unambiguous =
            Value::from_json_str(r#"{"$serde_json::private::Number":"1.5"}"#).unwrap();
        assert!(matches!(unambiguous, Value::Object(_)));
    }

    #[test]
    fn number_token_text_follows_json_grammar() {
        assert_eq!(parse_number_token("1.5"), Some(1.5));
        assert_eq!(parse_number_token("-1.5e-7"), Some(-1.5e-7));
        assert_eq!(parse_number_token("1e+3"), Some(1000.0));
        assert_eq!(parse_number_token("1e400"), Some(f64::INFINITY));
        // Spellings f64::from_str accepts but JSON cannot produce.
        assert_eq!(parse_number_token("inf"), None);
        assert_eq!(parse_number_token("NaN"), None);
        assert_eq!(parse_number_token("+1"), None);
        assert_eq!(parse_number_token(" 1"), None);
        assert_eq!(parse_number_token(""), None);
        assert_eq!(parse_number_token("hello"), None);
        assert_eq!(parse_number_token("1.2.3"), None);
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
        assert!(!Value::Undefined.to_boolean().unwrap());
        assert!(!Value::Null.to_boolean().unwrap());
        assert!(Value::Bool(true).to_boolean().unwrap());
        assert!(!Value::Bool(false).to_boolean().unwrap());
        assert!(Value::String("hello".into()).to_boolean().unwrap());
        assert!(!Value::String("".into()).to_boolean().unwrap());
        // "0" is truthy, "" is falsy, "false" is truthy
        assert!(Value::String("0".into()).to_boolean().unwrap());
        assert!(Value::String("false".into()).to_boolean().unwrap());
        assert!(Value::Number(1.0).to_boolean().unwrap());
        assert!(!Value::Number(0.0).to_boolean().unwrap());
    }

    /// jsonata-js 2.2.2-verified (2026-08-15, jsntrs-p0v.25): the reference's
    /// `boolean()` reaches its number branch through `utils.isNumeric`, which
    /// throws D1001 on an infinity and answers a plain `false` for NaN — so a
    /// NaN falls past every remaining branch and comes out falsy rather than
    /// "non-zero".
    #[test]
    fn boolean_coercion_rejects_infinity_and_reads_nan_as_false() {
        assert_eq!(
            Value::Number(f64::INFINITY).to_boolean().unwrap_err().code,
            "D1001"
        );
        assert_eq!(
            Value::Number(f64::NEG_INFINITY)
                .to_boolean()
                .unwrap_err()
                .code,
            "D1001"
        );
        assert!(!Value::Number(f64::NAN).to_boolean().unwrap());
        // An infinity nested in an array is still D1001, and the reference
        // filters the whole array, so an earlier truthy element does not
        // short-circuit past it.
        let nested = Value::Array(Rc::from(vec![Value::Number(f64::INFINITY)]));
        assert_eq!(nested.to_boolean().unwrap_err().code, "D1001");
        let late = Value::Array(Rc::from(vec![
            Value::Bool(true),
            Value::Number(f64::INFINITY),
        ]));
        assert_eq!(late.to_boolean().unwrap_err().code, "D1001");
        // An object holding one is not: the reference only counts its keys.
        let mut obj = ObjectMap::default();
        obj.insert("a".into(), Value::Number(f64::INFINITY));
        assert!(Value::Object(Rc::new(obj)).to_boolean().unwrap());
    }

    #[test]
    fn boolean_array_coercion() {
        // Empty array → false
        assert!(!Value::Array(Rc::from(vec![])).to_boolean().unwrap());
        // Single element → recurse
        assert!(
            Value::Array(Rc::from(vec![Value::Bool(true)]))
                .to_boolean()
                .unwrap()
        );
        assert!(
            !Value::Array(Rc::from(vec![Value::Bool(false)]))
                .to_boolean()
                .unwrap()
        );
        // Multiple → any truthy
        assert!(
            Value::Array(Rc::from(vec![Value::Bool(false), Value::Bool(true)]))
                .to_boolean()
                .unwrap()
        );
        assert!(
            !Value::Array(Rc::from(vec![Value::Bool(false), Value::Bool(false)]))
                .to_boolean()
                .unwrap()
        );
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

    /// jsonata-js 2.2.2-verified (2026-08-15, jsntrs-wvq): `$string` on a
    /// container runs `JSON.stringify` with a replacer that pushes every
    /// *non-integral* number through `Number(val.toPrecision(15))` — so a
    /// container member is cast exactly like a bare number would be, at any
    /// nesting depth, while integers keep their exact digits.
    #[test]
    fn stringify_container_casts_non_integral_members() {
        let arr = Value::Array(Rc::from(vec![Value::Number(1_234_567_890_123_456.7)]));
        assert_eq!(arr.stringify(false).unwrap(), "[1234567890123460]");

        let nested = Value::Array(Rc::from(vec![Value::Array(Rc::from(vec![Value::Number(
            0.430_801_391_601_562_5,
        )]))]));
        assert_eq!(nested.stringify(false).unwrap(), "[[0.430801391601563]]");

        let mut obj = ObjectMap::default();
        obj.insert("b".into(), Value::Number(0.430_801_391_601_562_5));
        obj.insert("c".into(), Value::Number(5_890_840_712_243_076.0));
        let obj = Value::Object(Rc::new(obj));
        assert_eq!(
            obj.stringify(false).unwrap(),
            "{\"b\":0.430801391601563,\"c\":5890840712243076}"
        );
        // Key order survives the rebuild (invariant #4).
        let mut outer = ObjectMap::default();
        outer.insert("z".into(), Value::Number(22.0 / 7.0));
        outer.insert("a".into(), obj.clone());
        assert_eq!(
            Value::Object(Rc::new(outer)).stringify(false).unwrap(),
            "{\"z\":3.14285714285714,\"a\":{\"b\":0.430801391601563,\"c\":5890840712243076}}"
        );

        // The prettify branch takes the same cast.
        assert_eq!(arr.stringify(true).unwrap(), "[\n  1234567890123460\n]");
        assert_eq!(
            obj.stringify(true).unwrap(),
            "{\n  \"b\": 0.430801391601563,\n  \"c\": 5890840712243076\n}"
        );

        // Integers, at every size, are left exact by the replacer's
        // `!Number.isInteger(val)` guard.
        let ints = Value::Array(Rc::from(vec![
            Value::Number(9_007_199_254_740_994.0),
            Value::Number(1.234_567_890_123_456_8e20),
            Value::Number(1e21),
        ]));
        assert_eq!(
            ints.stringify(false).unwrap(),
            "[9007199254740994,123456789012345680000,1e+21]"
        );
        assert!(!ints.needs_string_cast());
    }

    /// The cast belongs to the `$string` path only: the JSON layer keeps
    /// emitting exact round-tripping `ryu-js` digits for the same value.
    #[test]
    fn stringify_cast_leaves_the_json_layer_alone() {
        let arr = Value::Array(Rc::from(vec![
            Value::Number(1_234_567_890_123_456.7),
            Value::Number(0.430_801_391_601_562_5),
            Value::Number(0.1 + 0.2),
        ]));
        assert_eq!(
            arr.to_json_string(),
            "[1234567890123456.8,0.4308013916015625,0.30000000000000004]"
        );
        assert_eq!(
            arr.stringify(false).unwrap(),
            "[1234567890123460,0.430801391601563,0.3]"
        );
        // The value itself is untouched — the cast builds a new tree.
        assert_eq!(
            arr.to_json_string(),
            "[1234567890123456.8,0.4308013916015625,0.30000000000000004]"
        );
    }
}
