//! Subscript/filter evaluation: `lhs[rhs]` predicates and index selection.
//!
//! Split from `evaluator/mod.rs`; port of Go `internal/evaluator` subscript
//! handling, including index-binding (`#`) and keep-array interaction.

use std::rc::Rc;

use crate::error::{JsonataError, JsonataResult};
use crate::parser::BinaryOp;
use crate::parser::{AstArena, Expr, NodeId};
use crate::value::{Sequence, Value, format_float};

use super::binary::{flag_keep_array, has_keep_array};
use super::environment::Environment;
use super::path::node_has_parent_ref;
use super::{PARENT_BINDING, descendant_lookup, eval_no_stack_check, eval_operand};

/// Subscript/filter binary `[` — needs AST-level access to lhs for
/// Descendant checks, index_var extraction, and keep_array detection.
pub(super) fn eval_subscript_binary(
    arena: &AstArena,
    node: NodeId,
    lhs: NodeId,
    rhs: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    let left = if matches!(arena.get(lhs), Expr::Descendant { .. }) {
        let descendants = descendant_lookup(input);
        let mut seq = Sequence::new();
        seq.append(input.clone());
        match descendants {
            Value::Array(arr) => {
                for item in arr.iter() {
                    seq.append(item.clone());
                }
            }
            Value::Sequence(s) => {
                for item in s.values {
                    seq.append(item);
                }
            }
            Value::Undefined => {}
            other => seq.append(other),
        }
        seq.into_value()
    } else {
        eval_no_stack_check(arena, lhs, input, env)?
    };
    // Extract index variable from the LHS (e.g. $#$pos[...] → index_var="pos").
    let index_var = match arena.get(lhs) {
        Expr::Name { index, .. }
        | Expr::Variable { index, .. }
        | Expr::Binary { index, .. }
        | Expr::Sort { index, .. } => index.clone(),
        _ => None,
    };
    // Check keep_array: the [] suffix on this node or anywhere in the LHS chain.
    let keep_array = has_keep_array(arena, node);
    let result = eval_subscript(arena, rhs, &left, input, env, index_var.as_ref())?;
    if keep_array {
        // Undefined stays undefined: `[]` never manufactures an empty array.
        // jsonata-js `evaluate()` (jsonata.js 2.2.2, the tail of the big
        // switch) reads
        //     if(result && isSequence(result) && !result.tupleStream) {
        //         if(expr.keepArray) { result.keepSingleton = true; }
        //         if(result.length === 0) { result = undefined; }
        //         else if(result.length === 1) { … }
        //     }
        // — an undefined result skips the block entirely (`result &&`), and
        // an empty sequence is turned back into undefined *after* the
        // keepSingleton flag is set, so the flag never has anything to wrap.
        // Handing `[]` in as the undefined replacement made `a[0][]` on a
        // document with no `a` answer `[]` (jsntrs-k3a).
        Ok(flag_keep_array(result, Value::Undefined))
    } else {
        Ok(result)
    }
}

/// The `utils.isNumeric` gate every subscript number has to pass before it
/// may act as an index (jsonata.js 2.2.2, `isNumeric`):
///
/// ```js
/// function isNumeric(n) {
///     var isNum = false;
///     if(typeof n === 'number') {
///         isNum = !isNaN(n);
///         if (isNum && !isFinite(n)) {
///             throw { code: "D1001", value: n, stack: (new Error()).stack };
///         }
///     }
///     return isNum;
/// }
/// ```
///
/// So a finite number answers `Some(n)`; a NaN is simply *not numeric* and
/// answers `None`, falling through to the truthiness test where `fn.boolean`
/// finds it falsy; and an infinity is a hard `D1001` raised from inside the
/// check itself, which is why `[1,2,3][1/0]` is an error rather than a miss.
/// Anything that is not a number at all answers `None`.
///
/// This is *only* the consumption of a number as a subscript. Arithmetic
/// keeps propagating infinities unchanged — `1/0` is still `Infinity`, and
/// `evaluateNumericExpression` only guards its own operands (jsntrs-c7l).
fn numeric_index(value: &Value) -> JsonataResult<Option<f64>> {
    match value {
        Value::Number(n) if n.is_nan() => Ok(None),
        Value::Number(n) if !n.is_finite() => Err(JsonataError::new(
            "D1001",
            format!("Number out of range: {}", format_float(*n)),
        )
        .with_value(format_float(*n))),
        Value::Number(n) => Ok(Some(*n)),
        _ => Ok(None),
    }
}

