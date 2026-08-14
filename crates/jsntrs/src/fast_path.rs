//! Fast-path analysis and evaluation for simple JSONata expressions.
//!
//! Classifies expressions at compile time into categories that can bypass
//! the full AST-walking evaluator:
//!
//! - **Pure path**: `Account.Name`, `a.b.c` — direct field traversal
//! - **Comparison**: `Price = 100`, `Name != "x"` — path + literal compare
//! - **Function**: `$exists(a.b)`, `$sum(prices)` — builtin on a path
//!
//! Port of Go `internal/parser/analysis.go` and `func_fast.go`.

use std::rc::Rc;

use compact_str::CompactString;
use simd_json::tape;

use crate::parser::{AstArena, BinaryOp, Expr, NodeId};
use crate::value::Value;

/// Test-only escape hatch to force every fast path off so differential
/// tests can compare fast-path results against the general evaluator.
#[doc(hidden)]
pub mod testing {
    use std::cell::Cell;

    thread_local! {
        static FAST_PATHS_DISABLED: Cell<bool> = const { Cell::new(false) };
    }

    /// Disable (or re-enable) all fast paths on the current thread.
    ///
    /// For differential testing only — never use in production code.
    #[cfg(feature = "internals")]
    #[doc(hidden)]
    pub fn set_fast_paths_disabled(disabled: bool) {
        FAST_PATHS_DISABLED.with(|c| c.set(disabled));
    }

    /// True when fast paths are disabled on the current thread.
    #[inline]
    pub(crate) fn fast_paths_disabled() -> bool {
        FAST_PATHS_DISABLED.with(std::cell::Cell::get)
    }
}

/// A literal value that is `Send + Sync` — used by `ComparisonFastPath`
/// so that `FastPath` (and thus `Expression`) can be shared across threads.
/// Comparison RHS values are always JSON scalars.
#[derive(Debug, Clone)]
pub enum Literal {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
}

/// Result of fast-path analysis on a compiled expression.
#[derive(Debug, Clone)]
pub enum FastPath {
    /// No fast path available — use full evaluator.
    None,
    /// Simple dotted field path: direct Value traversal.
    PurePath(Vec<String>),
    /// Binary = or != with pure-path LHS and literal RHS.
    Comparison(ComparisonFastPath),
    /// Supported builtin function applied to a pure-path argument.
    Function(FuncFastPath),
}

/// Metadata for comparison fast path.
#[derive(Debug, Clone)]
pub struct ComparisonFastPath {
    pub path: Vec<String>,
    pub op: ComparisonOp,
    pub rhs: Literal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonOp {
    Equal,
    NotEqual,
}

/// Metadata for function fast path.
#[derive(Debug, Clone)]
pub struct FuncFastPath {
    pub kind: FuncFastKind,
    pub path: Vec<String>,
    /// Second string argument for `$contains(path, "literal")`.
    pub str_arg: Option<String>,
}

/// Supported fast-path function kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuncFastKind {
    Exists,
    String,
    Boolean,
    Number,
    Not,
    Type,
    Lowercase,
    Uppercase,
    Trim,
    Length,
    Contains,
    Abs,
    Floor,
    Ceil,
    Sqrt,
    Keys,
    Values,
    Count,
    Sum,
    Max,
    Min,
    Average,
    Reverse,
    Distinct,
    Shuffle,
    Flatten,
}

// ── Analysis ────────────────────────────────────────────────────────

/// Analyze an AST node and return its fast-path classification.
/// Called once at compile time after `process_ast`.
pub fn analyze(arena: &AstArena, node: NodeId) -> FastPath {
    if node.is_empty() {
        return FastPath::None;
    }
    // Try pure path first.
    if let Some(path) = collect_pure_path(arena, node) {
        return FastPath::PurePath(path);
    }
    // Try comparison.
    if let Some(cmp) = try_comparison(arena, node) {
        return FastPath::Comparison(cmp);
    }
    // Try function.
    if let Some(func) = try_function(arena, node) {
        return FastPath::Function(func);
    }
    FastPath::None
}

/// Check if a node is a pure dotted path (no filters, wildcards, etc.).
/// Returns the path segments if so.
fn collect_pure_path(arena: &AstArena, node: NodeId) -> Option<Vec<String>> {
    match arena.get(node) {
        // Single name: `foo`
        Expr::Name {
            value,
            stages,
            group: None,
            focus: None,
            index: None,
            keep_array: false,
            ..
        } if stages.is_empty() => Some(vec![value.clone()]),

        // Dotted path: `a.b.c`
        Expr::Path {
            steps,
            group: None,
            keep_singleton_array: false,
            ..
        } => {
            let mut segments = Vec::with_capacity(steps.len());
            for &step in steps {
                match arena.get(step) {
                    Expr::Name {
                        value,
                        stages,
                        group: None,
                        focus: None,
                        index: None,
                        keep_array: false,
                        ..
                    } if stages.is_empty() => {
                        segments.push(value.clone());
                    }
                    _ => return None,
                }
            }
            Some(segments)
        }

        _ => None,
    }
}

