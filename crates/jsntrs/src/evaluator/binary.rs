//! Binary operators: arithmetic, comparison, concatenation, and ranges.
//!
//! Split from `evaluator/mod.rs`; port of Go `internal/evaluator` binary
//! dispatch, with left-associative chains flattened iteratively.

use std::rc::Rc;

use crate::error::{JsonataError, JsonataResult};
use crate::parser::{AstArena, BinaryOp, Expr, NodeId};
use crate::value::{CompareOp, Value};

use super::environment::Environment;
use super::subscript::eval_subscript_binary;
use super::{
    collapse_sequence_in_place, eval_chain, eval_no_stack_check, eval_operand,
    eval_with_stack_check,
};

// ── Binary operators (stub for Phase 5, full impl in Phase 7) ───────

// Large dispatch function for all binary operator types.
// Flattens left-associative chains iteratively to avoid deep recursion.
pub(super) fn eval_binary(
    arena: &AstArena,
    node: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    let (op, lhs, rhs) = match arena.get(node) {
        Expr::Binary { op, lhs, rhs, .. } => (*op, *lhs, *rhs),
        _ => unreachable!("eval_binary is dispatched only for Expr::Binary nodes"),
    };

    // Handle subscript `[` separately — it needs AST-level lhs access
    // for Descendant checks, index_var extraction, and keep_array.
    if op == BinaryOp::Subscript {
        return eval_subscript_binary(arena, node, lhs, rhs, input, env);
    }

    // Fast path: flatten chained concat (a & b & c) into a single buffer.
    if op == BinaryOp::Concat {
        return eval_concat_chain(arena, node, input, env);
    }

    // Only use stacker check when lhs is a Binary (could recurse into another
    // eval_binary creating a deep chain). Leaf nodes (Name, Number, Variable, etc.)
    // are evaluated directly without the closure overhead.
    // Operands are consumer positions: collapse a sequence here, the way
    // the reference's `evaluate()` does for each operand expression.
    let mut left = if matches!(arena.get(lhs), Expr::Binary { .. }) {
        eval_with_stack_check(arena, lhs, input, env)?
    } else {
        eval_no_stack_check(arena, lhs, input, env)?
    };
    collapse_sequence_in_place(&mut left);
    apply_binary_op(arena, op, left, rhs, lhs, input, env)
}

/// Flatten and evaluate a chain of `&` concat operators into a single buffer.
/// Turns `a & b & c & d` (left-recursive tree) into a linear sequence of evals
/// with a single String allocation.
fn eval_concat_chain(
    arena: &AstArena,
    node: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    // Collect leaf operands by walking the left-recursive concat tree.
    let mut operands: Vec<NodeId> = Vec::new();
    collect_concat_operands(arena, node, &mut operands);

    let mut buf = String::new();
    for &operand in &operands {
        let val = eval_operand(arena, operand, input, env)?;
        if !val.is_undefined() {
            val.stringify_into(&mut buf)?;
        }
    }
    Ok(Value::String(buf.into()))
}

/// Walk a left-recursive Concat tree and collect non-Concat leaf nodes.
fn collect_concat_operands(arena: &AstArena, node: NodeId, out: &mut Vec<NodeId>) {
    if let Expr::Binary {
        op: BinaryOp::Concat,
        lhs,
        rhs,
        ..
    } = arena.get(node)
    {
        collect_concat_operands(arena, *lhs, out);
        out.push(*rhs);
    } else {
        out.push(node);
    }
}