/// The `utils.isArrayOfNumbers` gate for multi-index selection (`[[1,3]]`).
///
/// The reference builds it out of `isNumeric`:
/// `result = (arg.filter(item => !isNumeric(item)).length === 0)`. `filter`
/// visits every element, so the `D1001` an infinity raises is *not*
/// short-circuited away by an earlier non-numeric element — hence no early
/// return here: `[1,2,3][[0/0,1/0]]` is `D1001`, not a fall-through to the
/// truthiness test.
fn all_numeric_indices(items: &[Value]) -> JsonataResult<bool> {
    let mut all = true;
    for item in items {
        all &= numeric_index(item)?.is_some();
    }
    Ok(all)
}

fn eval_subscript(
    arena: &AstArena,
    rhs: NodeId,
    left: &Value,
    input: &Value,
    env: &Rc<Environment>,
    index_var: Option<&String>,
) -> JsonataResult {
    // Only create a child environment when the predicate actually needs it:
    // either it references % (parent operator) or it has an index variable binding.
    let needs_env = index_var.is_some() || node_has_parent_ref(arena, rhs);

    // For non-array inputs without index variable, evaluate directly.
    // (With an index variable, fall through: the predicate filter path
    // handles index binding correctly, like Go's evalSubscriptLeft.)
    if !matches!(left, Value::Array(_) | Value::Sequence(_)) && index_var.is_none() {
        return eval_subscript_single(arena, rhs, left, input, env, needs_env);
    }

    // Avoid cloning: borrow the array as a slice where possible.
    let owned_arr;
    let arr: &[Value] = match &left {
        Value::Array(a) => a,
        Value::Sequence(s) => {
            owned_arr = s.values.clone();
            &owned_arr
        }
        _ => {
            owned_arr = vec![left.clone()];
            &owned_arr
        }
    };

    // Try evaluating RHS as a simple expression (might be a numeric literal or
    // variable). If it resolves to a number, use it as a direct index.
    // If it resolves to an array of all-numeric values, use as index list.
    // If it errors or is non-numeric/non-index, fall through to per-element predicate filter.
    //
    // Optimization: skip the probe for comparison/boolean predicates — they can't
    // be numeric indices. Only probe when RHS could plausibly produce a number.
    let rhs_could_be_numeric = !matches!(arena.get(rhs),
        Expr::Binary { op, .. } if matches!(op, BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge | BinaryOp::And | BinaryOp::Or | BinaryOp::In)
    );
    // Probe with a representative element — the first item — like Go's
    // evalSubscript (rightCtx := items[0]): probing against the whole array
    // would auto-map a bare numeric field into an all-numeric array, turning
    // a predicate like a[i] into multi-index selection (one element per item).
    //
    // `arr.first()` also gates the probe on a non-empty array, because the
    // reference's `evaluateFilter` loop body never runs over an empty input:
    // `a[1/0]` on `{"a": []}` answers undefined there, so the probe must not
    // reach `numeric_index` and raise D1001 for it either.
    if let Some(probe_ctx) = arr.first()
        && rhs_could_be_numeric
        && let Ok(index) = eval_operand(arena, rhs, probe_ctx, env)
    {
        // Array of all-numeric values → select those indices (e.g. [[1..4]]).
        if let Value::Array(ref indices) = index
            && !indices.is_empty()
            && all_numeric_indices(indices)?
        {
            return Ok(select_by_indices(arr, indices));
        }
        // Single numeric index.
        if let Some(n) = numeric_index(&index)? {
            let idx = n.trunc() as i64;
            let len = arr.len() as i64;
            let actual = if idx < 0 { len + idx } else { idx };
            if actual < 0 || actual >= len {
                return Ok(Value::Undefined);
            }
            return Ok(arr[actual as usize].clone());
        }
        // Not numeric — fall through to per-element predicate filter below.
        let _ = index;
    }

    // Predicate filter — evaluate rhs against each element.
    // Only create a child env when the predicate uses % or has index variable bindings.
    let filter_env_owned: Option<Rc<Environment>> = if needs_env {
        let fe = Rc::new(Environment::new_child(Rc::clone(env)));
        fe.bind(PARENT_BINDING, input.clone());
        Some(fe)
    } else {
        None
    };
    let eval_env = filter_env_owned.as_ref().unwrap_or(env);
    let mut seq = Sequence::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        eval_env.poll_cancelled(i)?;
        // Bind index variable if present (e.g. $#$pos[...]).
        if let Some(var_name) = index_var {
            eval_env.bind(var_name.clone(), Value::Number(i as f64));
        }
        let test = eval_operand(arena, rhs, item, eval_env)?;
        // Numeric result = index selection from entire array.
        if let Some(n) = numeric_index(&test)? {
            let idx = n.trunc() as i64;
            let len = arr.len() as i64;
            let actual = if idx < 0 { len + idx } else { idx };
            if actual >= 0 && actual < len {
                return Ok(arr[actual as usize].clone());
            }
            return Ok(Value::Undefined);
        }
        // Boolean predicate — keep item if truthy.
        if test.to_boolean()? {
            seq.values.push(item.clone());
        }
    }
    Ok(seq.into_value())
}