/// Try to classify as a comparison fast path: `path = literal` or `path != literal`.
fn try_comparison(arena: &AstArena, node: NodeId) -> Option<ComparisonFastPath> {
    let (bin_op, lhs, rhs) = match arena.get(node) {
        Expr::Binary { op, lhs, rhs, .. } if *op == BinaryOp::Eq || *op == BinaryOp::Ne => {
            (*op, *lhs, *rhs)
        }
        _ => return None,
    };

    let path = collect_pure_path(arena, lhs)?;
    let rhs_val = extract_literal(arena, rhs)?;
    let op = if bin_op == BinaryOp::Eq {
        ComparisonOp::Equal
    } else {
        ComparisonOp::NotEqual
    };

    Some(ComparisonFastPath {
        path,
        op,
        rhs: rhs_val,
    })
}

/// Try to classify as a function fast path: `$func(path)` or `$contains(path, "lit")`.
fn try_function(arena: &AstArena, node: NodeId) -> Option<FuncFastPath> {
    let (procedure, arguments) = match arena.get(node) {
        Expr::Function {
            procedure,
            arguments,
            ..
        } => (*procedure, arguments.clone()),
        _ => return None,
    };

    // Procedure must be a variable reference to a builtin name.
    let func_name = match arena.get(procedure) {
        Expr::Variable { name, .. } => name.as_str(),
        _ => return None,
    };

    let kind = match func_name {
        "exists" => FuncFastKind::Exists,
        "string" => FuncFastKind::String,
        "boolean" => FuncFastKind::Boolean,
        "number" => FuncFastKind::Number,
        "not" => FuncFastKind::Not,
        "type" => FuncFastKind::Type,
        "lowercase" => FuncFastKind::Lowercase,
        "uppercase" => FuncFastKind::Uppercase,
        "trim" => FuncFastKind::Trim,
        "length" => FuncFastKind::Length,
        "abs" => FuncFastKind::Abs,
        "floor" => FuncFastKind::Floor,
        "ceil" => FuncFastKind::Ceil,
        "sqrt" => FuncFastKind::Sqrt,
        "keys" => FuncFastKind::Keys,
        "values" => FuncFastKind::Values,
        "count" => FuncFastKind::Count,
        "sum" => FuncFastKind::Sum,
        "max" => FuncFastKind::Max,
        "min" => FuncFastKind::Min,
        "average" => FuncFastKind::Average,
        "reverse" => FuncFastKind::Reverse,
        "distinct" => FuncFastKind::Distinct,
        "shuffle" => FuncFastKind::Shuffle,
        "flatten" => FuncFastKind::Flatten,
        "contains" => FuncFastKind::Contains,
        _ => return None,
    };

    // $contains special case: two args, second must be string literal.
    if kind == FuncFastKind::Contains {
        if arguments.len() != 2 {
            return None;
        }
        let path = collect_pure_path(arena, arguments[0])?;
        let str_arg = match arena.get(arguments[1]) {
            Expr::StringLit { value, .. } => value.clone(),
            _ => return None,
        };
        return Some(FuncFastPath {
            kind,
            path,
            str_arg: Some(str_arg),
        });
    }

    // Standard case: single pure-path argument.
    if arguments.len() != 1 {
        return None;
    }
    // Unwrap single-element array constructor: $func([path]) → $func(path)
    // In JSONata, [expr] doesn't nest when expr already yields an array.
    let inner_arg = match arena.get(arguments[0]) {
        Expr::Unary {
            op: crate::parser::ast::UnaryOp::ArrayCons,
            expressions,
            ..
        } if expressions.len() == 1 => expressions[0],
        _ => arguments[0],
    };
    let path = collect_pure_path(arena, inner_arg)?;

    Some(FuncFastPath {
        kind,
        path,
        str_arg: None,
    })
}

/// Extract a literal value from an AST node.
fn extract_literal(arena: &AstArena, node: NodeId) -> Option<Literal> {
    match arena.get(node) {
        Expr::StringLit { value, .. } => Some(Literal::String(value.clone())),
        Expr::NumberLit { value, .. } => Some(Literal::Number(*value)),
        Expr::ValueLit { value, .. } => match value.as_str() {
            "true" => Some(Literal::Bool(true)),
            "false" => Some(Literal::Bool(false)),
            "null" => Some(Literal::Null),
            _ => None,
        },
        _ => None,
    }
}

// ── Fast-path evaluation ────────────────────────────────────────────