/// Apply a single binary operator given a pre-evaluated left value and an unevaluated rhs node.
/// `lhs_node` is the original LHS NodeId (used only for subscript `[` AST inspection).
fn apply_binary_op(
    arena: &AstArena,
    op: BinaryOp,
    left: Value,
    rhs: NodeId,
    _lhs_node: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    match op {
        // Short-circuit operators.
        BinaryOp::And => {
            if !left.to_boolean()? {
                return Ok(Value::Bool(false));
            }
            let right = eval_operand(arena, rhs, input, env)?;
            Ok(Value::Bool(right.to_boolean()?))
        }
        BinaryOp::Or => {
            if left.to_boolean()? {
                return Ok(Value::Bool(true));
            }
            let right = eval_operand(arena, rhs, input, env)?;
            Ok(Value::Bool(right.to_boolean()?))
        }
        BinaryOp::CondTern => {
            if left.to_boolean()? {
                Ok(left)
            } else {
                eval_operand(arena, rhs, input, env)
            }
        }
        BinaryOp::NullCoal => {
            if left.is_undefined() {
                eval_operand(arena, rhs, input, env)
            } else {
                Ok(left)
            }
        }
        BinaryOp::Chain => eval_chain(arena, rhs, &left, input, env),
        // Arithmetic operators.
        BinaryOp::Add
        | BinaryOp::Sub
        | BinaryOp::Mul
        | BinaryOp::Div
        | BinaryOp::Mod
        | BinaryOp::Pow => {
            let right = eval_operand(arena, rhs, input, env)?;
            apply_arithmetic(op, &left, &right)
        }
        // String concatenation — handled by eval_concat_chain before reaching here.
        // This fallback exists only if somehow reached directly (shouldn't happen).
        BinaryOp::Concat => {
            let right = eval_operand(arena, rhs, input, env)?;
            let mut buf = String::new();
            if !left.is_undefined() {
                left.stringify_into(&mut buf)?;
            }
            if !right.is_undefined() {
                right.stringify_into(&mut buf)?;
            }
            Ok(Value::String(buf.into()))
        }
        // Equality.
        BinaryOp::Eq => {
            let right = eval_operand(arena, rhs, input, env)?;
            if left.is_undefined() || right.is_undefined() {
                return Ok(Value::Bool(false));
            }
            Ok(Value::Bool(left.deep_equal(&right)))
        }
        BinaryOp::Ne => {
            let right = eval_operand(arena, rhs, input, env)?;
            if left.is_undefined() || right.is_undefined() {
                return Ok(Value::Bool(false));
            }
            Ok(Value::Bool(!left.deep_equal(&right)))
        }
        // Comparison.
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            let right = eval_operand(arena, rhs, input, env)?;
            compare_values(&left, &right, op)
        }
        // Membership.
        BinaryOp::In => {
            let right = eval_operand(arena, rhs, input, env)?;
            Ok(Value::Bool(left.contained_in(&right)))
        }
        // Range.
        BinaryOp::Range => {
            let right = eval_operand(arena, rhs, input, env)?;
            apply_range(&left, &right)
        }
        _ => Err(JsonataError::new(
            "D3001",
            format!("unknown binary operator: {op}"),
        )),
    }
}

/// Typed comparison — avoids string matching on the operator in the hot loop.
#[inline]
fn compare_values(left: &Value, right: &Value, op: BinaryOp) -> JsonataResult {
    let cmp = match op {
        BinaryOp::Lt => CompareOp::Lt,
        BinaryOp::Le => CompareOp::Le,
        BinaryOp::Gt => CompareOp::Gt,
        BinaryOp::Ge => CompareOp::Ge,
        _ => {
            return Err(JsonataError::new(
                "D3001",
                format!("unknown comparison operator: {op}"),
            ));
        }
    };
    // Fast path: both numbers (the common case in filter predicates).
    if let (Value::Number(a), Value::Number(b)) = (left, right) {
        let result = match cmp {
            CompareOp::Lt => a < b,
            CompareOp::Le => a <= b,
            CompareOp::Gt => a > b,
            CompareOp::Ge => a >= b,
        };
        return Ok(Value::Bool(result));
    }
    // Slow path: delegates to Value::compare for type checking and error messages.
    left.compare(right, cmp)
}

/// Apply arithmetic operator to pre-evaluated values.
pub(crate) fn apply_arithmetic(op: BinaryOp, left: &Value, right: &Value) -> JsonataResult {
    // Fast path: both are numbers (the common case in arithmetic expressions).
    if let (Value::Number(ln), Value::Number(rn)) = (left, right) {
        return apply_arithmetic_nums(op, *ln, *rn);
    }
    // Slow path: type-check non-undefined operands BEFORE undefined propagation.
    if !left.is_undefined() && !left.is_number() {
        return Err(JsonataError::new(
            "T2001",
            "the left operand must be a number",
        ));
    }
    if !right.is_undefined() && !right.is_number() {
        return Err(JsonataError::new(
            "T2002",
            "the right operand must be a number",
        ));
    }
    // At least one is undefined.
    Ok(Value::Undefined)
}

#[inline]
fn apply_arithmetic_nums(op: BinaryOp, ln: f64, rn: f64) -> JsonataResult {
    // Modulo by zero → D3001 immediately (matches Go).
    if op == BinaryOp::Mod && rn == 0.0 {
        return Err(JsonataError::new("D3001", "modulo by zero"));
    }
    let result = match op {
        BinaryOp::Add => ln + rn,
        BinaryOp::Sub => ln - rn,
        BinaryOp::Mul => ln * rn,
        BinaryOp::Div => ln / rn,
        BinaryOp::Mod => ln % rn,
        BinaryOp::Pow => ln.powf(rn),
        _ => unreachable!("caller matches arithmetic operators only"),
    };
    // Division by zero → let Inf propagate (error comes from downstream use).
    // Other non-finite results → D1001 "number out of range".
    if op == BinaryOp::Div {
        return Ok(Value::Number(result));
    }
    if !result.is_finite() {
        return Err(JsonataError::new(
            "D1001",
            format!(
                "Number out of range: {}",
                crate::value::format_float(result)
            ),
        ));
    }
    Ok(Value::Number(result))
}