/// Subscript on a single (non-array) value: numeric index 0/-1 selects the
/// value itself; any other number selects nothing; otherwise the result is a
/// boolean predicate on the value.
fn eval_subscript_single(
    arena: &AstArena,
    rhs: NodeId,
    left: &Value,
    input: &Value,
    env: &Rc<Environment>,
    needs_env: bool,
) -> JsonataResult {
    // Conditionally bind %% → input for the % operator.
    let filter_env_owned;
    let eval_env = if needs_env {
        filter_env_owned = Rc::new(Environment::new_child(Rc::clone(env)));
        filter_env_owned.bind(PARENT_BINDING, input.clone());
        &filter_env_owned
    } else {
        env
    };
    let index = eval_operand(arena, rhs, left, eval_env)?;
    if let Some(n) = numeric_index(&index)? {
        // Numeric index on a single value — treat as array of one.
        let idx = n.trunc() as i64;
        if idx == 0 || idx == -1 {
            return Ok(left.clone());
        }
        return Ok(Value::Undefined);
    }
    // Boolean predicate on single value.
    if index.to_boolean()? {
        return Ok(left.clone());
    }
    Ok(Value::Undefined)
}

/// Select elements by an all-numeric index list (e.g. [[1..3,8,-1]]).
/// Matches Go's selectByIndices: resolve negative indices (add len), sort
/// ascending, then select — [[1..3,8,-1]] on a 10-element array gives sorted
/// actual indices [1,2,3,8,9] → elements [2,3,4,9,10].
fn select_by_indices(arr: &[Value], indices: &[Value]) -> Value {
    let len = arr.len() as i64;
    let mut actual_indices: Vec<i64> = indices
        .iter()
        .map(|v| {
            let idx = v.as_f64().unwrap_or(0.0) as i64;
            if idx < 0 { len + idx } else { idx }
        })
        .collect();
    actual_indices.sort_unstable();
    let result: Vec<Value> = actual_indices
        .into_iter()
        .filter_map(|actual| {
            if actual >= 0 && actual < len {
                Some(arr[actual as usize].clone())
            } else {
                None
            }
        })
        .collect();
    if result.is_empty() {
        Value::Undefined
    } else {
        Value::Array(Rc::from(result))
    }
}