/// Evaluate a fast-path expression against input data.
/// Returns `Some(result)` if handled, `None` if fallback to full eval is needed.
pub fn eval_fast(fast_path: &FastPath, input: &Value) -> Option<Value> {
    if testing::fast_paths_disabled() {
        return None;
    }
    match fast_path {
        FastPath::None => None,
        FastPath::PurePath(segments) => Some(eval_pure_path(segments, input)),
        FastPath::Comparison(cmp) => eval_comparison(cmp, input),
        FastPath::Function(func) => eval_function(func, input),
    }
}

/// Traverse a pure dotted path on a Value.
/// Walk a pre-parsed Value through pure path segments with array auto-mapping.
///
/// Simplified version of `evaluator::eval_name` — handles only the pure-path
/// case (no Sequence, no field_found tracking, no Undefined→Null substitution).
/// If path semantics change in `eval_name`, update this function to match.
fn eval_pure_path(segments: &[String], input: &Value) -> Value {
    let mut current = input.clone();
    for segment in segments {
        current = path_step(segment, &current);
        if current.is_undefined() {
            return Value::Undefined;
        }
    }
    current
}

/// One path step with array auto-mapping (mirror of `evaluator::eval_name`):
/// nested arrays auto-map recursively, each array level flattens one level
/// of array-valued results and collapses (0 → undefined, 1 → unwrapped,
/// n → array), so `a.b` on `{"a": [[{"b": 1}]]}` yields 1. A field that
/// resolved on some item but contributed no values (empty-array field)
/// collapses to `[]`, not undefined — this keeps `$exists(a.b)` true on
/// `{"a": [{"b": []}]}` like the general path.
pub(crate) fn path_step(segment: &str, input: &Value) -> Value {
    match input {
        Value::Object(obj) => obj.get(segment).cloned().unwrap_or(Value::Undefined),
        Value::Array(arr) => {
            let mut results = Vec::new();
            let mut field_found = false;
            for item in arr.iter() {
                match path_step(segment, item) {
                    Value::Undefined => {}
                    Value::Array(inner) => {
                        field_found = true;
                        results.extend(inner.iter().cloned());
                    }
                    other => {
                        field_found = true;
                        results.push(other);
                    }
                }
            }
            match results.len() {
                0 if field_found => Value::Array(Rc::from(results)),
                0 => Value::Undefined,
                1 => results.into_iter().next().unwrap_or(Value::Undefined),
                _ => Value::Array(Rc::from(results)),
            }
        }
        _ => Value::Undefined,
    }
}

/// Count matches along a pure path without materializing values.
/// Avoids cloning leaf values — just counts how many exist. The `bool`
/// reports whether the path resolved to a defined value at all: a field
/// holding an empty array counts 0 but IS found (`a.b` → `[]`), which is
/// what `$exists` needs.
///
/// Returns `None` when the data contains nested arrays: their per-level
/// singleton collapse can merge or split results in ways a count-only
/// traversal cannot reproduce, so the caller must fall back to full
/// evaluation.
fn count_pure_path(segments: &[String], input: &Value) -> Option<(usize, bool)> {
    if segments.is_empty() {
        return Some(match input {
            Value::Undefined => (0, false),
            Value::Array(arr) => (arr.len(), true),
            _ => (1, true),
        });
    }

    let segment = &segments[0];
    let rest = &segments[1..];

    match input {
        Value::Object(obj) => match obj.get(segment.as_str()) {
            Some(v) if !v.is_undefined() => count_pure_path(rest, v),
            _ => Some((0, false)),
        },
        Value::Array(arr) => {
            let mut total = 0;
            let mut found = false;
            for item in arr.iter() {
                match item {
                    Value::Object(obj) => {
                        if let Some(v) = obj.get(segment.as_str()) {
                            if rest.is_empty() {
                                match v {
                                    Value::Array(inner) => {
                                        // An array element inside the leaf
                                        // array can collapse to a different
                                        // count (singleton unwrap) — defer.
                                        if inner.iter().any(|e| matches!(e, Value::Array(_))) {
                                            return None;
                                        }
                                        total += inner.len();
                                        found = true;
                                    }
                                    Value::Undefined => {}
                                    _ => {
                                        total += 1;
                                        found = true;
                                    }
                                }
                            } else {
                                let (c, f) = count_pure_path(rest, v)?;
                                total += c;
                                found |= f;
                            }
                        }
                    }
                    Value::Array(_) => return None,
                    _ => {}
                }
            }
            Some((total, found))
        }
        _ => Some((0, false)),
    }
}

/// `$count` semantics on the per-segment collapsed path result — the exact
/// (but materializing) fallback when `count_pure_path` defers.
fn collapsed_count(segments: &[String], input: &Value) -> usize {
    match eval_pure_path(segments, input) {
        Value::Undefined => 0,
        Value::Array(arr) => arr.len(),
        _ => 1,
    }
}