/// Maximum number of elements the range operator (`..`) may produce.
///
/// Larger ranges error with `D2014`, matching the reference
/// implementation's 1e7 cap.
const MAX_RANGE_SIZE: i128 = 10_000_000;

/// Apply range operator to pre-evaluated values.
fn apply_range(left: &Value, right: &Value) -> JsonataResult {
    // Type-check non-undefined operands BEFORE undefined propagation.
    if !left.is_undefined() && left.as_f64().is_none() {
        return Err(JsonataError::new(
            "T2003",
            "the left operand of the range operator (..) must be a number",
        ));
    }
    if !right.is_undefined() && right.as_f64().is_none() {
        return Err(JsonataError::new(
            "T2004",
            "the right operand of the range operator (..) must be a number",
        ));
    }

    // Undefined operands → undefined (not an error).
    if left.is_undefined() || right.is_undefined() {
        return Ok(Value::Undefined);
    }

    let ln = left
        .as_f64()
        .ok_or_else(|| JsonataError::new("D0000", "left verified as number above"))?;
    let rn = right
        .as_f64()
        .ok_or_else(|| JsonataError::new("D0000", "right verified as number above"))?;

    // Must be finite integers (no fractional part, no Inf/NaN).
    if !ln.is_finite() || ln != ln.trunc() {
        return Err(JsonataError::new(
            "T2003",
            "the left operand of the range operator (..) must be an integer",
        ));
    }
    if !rn.is_finite() || rn != rn.trunc() {
        return Err(JsonataError::new(
            "T2004",
            "the right operand of the range operator (..) must be an integer",
        ));
    }

    let start = ln as i64;
    let end = rn as i64;
    if start > end {
        return Ok(Value::Undefined);
    }
    let count_wide = i128::from(end) - i128::from(start) + 1;
    if count_wide > MAX_RANGE_SIZE {
        return Err(JsonataError::new("D2014", "range operator too large"));
    }
    let mut arr = Vec::with_capacity(count_wide as usize);
    arr.extend((start..=end).map(|i| Value::Number(i as f64)));
    Ok(Value::Array(Rc::from(arr)))
}

/// Record the `[]` keep-array suffix on an evaluation result *lazily*, the
/// way the tail of jsonata-js `evaluate()` does (`result.keepSingleton =
/// true`): the value comes back as a flagged [`Value::Sequence`], and the
/// singleton is only promoted to an array where something collapses it.
/// Arrays pass through; `undefined` is what Undefined becomes — the suffix
/// yields Undefined on function results but an empty array in
/// subscript/group-value positions.
///
/// Deferring the wrap is what lets an enclosing path drop the flag while
/// re-sequencing, so `x.($keys(a)[])` answers `"k"` and not `["k"]`
/// (jsntrs-p0v.19).
pub(crate) fn flag_keep_array(result: Value, undefined: Value) -> Value {
    match result {
        Value::Undefined => undefined,
        other => super::mark_keep_singleton(other),
    }
}

/// [`flag_keep_array`] followed immediately by the collapse, for the one
/// position whose result is user-visible on the spot and can never hold a
/// sequence: a group-by pair's value, which is inserted straight into the
/// output object.
pub(crate) fn apply_keep_array(result: Value, undefined: Value) -> Value {
    super::collapse_sequence(flag_keep_array(result, undefined))
}

/// Walk the left chain of a Binary "[" node to check if keep_array is set
/// on the node itself or anywhere in the LHS chain.
/// Go equivalent: `hasKeepArrayInChain` in `eval_binary.go`.
pub(super) fn has_keep_array(arena: &AstArena, node: NodeId) -> bool {
    let mut current = node;
    loop {
        match arena.get(current) {
            Expr::Name { keep_array, .. }
            | Expr::Binary { keep_array, .. }
            | Expr::Variable { keep_array, .. }
            | Expr::Function { keep_array, .. }
            | Expr::Sort { keep_array, .. }
            | Expr::Unary { keep_array, .. }
                if *keep_array =>
            {
                return true;
            }
            _ => {}
        }
        // Walk into the LHS of Binary nodes or the expr of Sort nodes.
        match arena.get(current) {
            Expr::Binary { lhs, .. } => current = *lhs,
            Expr::Sort { expr, .. } => current = *expr,
            _ => break,
        }
    }
    false
}