/// Visit each leaf value along a pure path without cloning.
/// Calls `f` with a borrowed reference to each matching value.
///
/// Returns `false` when the data contains nested arrays: their per-level
/// singleton collapse can reshape the leaves, so the caller must fall
/// back to full evaluation.
#[must_use]
fn fold_pure_path<'a>(
    segments: &[String],
    input: &'a Value,
    f: &mut impl FnMut(&'a Value),
) -> bool {
    if segments.is_empty() {
        match input {
            Value::Array(arr) => {
                for item in arr.iter() {
                    f(item);
                }
            }
            Value::Undefined => {}
            other => f(other),
        }
        return true;
    }

    let segment = &segments[0];
    let rest = &segments[1..];

    match input {
        Value::Object(obj) => {
            if let Some(v) = obj.get(segment.as_str()) {
                return fold_pure_path(rest, v, f);
            }
            true
        }
        Value::Array(arr) => {
            for item in arr.iter() {
                match item {
                    Value::Object(obj) => {
                        if let Some(v) = obj.get(segment.as_str()) {
                            if rest.is_empty() {
                                match v {
                                    Value::Array(inner) => {
                                        for elem in inner.iter() {
                                            f(elem);
                                        }
                                    }
                                    Value::Undefined => {}
                                    other => f(other),
                                }
                            } else if !fold_pure_path(rest, v, f) {
                                return false;
                            }
                        }
                    }
                    Value::Array(_) => return false,
                    _ => {}
                }
            }
            true
        }
        _ => true,
    }
}

/// Evaluate a comparison fast path.
fn eval_comparison(cmp: &ComparisonFastPath, input: &Value) -> Option<Value> {
    let lhs = eval_pure_path(&cmp.path, input);

    // Undefined comparisons always return false.
    if lhs.is_undefined() {
        return Some(Value::Bool(false));
    }

    // Arrays require auto-mapping — fall back to full evaluator.
    if matches!(lhs, Value::Array(_)) {
        return None;
    }

    let rhs_val = match &cmp.rhs {
        Literal::Null => Value::Null,
        Literal::Bool(b) => Value::Bool(*b),
        Literal::Number(n) => Value::Number(*n),
        Literal::String(s) => Value::String(s.as_str().into()),
    };
    let matches = lhs.deep_equal(&rhs_val);
    let result = match cmp.op {
        ComparisonOp::Equal => matches,
        ComparisonOp::NotEqual => !matches,
    };
    Some(Value::Bool(result))
}

/// Evaluate a function fast path.
fn eval_function(func: &FuncFastPath, input: &Value) -> Option<Value> {
    // Aggregations that don't need materialized values — traverse and accumulate.
    match func.kind {
        FuncFastKind::Count => {
            let n = match count_pure_path(&func.path, input) {
                Some((n, _)) => n,
                None => collapsed_count(&func.path, input),
            };
            return Some(Value::Number(n as f64));
        }
        FuncFastKind::Exists => {
            // $exists is about definedness, not emptiness: a field holding
            // an empty array resolves to [] and exists, despite counting 0.
            let found = match count_pure_path(&func.path, input) {
                Some((_, found)) => found,
                None => !eval_pure_path(&func.path, input).is_undefined(),
            };
            return Some(Value::Bool(found));
        }
        FuncFastKind::Sum => {
            let mut total = 0.0_f64;
            let mut count = 0usize;
            let mut all_numbers = true;
            let clean = fold_pure_path(&func.path, input, &mut |v| {
                count += 1;
                if let Value::Number(n) = v {
                    total += n;
                } else {
                    all_numbers = false;
                }
            });
            // Nested arrays, non-number leaves (general path raises T0412),
            // or an empty path (general returns Undefined) → defer to full
            // eval.
            if !clean || !all_numbers || count == 0 {
                return None;
            }
            return Some(Value::Number(total));
        }
        FuncFastKind::Max => {
            let mut result: Option<f64> = None;
            let mut all_numbers = true;
            let clean = fold_pure_path(&func.path, input, &mut |v| {
                if let Value::Number(n) = v {
                    result = Some(result.map_or(*n, |cur| cur.max(*n)));
                } else {
                    all_numbers = false;
                }
            });
            if !clean || !all_numbers {
                return None;
            }
            return Some(result.map_or(Value::Undefined, Value::Number));
        }
        FuncFastKind::Min => {
            let mut result: Option<f64> = None;
            let mut all_numbers = true;
            let clean = fold_pure_path(&func.path, input, &mut |v| {
                if let Value::Number(n) = v {
                    result = Some(result.map_or(*n, |cur| cur.min(*n)));
                } else {
                    all_numbers = false;
                }
            });
            if !clean || !all_numbers {
                return None;
            }
            return Some(result.map_or(Value::Undefined, Value::Number));
        }
        FuncFastKind::Average => {
            let mut total = 0.0_f64;
            let mut count = 0_usize;
            let mut all_numbers = true;
            let clean = fold_pure_path(&func.path, input, &mut |v| {
                if let Value::Number(n) = v {
                    total += n;
                    count += 1;
                } else {
                    all_numbers = false;
                }
            });
            if !clean || !all_numbers {
                return None;
            }
            return Some(if count == 0 {
                Value::Undefined
            } else {
                Value::Number(total / count as f64)
            });
        }
        // $length only accepts strings (general path raises T0410 for
        // anything else) — materialize below and let apply_func handle
        // the string case, deferring everything else to full eval.
        _ => {}
    }

    let val = eval_pure_path(&func.path, input);

    // If path resolved to undefined, most functions should fall through
    // to full eval for proper error handling, except $exists.
    if val.is_undefined() {
        return Some(apply_func_undefined(func.kind));
    }

    apply_func(func, &val)
}

/// Handle undefined input for functions that have defined behavior on undefined.
fn apply_func_undefined(kind: FuncFastKind) -> Value {
    match kind {
        FuncFastKind::Exists
        | FuncFastKind::Count
        | FuncFastKind::Sum
        | FuncFastKind::Max
        | FuncFastKind::Min
        | FuncFastKind::Average => {
            unreachable!("aggregate kinds return early in eval_function and never materialize")
        }
        _ => Value::Undefined,
    }
}

/// Apply a fast-path function to a resolved value.
#[expect(clippy::too_many_lines)]
fn apply_func(func: &FuncFastPath, val: &Value) -> Option<Value> {
    match func.kind {
        FuncFastKind::Exists
        | FuncFastKind::Count
        | FuncFastKind::Sum
        | FuncFastKind::Max
        | FuncFastKind::Min
        | FuncFastKind::Average => {
            unreachable!("aggregate kinds return early in eval_function and never materialize")
        }

        FuncFastKind::Type => {
            let t = match val {
                Value::String(_) => "string",
                Value::Number(_) => "number",
                Value::Bool(_) => "boolean",
                Value::Null => "null",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
                Value::Undefined => return Some(Value::Undefined),
                _ => return None,
            };
            Some(Value::String(t.into()))
        }

        FuncFastKind::String => match val {
            Value::String(_) => Some(val.clone()),
            Value::Number(n) => Some(Value::String(crate::value::format_float(*n).into())),
            Value::Bool(b) => Some(Value::String(if *b { "true" } else { "false" }.into())),
            Value::Null => Some(Value::String("null".into())),
            _ => None,
        },

        FuncFastKind::Boolean => match val {
            Value::Bool(_) => Some(val.clone()),
            Value::String(s) => Some(Value::Bool(!s.is_empty())),
            Value::Number(n) => Some(Value::Bool(*n != 0.0)),
            Value::Null => Some(Value::Bool(false)),
            _ => None,
        },

        FuncFastKind::Number => match val {
            Value::Number(_) => Some(val.clone()),
            // Non-finite parses ("Infinity", "NaN") must defer so the
            // general path raises D3030, matching fn_number.
            Value::String(s) => s
                .parse::<f64>()
                .ok()
                .filter(|f| f.is_finite())
                .map(Value::Number),
            Value::Bool(b) => Some(Value::Number(if *b { 1.0 } else { 0.0 })),
            _ => None,
        },

        FuncFastKind::Not => Some(Value::Bool(!val.to_boolean())),

        FuncFastKind::Lowercase => match val {
            Value::String(s) => Some(Value::String(s.to_lowercase())),
            _ => None,
        },

        FuncFastKind::Uppercase => match val {
            Value::String(s) => Some(Value::String(s.to_uppercase())),
            _ => None,
        },

        FuncFastKind::Trim => match val {
            Value::String(s) => {
                let trimmed: Vec<&str> = s.split_whitespace().collect();
                Some(Value::String(trimmed.join(" ").into()))
            }
            _ => None,
        },

        FuncFastKind::Length => match val {
            Value::String(s) => Some(Value::Number(s.chars().count() as f64)),
            _ => None,
        },

        FuncFastKind::Contains => {
            let search = func.str_arg.as_deref()?;
            match val {
                Value::String(s) => Some(Value::Bool(s.contains(search))),
                _ => None,
            }
        }

        FuncFastKind::Abs => match val {
            Value::Number(n) => Some(Value::Number(n.abs())),
            _ => None,
        },

        FuncFastKind::Floor => match val {
            Value::Number(n) => Some(Value::Number(n.floor())),
            _ => None,
        },

        FuncFastKind::Ceil => match val {
            Value::Number(n) => Some(Value::Number(n.ceil())),
            _ => None,
        },

        FuncFastKind::Sqrt => match val {
            Value::Number(n) if *n >= 0.0 => Some(Value::Number(n.sqrt())),
            _ => None,
        },

        FuncFastKind::Keys => match val {
            Value::Object(obj) => {
                let keys: Vec<Value> = obj
                    .keys()
                    .map(|k| Value::String(k.as_str().into()))
                    .collect();
                Some(match keys.len() {
                    0 => Value::Undefined,
                    1 => keys.into_iter().next().unwrap_or(Value::Undefined),
                    _ => Value::Array(Rc::from(keys)),
                })
            }
            _ => None,
        },

        FuncFastKind::Values => match val {
            Value::Object(obj) => {
                let vals: Vec<Value> = obj.values().cloned().collect();
                Some(Value::Array(Rc::from(vals)))
            }
            _ => None,
        },

        FuncFastKind::Reverse => match val {
            Value::Array(arr) => {
                let mut rev = arr.to_vec();
                rev.reverse();
                Some(Value::Array(Rc::from(rev)))
            }
            _ => None,
        },

        FuncFastKind::Distinct => match val {
            Value::Array(arr) => {
                let mut seen = std::collections::HashSet::new();
                let mut result = Vec::new();
                for item in arr.iter() {
                    // Non-scalar items defer the whole call: the general
                    // path dedupes with deep_equal, which keyed dedup can't
                    // reproduce for objects (key order insensitivity).
                    let key = scalar_key(item)?;
                    if seen.insert(key) {
                        result.push(item.clone());
                    }
                }
                Some(Value::Array(Rc::from(result)))
            }
            _ => None,
        },

        FuncFastKind::Shuffle => match val {
            Value::Array(arr) => {
                let mut shuffled = arr.to_vec();
                for i in (1..shuffled.len()).rev() {
                    let j = fastrand::usize(..=i);
                    shuffled.swap(i, j);
                }
                Some(Value::Array(Rc::from(shuffled)))
            }
            _ => None,
        },

        FuncFastKind::Flatten => match val {
            Value::Array(arr) => Some(Value::Array(Rc::from(flatten_recursive(arr, usize::MAX)))),
            _ => None,
        },
    }
}

/// Recursively flatten nested arrays up to the given depth.
fn flatten_recursive(arr: &[Value], depth: usize) -> Vec<Value> {
    let mut result = Vec::new();
    for item in arr {
        if depth > 0
            && let Value::Array(inner) = item
        {
            result.extend(flatten_recursive(inner.as_ref(), depth - 1));
            continue;
        }
        result.push(item.clone());
    }
    result
}

// ── Tape-based evaluation (raw bytes, no Value tree) ───────────────

/// Evaluate a pure dotted path directly on raw JSON bytes using simd-json's tape.
/// Returns `Some(Value)` on success, `None` if the fast path doesn't apply.
///
/// This avoids building the full Value tree — only leaf values at the end of
/// the path are converted to `Value`. For 100K products with 10 fields each,
/// this means ~100K leaf conversions instead of ~1.5M Value allocations.
pub fn eval_tape_path(
    fast_path: &FastPath,
    json_bytes: &[u8],
) -> Option<Result<Value, Box<dyn std::error::Error>>> {
    if testing::fast_paths_disabled() {
        return None;
    }
    let FastPath::PurePath(segments) = fast_path else {
        return None;
    };

    let mut buf = json_bytes.to_vec();
    let tape = match simd_json::to_tape(&mut buf) {
        Ok(t) => t,
        Err(e) => return Some(Err(e.into())),
    };

    let root = tape.as_value();
    Some(Ok(tape_walk(root, segments)))
}

/// Intermediate result of a per-segment tape walk: the collapsed value at
/// the current step, held as un-materialized tape nodes.
enum TapeStep<'tape, 'input> {
    Undefined,
    One(tape::Value<'tape, 'input>),
    Many(Vec<tape::Value<'tape, 'input>>),
}

/// Walk a simd-json tape value through path segments with array auto-mapping.
///
/// Tape equivalent of `eval_pure_path` — the same per-segment algorithm
/// (auto-map with nested-array recursion, one-level flatten, 0/1/n collapse
/// at every array level), operating on tape nodes so only the final leaves
/// are converted to `Value`. A fused walk (whole remaining path per item) is
/// NOT equivalent: an intermediate singleton collapse switches the path back
/// to object-chain mode, which suppresses the terminal flatten. If path
/// semantics change in `eval_pure_path` or `evaluator::eval_name`, update
/// this function to match.
fn tape_walk(root: tape::Value<'_, '_>, segments: &[String]) -> Value {
    let mut cur = TapeStep::One(root);
    for segment in segments {
        cur = match cur {
            TapeStep::Undefined => return Value::Undefined,
            TapeStep::One(tv) => tape_path_step(segment, tv),
            TapeStep::Many(nodes) => tape_step_over(segment, nodes),
        };
    }
    match cur {
        TapeStep::Undefined => Value::Undefined,
        TapeStep::One(tv) => tape_to_value(tv),
        TapeStep::Many(nodes) => {
            let items: Vec<Value> = nodes.into_iter().map(tape_to_value).collect();
            Value::Array(Rc::from(items))
        }
    }
}

/// One path step on a single tape node (tape mirror of `path_step`).
fn tape_path_step<'t, 'i>(segment: &str, tv: tape::Value<'t, 'i>) -> TapeStep<'t, 'i> {
    if let Some(obj) = tv.as_object() {
        return match obj.get(segment) {
            Some(child) => TapeStep::One(child),
            None => TapeStep::Undefined,
        };
    }
    if let Some(arr) = tv.as_array() {
        let nodes: Vec<_> = (&arr).into_iter().collect();
        return tape_step_over(segment, nodes);
    }
    TapeStep::Undefined
}

/// Auto-map a segment over tape nodes (tape mirror of `path_step`'s array
/// arm): recurse into nested arrays, flatten one level of array-valued
/// results, collapse 0/1/n. A field found but contributing no values
/// collapses to an empty `Many` (materializes as `[]`), like `path_step`.
fn tape_step_over<'t, 'i>(segment: &str, nodes: Vec<tape::Value<'t, 'i>>) -> TapeStep<'t, 'i> {
    let mut results: Vec<tape::Value<'t, 'i>> = Vec::new();
    let mut field_found = false;
    for node in nodes {
        match tape_path_step(segment, node) {
            TapeStep::Undefined => {}
            TapeStep::One(child) => {
                field_found = true;
                if let Some(inner) = child.as_array() {
                    results.extend(&inner);
                } else {
                    results.push(child);
                }
            }
            TapeStep::Many(vs) => {
                field_found = true;
                results.extend(vs);
            }
        }
    }
    match results.len() {
        0 if field_found => TapeStep::Many(results),
        0 => TapeStep::Undefined,
        1 => match results.pop() {
            Some(v) => TapeStep::One(v),
            None => TapeStep::Undefined,
        },
        _ => TapeStep::Many(results),
    }
}

/// Convert a simd-json tape value to a jsntrs Value.
/// Only called for leaf values at the end of a path — not the full tree.
fn tape_to_value(val: tape::Value<'_, '_>) -> Value {
    use simd_json::prelude::*;

    if let Some(s) = val.as_str() {
        return Value::String(CompactString::from(s));
    }
    if let Some(b) = val.as_bool() {
        return Value::Bool(b);
    }
    if val.is_null() {
        return Value::Null;
    }
    // cast_f64, not as_f64: integers sit on the tape as I64/U64 static
    // nodes and strict as_f64 returns None for them.
    if let Some(n) = val.cast_f64() {
        return Value::Number(n);
    }

    // Container at leaf position — convert recursively
    if let Some(arr) = val.as_array() {
        let items: Vec<Value> = arr.iter().map(tape_to_value).collect();
        return Value::Array(Rc::from(items));
    }
    if let Some(obj) = val.as_object() {
        let mut map = crate::value::ObjectMap::default();
        for (k, v) in &obj {
            map.insert(CompactString::from(k), tape_to_value(v));
        }
        return Value::Object(Rc::new(map));
    }

    Value::Undefined
}

/// Dedup key for $distinct, scalars only — must agree with deep_equal.
/// Returns None for non-scalars (and NaN, which is never equal to itself).
fn scalar_key(val: &Value) -> Option<String> {
    match val {
        Value::String(s) => Some(format!("s:{s}")),
        Value::Number(n) if n.is_nan() => None,
        // -0.0 == 0.0 under deep_equal — normalize so they share a key.
        Value::Number(n) => Some(format!("n:{}", if *n == 0.0 { 0.0 } else { *n })),
        Value::Bool(b) => Some(format!("b:{b}")),
        Value::Null => Some("null".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Expression;

    /// Helper: evaluate via Expression API (uses fast path when available)
    /// and via full evaluator, then compare results.
    fn assert_fast_matches_full(expr: &str, json: &str) {
        let input = Value::from_json_str(json).unwrap_or(Value::Undefined);
        let compiled = Expression::compile(expr).expect("compile failed");
        let fast_result = compiled.evaluate_value(&input).expect("eval failed");

        // Also run through full evaluator (bypassing fast path).
        let (mut arena, root) = crate::parser::Parser::parse(expr).expect("parse failed");
        let root = crate::parser::process_ast(&mut arena, root).expect("process failed");
        let mut env = crate::evaluator::Environment::new();
        crate::stdlib::register_all(&mut env);
        if !input.is_undefined() {
            env.bind("$", input.clone());
        }
        let env = std::rc::Rc::new(env);
        let full_result =
            crate::evaluator::eval(&arena, root, &input, &env).expect("full eval failed");

        assert_eq!(
            fast_result.to_json(),
            full_result.to_json(),
            "fast-path and full evaluator disagree for expr={expr:?}"
        );
    }

    #[test]
    fn pure_path_simple() {
        assert_fast_matches_full("name", r#"{"name": "Alice"}"#);
    }

    #[test]
    fn pure_path_dotted() {
        assert_fast_matches_full("a.b.c", r#"{"a": {"b": {"c": 42}}}"#);
    }

    #[test]
    fn pure_path_missing() {
        assert_fast_matches_full("a.b.x", r#"{"a": {"b": {"c": 42}}}"#);
    }

    #[test]
    fn pure_path_array_auto_map() {
        assert_fast_matches_full(
            "orders.product",
            r#"{"orders": [{"product": "A"}, {"product": "B"}]}"#,
        );
    }

    #[test]
    fn comparison_equal_string() {
        assert_fast_matches_full("name = \"Alice\"", r#"{"name": "Alice"}"#);
    }

    #[test]
    fn comparison_equal_number() {
        assert_fast_matches_full("age = 30", r#"{"age": 30}"#);
    }

    #[test]
    fn comparison_not_equal() {
        assert_fast_matches_full("name != \"Bob\"", r#"{"name": "Alice"}"#);
    }

    #[test]
    fn comparison_missing_field() {
        assert_fast_matches_full("x = 1", r#"{"y": 2}"#);
    }

    #[test]
    fn func_exists() {
        assert_fast_matches_full("$exists(name)", r#"{"name": "Alice"}"#);
    }

    #[test]
    fn func_exists_missing() {
        assert_fast_matches_full("$exists(x)", r#"{"y": 1}"#);
    }

    #[test]
    fn func_type() {
        assert_fast_matches_full("$type(name)", r#"{"name": "Alice"}"#);
    }

    #[test]
    fn func_string() {
        assert_fast_matches_full("$string(age)", r#"{"age": 42}"#);
    }

    #[test]
    fn func_count() {
        assert_fast_matches_full("$count(items)", r#"{"items": [1, 2, 3]}"#);
    }

    #[test]
    fn func_sum() {
        assert_fast_matches_full("$sum(prices)", r#"{"prices": [10, 20, 30]}"#);
    }

    #[test]
    fn func_lowercase() {
        assert_fast_matches_full("$lowercase(name)", r#"{"name": "ALICE"}"#);
    }

    #[test]
    fn func_contains() {
        assert_fast_matches_full("$contains(name, \"li\")", r#"{"name": "Alice"}"#);
    }

    #[test]
    fn func_length() {
        assert_fast_matches_full("$length(name)", r#"{"name": "hello"}"#);
    }

    #[test]
    fn func_abs() {
        assert_fast_matches_full("$abs(val)", r#"{"val": -5}"#);
    }

    #[test]
    fn func_keys() {
        assert_fast_matches_full("$keys(obj)", r#"{"obj": {"a": 1, "b": 2}}"#);
    }

    #[test]
    fn classification_pure_path() {
        let expr = Expression::compile("a.b.c").unwrap();
        assert!(matches!(expr.fast_path_info(), FastPath::PurePath(_)));
    }

    #[test]
    fn classification_comparison() {
        let expr = Expression::compile("x = 1").unwrap();
        assert!(matches!(expr.fast_path_info(), FastPath::Comparison(_)));
    }

    #[test]
    fn classification_function() {
        let expr = Expression::compile("$sum(prices)").unwrap();
        assert!(matches!(expr.fast_path_info(), FastPath::Function(_)));
    }

    #[test]
    fn classification_complex_no_fast_path() {
        let expr = Expression::compile("a.b[c > 5]").unwrap();
        assert!(matches!(expr.fast_path_info(), FastPath::None));
    }

    #[test]
    fn classification_wildcard_no_fast_path() {
        let expr = Expression::compile("a.*").unwrap();
        assert!(matches!(expr.fast_path_info(), FastPath::None));
    }

    #[test]
    fn tape_eval_simple_path() {
        let expr = Expression::compile("Account.Name").unwrap();
        let json = r#"{"Account":{"Name":"Firefly"}}"#;
        let result = expr.evaluate_bytes(json.as_bytes()).unwrap();
        assert_eq!(result.as_str(), Some("Firefly"));
    }

    #[test]
    fn tape_eval_nested_array() {
        let expr = Expression::compile("Account.Order.Product.SKU").unwrap();
        let json = r#"{"Account":{"Order":[{"Product":[{"SKU":"A"},{"SKU":"B"}]},{"Product":[{"SKU":"C"}]}]}}"#;
        let result = expr.evaluate_bytes(json.as_bytes()).unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 3);
    }

    #[test]
    fn tape_eval_falls_back_for_non_pure_path() {
        let expr = Expression::compile("Account.Order[id > 1].Name").unwrap();
        let json = r#"{"Account":{"Order":[{"id":1,"Name":"A"},{"id":2,"Name":"B"}]}}"#;
        // Should fall back to full eval, still produce correct result
        let result = expr.evaluate_bytes(json.as_bytes()).unwrap();
        assert_eq!(result.as_str(), Some("B"));
    }
}
