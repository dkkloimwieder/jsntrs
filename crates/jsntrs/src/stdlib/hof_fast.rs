//! Fast-path lambda dispatch for HOFs.
//!
//! Analyzes lambda AST at the HOF call site. When the body is a simple
//! expression (field access, comparison, arithmetic on fields), bypasses
//! full call_function dispatch and evaluates directly against the item.
//!
//! This eliminates per-call overhead: environment creation, parameter binding,
//! Value cloning for function args, and stack checks.

use std::rc::Rc;

use crate::error::JsonataResult;
use crate::evaluator::functions::FunctionValue;
use crate::evaluator::{Environment, call_function};
use crate::parser::ast::{AstArena, BinaryOp, Expr, NodeId};
use crate::value::{CompareOp, Value};

/// A simple lambda pattern that can be evaluated without full dispatch.
#[derive(Debug)]
pub enum SimpleLambda {
    /// function($v) { $v.field } — direct field access
    FieldAccess { field: String },
    /// function($v) { $v.field op literal } — field compared to constant
    FieldPredicate {
        field: String,
        op: BinaryOp,
        literal: Value,
    },
    /// function($v) { $v.field = $v.field2 } — two fields compared
    TwoFieldPredicate {
        field1: String,
        op: BinaryOp,
        field2: String,
    },
    /// function($a, $b) { $a.field op $b.field } — sort comparator (same field both sides)
    SortComparator { field: String, op: BinaryOp },
    /// function($prev, $curr) { $prev op $curr.field } — simple reduce accumulator
    ReduceAccum { field: String, op: BinaryOp },
    /// function($prev, $curr) { $prev op ($curr.field1 op2 $curr.field2) } — compound reduce
    ReduceCompoundAccum {
        field1: String,
        field2: String,
        outer_op: BinaryOp,
        inner_op: BinaryOp,
    },
    /// function($v) { $v.A & "lit" & $string($v.B) & ... } — concat template
    ConcatTemplate { pieces: Vec<TemplatePiece> },
    /// function($v) { $v.field1 op1 lit1 and/or $v.field2 op2 lit2 ... } — compound predicate
    CompoundPredicate {
        clauses: Vec<PredicateClause>,
        combiner: BinaryOp, // And or Or
    },
}

/// A single clause in a compound predicate: field op literal.
#[derive(Debug, Clone)]
pub struct PredicateClause {
    pub field: String,
    pub op: BinaryOp,
    pub literal: Value,
}

/// A piece of a concat template — evaluated into a string buffer.
#[derive(Debug, Clone)]
pub enum TemplatePiece {
    /// A string literal known at analysis time.
    Literal(String),
    /// A field access on the lambda parameter — appends the string value.
    Field(String),
    /// $string(field) — stringify the field value into the buffer.
    StringifyField(String),
    /// $substring(field, start, len?) — extract substring from field value.
    SubstringField {
        field: String,
        start: i64,
        length: Option<usize>,
    },
    /// $lowercase(field) — lowercase the field value.
    LowercaseField(String),
    /// $uppercase(field) — uppercase the field value.
    UppercaseField(String),
}

/// Try to analyze a lambda body into a SimpleLambda for fast dispatch.
pub fn analyze_lambda(params: &[String], body: NodeId, arena: &AstArena) -> Option<SimpleLambda> {
    let expr = arena.get(body);
    match expr {
        // Body is a path: $v.field
        Expr::Path { .. } => {
            let param = params.first()?;
            extract_param_field(body, arena, param).map(|field| SimpleLambda::FieldAccess { field })
        }
        // Body is a binary op
        Expr::Binary {
            op: BinaryOp::Concat,
            ..
        } if !params.is_empty() => {
            // Try concat template first, fall back to generic binary analysis
            analyze_concat_template(params, body, arena).or_else(|| {
                let Expr::Binary { op, lhs, rhs, .. } = arena.get(body) else {
                    return None;
                };
                analyze_binary(params, *op, *lhs, *rhs, arena)
            })
        }
        Expr::Binary { op, lhs, rhs, .. } => analyze_binary(params, *op, *lhs, *rhs, arena),
        _ => None,
    }
}

/// Check if a node is `$param` (variable reference matching a parameter name).
fn is_param_ref(node: NodeId, arena: &AstArena, param: &str) -> bool {
    matches!(arena.get(node), Expr::Variable { name, .. } if name == param)
}

/// Check if `node` is the path `$param.field` and return the field name.
///
/// Mirrors `fast_path::collect_pure_path`: a keep-array step (`$v.x[]`, and
/// `$v[].x`, whose flag `process_ast` propagates onto the path) forces the
/// path to preserve singletons as arrays. No lifted shape carries that
/// flag, so such a path is not liftable — decline instead of collapsing
/// (jsntrs-6wr.3).
fn extract_param_field(node: NodeId, arena: &AstArena, param: &str) -> Option<String> {
    let Expr::Path {
        steps,
        keep_singleton_array: false,
        ..
    } = arena.get(node)
    else {
        return None;
    };
    if steps.len() != 2 {
        return None;
    }
    if !is_param_ref(steps[0], arena, param) {
        return None;
    }
    match arena.get(steps[1]) {
        Expr::Name {
            value,
            stages,
            group,
            focus,
            index,
            keep_array: false,
            ..
        } if stages.is_empty() && group.is_none() && focus.is_none() && index.is_none() => {
            Some(value.clone())
        }
        _ => None,
    }
}

/// Extract a literal value from a node.
fn extract_literal(node: NodeId, arena: &AstArena) -> Option<Value> {
    match arena.get(node) {
        Expr::NumberLit { value, .. } => Some(Value::Number(*value)),
        Expr::StringLit { value, .. } => Some(Value::String(value.clone().into())),
        Expr::ValueLit { value, .. } => match value.as_str() {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            "null" => Some(Value::Null),
            _ => None,
        },
        _ => None,
    }
}

/// Analyze a binary expression in a lambda body.
fn analyze_binary(
    params: &[String],
    op: BinaryOp,
    lhs: NodeId,
    rhs: NodeId,
    arena: &AstArena,
) -> Option<SimpleLambda> {
    // Sort comparator: function($a, $b) { $a.field > $b.field }
    if params.len() == 2 && is_relational(op) {
        let param_a = &params[0];
        let param_b = &params[1];
        if let (Some(field_a), Some(field_b)) = (
            extract_param_field(lhs, arena, param_a),
            extract_param_field(rhs, arena, param_b),
        ) {
            // Different fields on each side ($a.x > $b.y) cannot be lifted:
            // the fast comparator reads ONE field from both items, which
            // would silently diverge from the general path.
            if field_a == field_b {
                return Some(SimpleLambda::SortComparator { field: field_a, op });
            }
        }
    }

    // Reduce accumulator: function($prev, $curr) { $prev + $curr.field }
    // or compound: function($prev, $curr) { $prev + $curr.field1 * $curr.field2 }
    if params.len() >= 2 && is_arithmetic(op) {
        let param_prev = &params[0];
        let param_curr = &params[1];
        if is_param_ref(lhs, arena, param_prev) {
            // Simple: $prev op $curr.field
            if let Some(field) = extract_param_field(rhs, arena, param_curr) {
                return Some(SimpleLambda::ReduceAccum { field, op });
            }
            // Compound: $prev op ($curr.field1 inner_op $curr.field2)
            if let Expr::Binary {
                op: inner_op,
                lhs: inner_lhs,
                rhs: inner_rhs,
                ..
            } = arena.get(rhs)
            {
                if is_arithmetic(*inner_op) {
                    if let (Some(field1), Some(field2)) = (
                        extract_param_field(*inner_lhs, arena, param_curr),
                        extract_param_field(*inner_rhs, arena, param_curr),
                    ) {
                        return Some(SimpleLambda::ReduceCompoundAccum {
                            field1,
                            field2,
                            outer_op: op,
                            inner_op: *inner_op,
                        });
                    }
                }
            }
        }
    }

    // Compound predicate: function($v) { $v.field1 op1 lit1 and/or $v.field2 op2 lit2 }
    if !params.is_empty() && (op == BinaryOp::And || op == BinaryOp::Or) {
        let param = &params[0];
        let mut clauses = Vec::new();
        if collect_predicate_clauses(arena, lhs, param, op, &mut clauses)
            && collect_predicate_clauses(arena, rhs, param, op, &mut clauses)
            && clauses.len() >= 2
        {
            return Some(SimpleLambda::CompoundPredicate {
                clauses,
                combiner: op,
            });
        }
    }

    // Field predicate: function($v) { $v.field op literal }
    // Only ops eval_binary_simple implements may be lifted; anything else
    // (in, &, **, ??, ...) must fall through to the general evaluator.
    if !params.is_empty() && (is_relational(op) || is_arithmetic(op)) {
        let param = &params[0];

        // $v.field op literal
        if let Some(field) = extract_param_field(lhs, arena, param) {
            if let Some(lit) = extract_literal(rhs, arena) {
                return Some(SimpleLambda::FieldPredicate {
                    field,
                    op,
                    literal: lit,
                });
            }
            // $v.field1 op $v.field2
            if let Some(field2) = extract_param_field(rhs, arena, param) {
                return Some(SimpleLambda::TwoFieldPredicate {
                    field1: field,
                    op,
                    field2,
                });
            }
        }

        // literal op $v.field (reversed) — relational only: the lift stores
        // the field on the left, and flip_relational has no exact mirror for
        // non-commutative arithmetic (lit - field ≠ field - lit).
        if is_relational(op)
            && let Some(lit) = extract_literal(lhs, arena)
            && is_mirrorable_literal(op, &lit)
            && let Some(field) = extract_param_field(rhs, arena, param)
        {
            return Some(SimpleLambda::FieldPredicate {
                field,
                op: flip_relational(op),
                literal: lit,
            });
        }
    }

    None
}

/// Analyze a concat chain in a lambda body into a ConcatTemplate.
/// Flattens left-recursive Concat(Concat(a, b), c) → [a, b, c] and
/// classifies each operand as Literal, Field, or StringifyField.
fn analyze_concat_template(
    params: &[String],
    body: NodeId,
    arena: &AstArena,
) -> Option<SimpleLambda> {
    let param = &params[0];
    let mut operand_nodes = Vec::new();
    collect_concat_nodes(arena, body, &mut operand_nodes);

    if operand_nodes.len() < 2 {
        return None;
    }

    let mut pieces = Vec::with_capacity(operand_nodes.len());
    for &node in &operand_nodes {
        if let Some(piece) = classify_template_operand(node, arena, param) {
            pieces.push(piece);
        } else {
            return None; // unsupported operand, bail
        }
    }

    Some(SimpleLambda::ConcatTemplate { pieces })
}

/// Walk a left-recursive Concat tree and collect leaf nodes.
fn collect_concat_nodes(arena: &AstArena, node: NodeId, out: &mut Vec<NodeId>) {
    if let Expr::Binary {
        op: BinaryOp::Concat,
        lhs,
        rhs,
        ..
    } = arena.get(node)
    {
        collect_concat_nodes(arena, *lhs, out);
        out.push(*rhs);
    } else {
        out.push(node);
    }
}

/// Classify a single concat operand into a TemplatePiece.
fn classify_template_operand(node: NodeId, arena: &AstArena, param: &str) -> Option<TemplatePiece> {
    match arena.get(node) {
        // String literal
        Expr::StringLit { value, .. } => Some(TemplatePiece::Literal(value.clone())),

        // $param.field — direct field access (value should be a string)
        Expr::Path { .. } => extract_param_field(node, arena, param).map(TemplatePiece::Field),

        // Function calls on fields: $string, $substring, $lowercase, $uppercase
        Expr::Function {
            procedure,
            arguments,
            ..
        } => {
            let func_name = match arena.get(*procedure) {
                Expr::Variable { name, .. } => name.as_str(),
                _ => return None,
            };
            match func_name {
                // $string($param.field)
                "string" if arguments.len() == 1 => extract_param_field(arguments[0], arena, param)
                    .map(TemplatePiece::StringifyField),
                // $lowercase($param.field)
                "lowercase" if arguments.len() == 1 => {
                    extract_param_field(arguments[0], arena, param)
                        .map(TemplatePiece::LowercaseField)
                }
                // $uppercase($param.field)
                "uppercase" if arguments.len() == 1 => {
                    extract_param_field(arguments[0], arena, param)
                        .map(TemplatePiece::UppercaseField)
                }
                // $substring($param.field, start [, length])
                "substring" if arguments.len() >= 2 && arguments.len() <= 3 => {
                    let field = extract_param_field(arguments[0], arena, param)?;
                    let start = match arena.get(arguments[1]) {
                        Expr::NumberLit { value, .. } => *value as i64,
                        _ => return None,
                    };
                    let length = if arguments.len() == 3 {
                        match arena.get(arguments[2]) {
                            Expr::NumberLit { value, .. } if *value >= 0.0 => Some(*value as usize),
                            _ => return None,
                        }
                    } else {
                        None
                    };
                    Some(TemplatePiece::SubstringField {
                        field,
                        start,
                        length,
                    })
                }
                _ => None,
            }
        }

        _ => None,
    }
}

/// Recursively collect field-op-literal clauses from an and/or tree.
/// Returns false if any leaf is not a simple field predicate.
fn collect_predicate_clauses(
    arena: &AstArena,
    node: NodeId,
    param: &str,
    combiner: BinaryOp,
    out: &mut Vec<PredicateClause>,
) -> bool {
    match arena.get(node) {
        // Same combiner: flatten nested and/or
        Expr::Binary { op, lhs, rhs, .. } if *op == combiner => {
            collect_predicate_clauses(arena, *lhs, param, combiner, out)
                && collect_predicate_clauses(arena, *rhs, param, combiner, out)
        }
        // Leaf: must be $param.field op literal (or literal op $param.field)
        Expr::Binary { op, lhs, rhs, .. } if is_relational(*op) || is_arithmetic(*op) => {
            if let Some(field) = extract_param_field(*lhs, arena, param) {
                if let Some(lit) = extract_literal(*rhs, arena) {
                    out.push(PredicateClause {
                        field,
                        op: *op,
                        literal: lit,
                    });
                    return true;
                }
            }
            // Reversed: literal op $param.field — relational only, since the
            // clause stores the field on the left and flip_relational cannot
            // mirror non-commutative arithmetic.
            if is_relational(*op)
                && let Some(lit) = extract_literal(*lhs, arena)
                && is_mirrorable_literal(*op, &lit)
                && let Some(field) = extract_param_field(*rhs, arena, param)
            {
                out.push(PredicateClause {
                    field,
                    op: flip_relational(*op),
                    literal: lit,
                });
                return true;
            }
            false
        }
        _ => false,
    }
}

fn is_relational(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Gt | BinaryOp::Lt | BinaryOp::Ge | BinaryOp::Le | BinaryOp::Eq | BinaryOp::Ne
    )
}

fn is_arithmetic(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod
    )
}

/// May a left-hand literal be moved to the right of the field, with the
/// operator flipped by `flip_relational`?
///
/// `Eq`/`Ne` stay put and `eval_binary_simple` treats them symmetrically
/// (undefined on either side → false, otherwise `deep_equal`), so any
/// literal mirrors. The ordering operators go through `Value::compare`,
/// which validates the LEFT operand's type *before* undefined propagation:
/// with `null` or a boolean on the left the general path raises T2010 even
/// when the field is undefined, while the swapped form would propagate
/// undefined and silently drop the item. Numbers and strings always pass
/// that left-operand check, so swapping them observes nothing
/// (jsntrs-6wr.6).
fn is_mirrorable_literal(op: BinaryOp, lit: &Value) -> bool {
    matches!(op, BinaryOp::Eq | BinaryOp::Ne) || matches!(lit, Value::Number(_) | Value::String(_))
}

fn flip_relational(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::Gt => BinaryOp::Lt,
        BinaryOp::Lt => BinaryOp::Gt,
        BinaryOp::Ge => BinaryOp::Le,
        BinaryOp::Le => BinaryOp::Ge,
        other => other, // Eq, Ne are symmetric
    }
}

// ── Fast-path evaluation helpers ────────────────────────────────────────────

/// Get a field value from an item, auto-mapping over array items exactly
/// like the general path's `eval_name` (CLAUDE.md invariant 3) — a lambda
/// item can itself be an array, e.g. `$map([[{"x":1},{"x":2}]], fn)`.
#[inline]
pub fn get_field(item: &Value, field: &str) -> Value {
    crate::fast_path::path_step(field, item)
}

/// Evaluate a binary op on two Values (for predicates and comparators).
///
/// Delegates to the same primitives the general evaluator uses
/// (`Value::compare`, `apply_binary_op`'s equality rules, and
/// `apply_arithmetic`) so fast-path results — including error codes —
/// cannot diverge from full evaluation.
///
/// # Errors
/// Exactly the errors the general evaluator raises for the same
/// operands: T2009/T2010 for invalid comparisons, T2001/T2002 for
/// non-numeric arithmetic operands, D3001 for modulo by zero, D1001
/// for out-of-range results.
///
/// # Panics
/// If called with an op outside the relational/arithmetic set — the
/// analysis in this module must never lift such an op.
#[inline]
pub fn eval_binary_simple(lhs: &Value, op: BinaryOp, rhs: &Value) -> JsonataResult {
    match op {
        BinaryOp::Gt => lhs.compare(rhs, CompareOp::Gt),
        BinaryOp::Lt => lhs.compare(rhs, CompareOp::Lt),
        BinaryOp::Ge => lhs.compare(rhs, CompareOp::Ge),
        BinaryOp::Le => lhs.compare(rhs, CompareOp::Le),
        // Equality mirrors apply_binary_op: undefined operands → false.
        BinaryOp::Eq => {
            if lhs.is_undefined() || rhs.is_undefined() {
                Ok(Value::Bool(false))
            } else {
                Ok(Value::Bool(lhs.deep_equal(rhs)))
            }
        }
        BinaryOp::Ne => {
            if lhs.is_undefined() || rhs.is_undefined() {
                Ok(Value::Bool(false))
            } else {
                Ok(Value::Bool(!lhs.deep_equal(rhs)))
            }
        }
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
            crate::evaluator::apply_arithmetic(op, lhs, rhs)
        }
        // The analysis in this module only lifts relational/arithmetic ops;
        // reaching here with anything else is a bug in the lift gating, not
        // an evaluatable expression — returning a value would silently
        // diverge from the general path.
        _ => unreachable!("eval_binary_simple called with unlifted op {op:?}"),
    }
}

/// Compare two items by a field for `^()` sorting, surfacing JSONata sort type errors:
/// T2007 for string/number key mismatches, T2008 for non-sortable keys
/// (null, boolean, array, object). Missing (undefined) keys sort last
/// without error. Used by the `^()` operator fast path, which must match
/// the general path's error behavior (`Value::compare_order`).
///
/// # Errors
/// Returns T2007 or T2008 as described above.
pub fn compare_by_field_checked(
    a: &Value,
    b: &Value,
    field: &str,
) -> JsonataResult<std::cmp::Ordering> {
    let va = get_field(a, field);
    let vb = get_field(b, field);
    Ok(va.compare_order(&vb)?.cmp(&0))
}

/// Evaluate a ConcatTemplate against an item, writing into a single buffer.
///
/// Function pieces delegate to the real builtins (`fn_substring`,
/// `fn_lowercase`, `fn_uppercase`) so type errors and edge cases match
/// the general path exactly; concat semantics append nothing for
/// undefined piece values, mirroring the `&` operator.
///
/// # Errors
/// Whatever the underlying builtin or stringification raises for the
/// same inputs on the general path (e.g. T0410 for `$substring` on a
/// non-string field).
pub fn eval_concat_template(item: &Value, pieces: &[TemplatePiece]) -> JsonataResult {
    let mut buf = String::new();
    for piece in pieces {
        match piece {
            TemplatePiece::Literal(s) => buf.push_str(s),
            TemplatePiece::Field(field) => {
                let v = get_field(item, field);
                if let Value::String(s) = &v {
                    buf.push_str(s);
                } else if !v.is_undefined() {
                    // Non-string field in concat — stringify it
                    v.stringify_into(&mut buf)?;
                }
            }
            TemplatePiece::StringifyField(field) => {
                let v = get_field(item, field);
                if !v.is_undefined() {
                    v.stringify_into(&mut buf)?;
                }
            }
            TemplatePiece::SubstringField {
                field,
                start,
                length,
            } => {
                let mut args = vec![get_field(item, field), Value::Number(*start as f64)];
                if let Some(l) = length {
                    args.push(Value::Number(*l as f64));
                }
                push_piece(
                    &mut buf,
                    super::string_funcs::fn_substring(&args, &Value::Undefined)?,
                );
            }
            TemplatePiece::LowercaseField(field) => {
                let args = [get_field(item, field)];
                push_piece(
                    &mut buf,
                    super::string_funcs::fn_lowercase(&args, &Value::Undefined)?,
                );
            }
            TemplatePiece::UppercaseField(field) => {
                let args = [get_field(item, field)];
                push_piece(
                    &mut buf,
                    super::string_funcs::fn_uppercase(&args, &Value::Undefined)?,
                );
            }
        }
    }
    Ok(Value::String(buf.into()))
}

/// Append a builtin's result to the concat buffer (`&` semantics:
/// undefined contributes nothing).
fn push_piece(buf: &mut String, v: Value) {
    if let Value::String(s) = &v {
        buf.push_str(s);
    }
}

// ── Lifted dispatch for mapped expressions ──────────────────────────────────

/// Pre-computed function-specific state for lifted dispatch.
/// Each variant captures what a specific function needs to skip per-call setup.
#[allow(clippy::large_enum_variant)] // allow, not expect: fires on 64-bit targets only, so expect would be unfulfilled on wasm32
pub(crate) enum PreparedState {
    /// $formatNumber: pre-parsed picture into SubPicture + FmtChars
    FormatNumber {
        pos_pic: super::format_number::SubPicture,
        neg_pic: super::format_number::SubPicture,
        fc: super::format_number::FmtChars,
    },
    /// $round: pre-extracted precision
    Round { precision: i64 },
    /// $contains with string arg: pre-extracted needle
    Contains { needle: String },
    /// $formatBase: pre-extracted radix
    FormatBase { radix: u32 },
}

impl std::fmt::Debug for PreparedState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FormatNumber { .. } => write!(f, "FormatNumber(...)"),
            Self::Round { precision } => write!(f, "Round({precision})"),
            Self::Contains { needle } => write!(f, "Contains({needle:?})"),
            Self::FormatBase { radix } => write!(f, "FormatBase({radix})"),
        }
    }
}

/// A pre-analyzed function call that can be dispatched efficiently per item.
/// Function resolution and constant arg evaluation happen once at analysis time.
#[derive(Debug)]
pub(crate) struct MappedCall {
    func: Box<FunctionValue>,
    arg_template: Vec<CallArg>,
    prepared: Option<PreparedState>,
}

/// Classification of a function argument for lifted dispatch.
#[derive(Debug, Clone)]
pub enum CallArg {
    /// $param.field — resolved per item via get_field
    Field(String),
    /// A constant value — evaluated once at analysis time
    Const(Value),
    /// A complex expression that can't be lifted — falls back to per-item eval
    Expr(NodeId),
}

/// Analyze a function call node in a mapped context.
/// `param` is the mapping variable name (e.g., the implicit scope in `.()` or the lambda param).
/// `env` must be the scope the callee name resolves in on the general path:
/// the lambda's closure for HOF lifts, the step's own env for `.()` steps.
///
/// Returns Some(MappedCall) if the call can be lifted, None otherwise.
pub(crate) fn analyze_mapped_call(
    node: NodeId,
    arena: &AstArena,
    param: Option<&str>,
    env: &Rc<Environment>,
) -> Option<MappedCall> {
    if crate::fast_path::testing::fast_paths_disabled() {
        return None;
    }
    // The node should be a Function call, possibly wrapped in a Block.
    let func_node = unwrap_block(node, arena);

    let (procedure, arguments) = match arena.get(func_node) {
        Expr::Function {
            procedure,
            arguments,
            ..
        } => (*procedure, arguments.clone()),
        _ => return None,
    };

    // Resolve the function from the environment.
    // procedure is typically a Variable node ($formatNumber → Variable { name: "formatNumber" })
    let func_name = match arena.get(procedure) {
        Expr::Variable { name, .. } => name.as_str(),
        _ => return None,
    };
    let func_val = env.lookup(func_name)?;
    let Value::Function(func) = func_val else {
        return None;
    };

    // An under-supplied lambda call means something different on each of the
    // two general paths this lift stands in for: as a path step
    // (`eval_path_function_step`) the context item is prepended as the first
    // argument, while inside a lambda body (`eval_function` → `call_function`)
    // the missing parameters are padded with undefined. The lifted arg
    // template carries no context item and cannot tell the two apart, so
    // decline the lift and let each caller's general path apply its own rule
    // (jsntrs-6wr.5).
    if let FunctionValue::Lambda(ref lambda) = *func
        && lambda.params.len() > arguments.len()
    {
        return None;
    }

    // Classify each argument.
    let mut arg_template = Vec::with_capacity(arguments.len());
    for &arg_node in &arguments {
        arg_template.push(classify_call_arg(arg_node, arena, param));
    }

    // Only worth lifting if at least one arg is a Field (otherwise nothing varies per item).
    let has_field = arg_template.iter().any(|a| matches!(a, CallArg::Field(_)));
    if !has_field {
        return None;
    }

    // Bail if any arg is a complex Expr — we can't fully lift.
    // (Could still partially lift, but keep it simple for now.)
    let has_complex = arg_template.iter().any(|a| matches!(a, CallArg::Expr(_)));
    if has_complex {
        return None;
    }

    // Try to pre-compute function-specific state from constant args.
    // Prepared state re-implements stdlib semantics keyed by name, so it
    // must only engage when the name resolved to a builtin — a user lambda
    // shadowing $round must dispatch through call_function below.
    // Prepared state replicates stdlib semantics, so it may only serve the
    // stdlib function itself: gate on Rc identity with the canonical
    // registration. A custom function or lambda bound over the same name
    // (its Rc can never be the canonical one) takes the generic call path.
    let prepared = match &*func {
        FunctionValue::Builtin(rc)
            if crate::stdlib::canonical_prepared(func_name)
                .is_some_and(|canon| Rc::ptr_eq(rc, &canon)) =>
        {
            try_prepare(func_name, &arg_template)
        }
        _ => None,
    };

    Some(MappedCall {
        func,
        arg_template,
        prepared,
    })
}

/// Unwrap a single-expression Block to get the inner expression.
fn unwrap_block(node: NodeId, arena: &AstArena) -> NodeId {
    if let Expr::Block { expressions, .. } = arena.get(node)
        && expressions.len() == 1
    {
        return expressions[0];
    }
    node
}

/// Classify a function argument as Field, Const, or Expr.
fn classify_call_arg(node: NodeId, arena: &AstArena, param: Option<&str>) -> CallArg {
    match arena.get(node) {
        // String literal
        Expr::StringLit { value, .. } => CallArg::Const(Value::String(value.clone().into())),

        // Number literal
        Expr::NumberLit { value, .. } => CallArg::Const(Value::Number(*value)),

        // Boolean/null literal
        Expr::ValueLit { value, .. } => match value.as_str() {
            "true" => CallArg::Const(Value::Bool(true)),
            "false" => CallArg::Const(Value::Bool(false)),
            "null" => CallArg::Const(Value::Null),
            _ => CallArg::Expr(node),
        },

        // $param.field or just FieldName (implicit scope)
        Expr::Path { .. } => {
            if let Some(p) = param
                && let Some(field) = extract_param_field(node, arena, p)
            {
                return CallArg::Field(field);
            }
            CallArg::Expr(node)
        }

        // Bare field name (in .() mapping context, no explicit param).
        // `keep_array` (`x[]`) wraps singletons, which CallArg::Field
        // cannot express — defer to the general path (jsntrs-6wr.3).
        Expr::Name {
            value,
            stages,
            group,
            focus,
            index,
            keep_array: false,
            ..
        } if stages.is_empty()
            && group.is_none()
            && focus.is_none()
            && index.is_none()
            && param.is_none() =>
        {
            CallArg::Field(value.clone())
        }

        // Bare $param reference (the whole object)
        Expr::Variable { name, .. } if param.is_some_and(|p| name == p) => CallArg::Expr(node),

        _ => CallArg::Expr(node),
    }
}

/// Try to pre-compute function-specific state from the argument template.
fn try_prepare(func_name: &str, args: &[CallArg]) -> Option<PreparedState> {
    match func_name {
        "formatNumber" => {
            // args: [Field(number), Const(picture), optional opts]
            // An options argument changes FmtChars — defer to the general
            // path rather than formatting with defaults and wrong output.
            if args.len() > 2 {
                return None;
            }
            let picture = match args.get(1) {
                Some(CallArg::Const(Value::String(s))) => s.to_string(),
                _ => return None,
            };
            let fc = super::format_number::FmtChars::default();
            let pics = super::format_number::split_on_pattern_sep(&picture, fc.pattern_sep);
            if pics.len() > 2 {
                return None;
            }
            let pos_pic = super::format_number::parse_sub_picture(&pics[0], &fc).ok()?;
            let neg_pic = if pics.len() == 2 {
                super::format_number::parse_sub_picture(&pics[1], &fc).ok()?
            } else {
                let mut np = pos_pic.clone();
                np.prefix = format!("-{}", pos_pic.prefix);
                np
            };
            Some(PreparedState::FormatNumber {
                pos_pic,
                neg_pic,
                fc,
            })
        }
        "round" => {
            let precision = match args.get(1) {
                Some(CallArg::Const(Value::Number(n))) => *n as i64,
                None => 0,
                _ => return None,
            };
            Some(PreparedState::Round { precision })
        }
        "contains" => {
            let needle = match args.get(1) {
                Some(CallArg::Const(Value::String(s))) => s.to_string(),
                _ => return None,
            };
            Some(PreparedState::Contains { needle })
        }
        "formatBase" => {
            let radix = match args.get(1) {
                Some(CallArg::Const(Value::Number(n))) => *n as u32,
                _ => return None,
            };
            if !(2..=36).contains(&radix) {
                return None;
            }
            Some(PreparedState::FormatBase { radix })
        }
        _ => None,
    }
}

/// Execute a prepared function call directly, skipping internal parsing.
fn exec_prepared(prepared: &PreparedState, field_val: &Value) -> Option<JsonataResult> {
    match prepared {
        PreparedState::FormatNumber {
            pos_pic,
            neg_pic,
            fc,
        } => {
            let n = match field_val {
                Value::Number(f) => *f,
                _ => return None,
            };
            let negative = n < 0.0;
            let sp = if negative { neg_pic } else { pos_pic };
            let mut value = if negative { -n } else { n };
            match sp.scale {
                1 => value *= 100.0,
                2 => value *= 1000.0,
                _ => {}
            }
            let inner = if sp.exp_mandatory > 0 {
                super::format_number::format_with_exponent(value, sp, fc)
            } else {
                super::format_number::format_fixed(value, sp, fc)
            };
            let inner = super::format_number::apply_digit_family(&inner, fc.zero_digit);
            Some(Ok(Value::String(
                format!("{}{}{}", sp.prefix, inner, sp.suffix).into(),
            )))
        }
        PreparedState::Round { precision } => {
            let n = match field_val {
                Value::Number(f) => *f,
                _ => return None,
            };
            // Must match $round exactly: half-to-even, same as numeric::fn_round.
            Some(Ok(Value::Number(super::numeric::bankers_round(
                n,
                *precision as i32,
            ))))
        }
        PreparedState::Contains { needle } => {
            let Value::String(s) = field_val else {
                return None;
            };
            Some(Ok(Value::Bool(s.contains(needle.as_str()))))
        }
        // Delegate to the real builtin: it owns rounding (2.5 → 3) and
        // negative-number formatting, so the fast path cannot diverge.
        PreparedState::FormatBase { radix } => match field_val {
            Value::Number(_) => Some(super::numeric::fn_format_base(
                &[field_val.clone(), Value::Number(f64::from(*radix))],
                &Value::Undefined,
            )),
            _ => None,
        },
    }
}

/// Execute a MappedCall for a single item. Function is already resolved,
/// constant args are already evaluated. Only field args need per-item work.
///
/// # Errors
/// Returns evaluation errors from the underlying function call.
pub(crate) fn exec_mapped_call(
    mc: &MappedCall,
    item: &Value,
    env: &Rc<Environment>,
    arena: &AstArena,
) -> JsonataResult {
    // If we have prepared state and the first arg is a field, try the fast path.
    if let Some(ref prepared) = mc.prepared {
        // Get the field value (first Field arg).
        if let Some(CallArg::Field(name)) = mc.arg_template.first() {
            let field_val = get_field(item, name);
            if let Some(result) = exec_prepared(prepared, &field_val) {
                return result;
            }
        }
    }

    // Fallback: generic dispatch with pre-resolved function.
    let mut args: Vec<Value> = Vec::with_capacity(mc.arg_template.len());
    for arg in &mc.arg_template {
        match arg {
            CallArg::Field(name) => args.push(get_field(item, name)),
            CallArg::Const(val) => args.push(val.clone()),
            CallArg::Expr(_node) => unreachable!("complex args filtered out in analysis"),
        }
    }
    call_function(&mc.func, &args, item, env, arena)
}

#[cfg(test)]
mod tests {
    use super::{PredicateClause, SimpleLambda, TemplatePiece, analyze_lambda};
    use crate::evaluator::functions::FunctionValue;
    use crate::evaluator::{Environment, eval};
    use crate::parser::ast::BinaryOp;
    use crate::parser::{Parser, process_ast};
    use crate::value::Value;
    use std::rc::Rc;

    /// Helper: parse a lambda literal and run the fast-path analyzer on it.
    fn analyze_src(src: &str) -> Option<SimpleLambda> {
        let (mut arena, root) = Parser::parse(src).expect("parse failed");
        let root = process_ast(&mut arena, root).expect("process failed");
        let mut env = Environment::new();
        crate::stdlib::register_all(&mut env);
        let env = Rc::new(env);
        match eval(&arena, root, &Value::Undefined, &env).expect("eval failed") {
            Value::Function(f) => match *f {
                FunctionValue::Lambda(lam) => analyze_lambda(&lam.params, lam.body, &arena),
                other => panic!("expected lambda, got {other:?}"),
            },
            other => panic!("expected function, got {other:?}"),
        }
    }

    // ── analyze_lambda recognition table ────────────────────────────────

    #[test]
    fn analyzer_recognizes_field_access() {
        assert!(matches!(
            analyze_src("function($v){$v.price}"),
            Some(SimpleLambda::FieldAccess { ref field }) if field == "price"
        ));
    }

    #[test]
    fn analyzer_recognizes_field_predicates_and_normalizes_reversed_literals() {
        assert!(matches!(
            analyze_src("function($v){$v.qty >= 10}"),
            Some(SimpleLambda::FieldPredicate {
                ref field,
                op: BinaryOp::Ge,
                literal: Value::Number(n),
            }) if field == "qty" && n == 10.0
        ));
        // Literal on the left flips the operator: 10 < $v.qty ≡ $v.qty > 10.
        assert!(matches!(
            analyze_src("function($v){10 < $v.qty}"),
            Some(SimpleLambda::FieldPredicate {
                ref field,
                op: BinaryOp::Gt,
                literal: Value::Number(n),
            }) if field == "qty" && n == 10.0
        ));
    }

    #[test]
    fn analyzer_recognizes_two_field_predicate() {
        assert!(matches!(
            analyze_src("function($v){$v.a = $v.b}"),
            Some(SimpleLambda::TwoFieldPredicate {
                ref field1,
                op: BinaryOp::Eq,
                ref field2,
            }) if field1 == "a" && field2 == "b"
        ));
    }

    #[test]
    fn analyzer_recognizes_same_field_sort_comparator() {
        assert!(matches!(
            analyze_src("function($a,$b){$a.price > $b.price}"),
            Some(SimpleLambda::SortComparator { ref field, op: BinaryOp::Gt })
                if field == "price"
        ));
    }

    /// A comparator reading DIFFERENT fields from each item must not be
    /// lifted: the fast sort comparator reads a single field from both
    /// sides, which would silently diverge from the general path.
    #[test]
    fn analyzer_rejects_cross_field_sort_comparator() {
        assert!(analyze_src("function($a,$b){$a.x > $b.y}").is_none());
    }

    #[test]
    fn analyzer_recognizes_reduce_accumulators() {
        assert!(matches!(
            analyze_src("function($p,$c){$p + $c.amount}"),
            Some(SimpleLambda::ReduceAccum { ref field, op: BinaryOp::Add })
                if field == "amount"
        ));
        assert!(matches!(
            analyze_src("function($p,$c){$p + $c.price * $c.qty}"),
            Some(SimpleLambda::ReduceCompoundAccum {
                ref field1,
                ref field2,
                outer_op: BinaryOp::Add,
                inner_op: BinaryOp::Mul,
            }) if field1 == "price" && field2 == "qty"
        ));
    }

    #[test]
    fn analyzer_recognizes_concat_template_pieces() {
        let Some(SimpleLambda::ConcatTemplate { pieces }) =
            analyze_src(r#"function($v){$v.first & " " & $string($v.n)}"#)
        else {
            panic!("expected ConcatTemplate");
        };
        assert_eq!(pieces.len(), 3);
        assert!(matches!(&pieces[0], TemplatePiece::Field(f) if f == "first"));
        assert!(matches!(&pieces[1], TemplatePiece::Literal(l) if l == " "));
        assert!(matches!(&pieces[2], TemplatePiece::StringifyField(f) if f == "n"));
    }

    #[test]
    fn analyzer_recognizes_compound_predicate() {
        let Some(SimpleLambda::CompoundPredicate { clauses, combiner }) =
            analyze_src("function($v){$v.a > 1 and $v.b < 5}")
        else {
            panic!("expected CompoundPredicate");
        };
        assert_eq!(combiner, BinaryOp::And);
        assert_eq!(clauses.len(), 2);
        assert!(matches!(
            &clauses[1],
            PredicateClause { field, op: BinaryOp::Lt, .. } if field == "b"
        ));
    }

    #[test]
    fn analyzer_rejects_unsupported_bodies() {
        // Deep path — only single-step field access is lifted.
        assert!(analyze_src("function($v){$v.a.b}").is_none());
        // Variable other than the parameter.
        assert!(analyze_src("function($v){$x.a}").is_none());
        // Function call bodies are not lifted.
        assert!(analyze_src("function($v){$sum($v.a)}").is_none());
        // No parameters to bind.
        assert!(analyze_src("function(){1}").is_none());
        // Mixed and/or combiners cannot form a compound predicate.
        assert!(analyze_src("function($v){$v.a > 1 and $v.b < 5 or $v.c = 2}").is_none());
    }

    /// A `null` or boolean literal on the LEFT of an ordering operator is
    /// not mirrored by swap-and-flip: `Value::compare` type-checks the left
    /// operand before undefined propagation, so the general path raises
    /// T2010 on a missing field where the swapped form returns undefined
    /// (jsntrs-6wr.6).
    #[test]
    fn analyzer_rejects_reversed_null_and_boolean_literals() {
        assert!(analyze_src("function($v){null > $v.x}").is_none());
        assert!(analyze_src("function($v){null < $v.x}").is_none());
        assert!(analyze_src("function($v){null >= $v.x}").is_none());
        assert!(analyze_src("function($v){false > $v.x}").is_none());
        assert!(analyze_src("function($v){true <= $v.x}").is_none());
        // Compound predicates reverse their clauses the same way.
        assert!(analyze_src("function($v){null > $v.x and $v.y = 1}").is_none());
        assert!(analyze_src("function($v){$v.y = 1 or true < $v.x}").is_none());
    }

    /// The jsntrs-6wr.6 guard must not over-bail: equality is symmetric, and
    /// numbers and strings pass `compare`'s left-operand check either way.
    #[test]
    fn analyzer_still_flips_mirrorable_literals() {
        assert!(matches!(
            analyze_src("function($v){null = $v.x}"),
            Some(SimpleLambda::FieldPredicate {
                op: BinaryOp::Eq,
                literal: Value::Null,
                ..
            })
        ));
        assert!(matches!(
            analyze_src("function($v){true != $v.x}"),
            Some(SimpleLambda::FieldPredicate {
                op: BinaryOp::Ne,
                literal: Value::Bool(true),
                ..
            })
        ));
        assert!(matches!(
            analyze_src("function($v){2 > $v.x}"),
            Some(SimpleLambda::FieldPredicate {
                op: BinaryOp::Lt,
                literal: Value::Number(n),
                ..
            }) if n == 2.0
        ));
        assert!(matches!(
            analyze_src("function($v){\"s\" <= $v.x}"),
            Some(SimpleLambda::FieldPredicate {
                op: BinaryOp::Ge,
                ..
            })
        ));
        assert!(matches!(
            analyze_src("function($v){2 > $v.x and $v.y = 1}"),
            Some(SimpleLambda::CompoundPredicate { .. })
        ));
    }

    /// `[]` on any step of a lambda-body path makes the path keep singletons
    /// as arrays; no lifted shape carries that flag, so every shape that
    /// reads a field must decline (jsntrs-6wr.3).
    #[test]
    fn analyzer_rejects_keep_array_paths() {
        // Keep-array on the field step, and on the parameter step (which
        // process_ast propagates onto the enclosing path).
        assert!(analyze_src("function($v){$v.a[]}").is_none());
        assert!(analyze_src("function($v){$v[].a}").is_none());
        // FieldPredicate, both operand orders.
        assert!(analyze_src("function($v){$v.a[] > 1}").is_none());
        assert!(analyze_src("function($v){1 < $v.a[]}").is_none());
        // TwoFieldPredicate — either side is enough to disqualify.
        assert!(analyze_src("function($v){$v.a[] = $v.b}").is_none());
        assert!(analyze_src("function($v){$v.a = $v.b[]}").is_none());
        // SortComparator, ReduceAccum, ReduceCompoundAccum.
        assert!(analyze_src("function($a,$b){$a.x[] > $b.x[]}").is_none());
        assert!(analyze_src("function($p,$c){$p + $c.x[]}").is_none());
        assert!(analyze_src("function($p,$c){$p + $c.x[] * $c.y}").is_none());
        // CompoundPredicate clauses.
        assert!(analyze_src("function($v){$v.a[] > 1 and $v.b < 5}").is_none());
        assert!(analyze_src("function($v){$v.a > 1 or $v.b[] < 5}").is_none());
        // ConcatTemplate pieces: bare field and stringifying wrappers.
        assert!(analyze_src("function($v){\"id-\" & $v.a[]}").is_none());
        assert!(analyze_src("function($v){\"id-\" & $string($v.a[])}").is_none());
        assert!(analyze_src("function($v){\"id-\" & $uppercase($v.a[])}").is_none());
        assert!(analyze_src("function($v){\"id-\" & $substring($v.a[], 1)}").is_none());
    }

    /// The keep-array guard must not over-bail: the same shapes without `[]`
    /// still lift, so the fast paths stay engaged.
    #[test]
    fn analyzer_still_lifts_without_keep_array() {
        assert!(matches!(
            analyze_src("function($v){$v.a}"),
            Some(SimpleLambda::FieldAccess { .. })
        ));
        assert!(matches!(
            analyze_src("function($v){$v.a > 1}"),
            Some(SimpleLambda::FieldPredicate { .. })
        ));
        assert!(matches!(
            analyze_src("function($v){$v.a = $v.b}"),
            Some(SimpleLambda::TwoFieldPredicate { .. })
        ));
        assert!(matches!(
            analyze_src("function($a,$b){$a.x > $b.x}"),
            Some(SimpleLambda::SortComparator { .. })
        ));
        assert!(matches!(
            analyze_src("function($p,$c){$p + $c.x}"),
            Some(SimpleLambda::ReduceAccum { .. })
        ));
        assert!(matches!(
            analyze_src("function($p,$c){$p + $c.x * $c.y}"),
            Some(SimpleLambda::ReduceCompoundAccum { .. })
        ));
        assert!(matches!(
            analyze_src("function($v){$v.a > 1 and $v.b < 5}"),
            Some(SimpleLambda::CompoundPredicate { .. })
        ));
        assert!(matches!(
            analyze_src("function($v){\"id-\" & $string($v.a)}"),
            Some(SimpleLambda::ConcatTemplate { .. })
        ));
    }

    /// Helper: parse, process, and evaluate against `input`.
    fn eval_expr(src: &str, input: &Value) -> Value {
        let (mut arena, root) = Parser::parse(src).expect("parse failed");
        let root = process_ast(&mut arena, root).expect("process failed");
        let mut env = Environment::new();
        crate::stdlib::register_all(&mut env);
        env.bind("$", input.clone());
        let env = Rc::new(env);
        eval(&arena, root, input, &env).expect("eval failed")
    }

    fn nums_input(values: &[f64]) -> Value {
        let items: Vec<Value> = values
            .iter()
            .map(|&x| {
                let mut obj = crate::value::ObjectMap::default();
                obj.insert("x".into(), Value::Number(x));
                Value::Object(Rc::new(obj))
            })
            .collect();
        let mut root = crate::value::ObjectMap::default();
        root.insert("nums".into(), Value::Array(Rc::from(items)));
        Value::Object(Rc::new(root))
    }

    /// Regression test for gnata-bec.1: the mapped-call fast path used
    /// round-half-away-from-zero while $round is half-to-even (banker's).
    /// `nums.$round(x)` is lifted by analyze_mapped_call; `nums.($round(x + 0))`
    /// has a complex argument, so it takes the general path. Both must agree.
    #[test]
    fn round_fast_path_matches_general_path() {
        let input = nums_input(&[0.5, 1.5, 2.5, 3.5, -0.5, -1.5, -2.5, 2.345]);
        let fast = eval_expr("nums.$round(x)", &input);
        let general = eval_expr("nums.($round(x + 0))", &input);
        assert!(
            fast.deep_equal(&general),
            "fast path {fast:?} != general path {general:?}"
        );
        let expected = eval_expr("[0, 2, 2, 4, -0, -2, -2, 2]", &Value::Undefined);
        assert!(
            fast.deep_equal(&expected),
            "banker's rounding expected, got {fast:?}"
        );
    }

    /// Regression test for gnata-bec.2: the fast path formatted with default
    /// separators, silently dropping the $formatNumber options argument.
    /// With options present the call must not be lifted with defaults.
    #[test]
    fn format_number_fast_path_honors_options_arg() {
        let input = nums_input(&[1234.56]);
        let with_opts = eval_expr(
            r##"nums.$formatNumber(x, "#.###,00", {"decimal-separator": ",", "grouping-separator": "."})"##,
            &input,
        );
        let expected = eval_expr(r#""1.234,56""#, &Value::Undefined);
        assert!(
            with_opts.deep_equal(&expected),
            "options ignored: got {with_opts:?}, expected {expected:?}"
        );
        // Without options the lift still applies and must agree with the general path.
        let fast = eval_expr(r##"nums.$formatNumber(x, "#,###.00")"##, &input);
        let general = eval_expr(r##"nums.($formatNumber(x + 0, "#,###.00"))"##, &input);
        assert!(
            fast.deep_equal(&general),
            "fast path {fast:?} != general path {general:?}"
        );
    }

    /// Regression test for jsntrs-6wr.5: a lambda path step that declares
    /// more parameters than the call supplies gets the context item
    /// prepended by `eval_path_function_step`. The lifted arg template has
    /// no context item, so the lift must decline — array and single-item
    /// input must agree, and both must keep the prepended `$a`.
    #[test]
    fn under_supplied_lambda_step_is_not_lifted() {
        let src = r#"( $f := function($a, $b){ $a.x & "/" & $b }; items.$f(y) )"#;
        let array_input =
            Value::from_json_str(r#"{"items": [{"x": "p"}]}"#).expect("valid test JSON");
        let single_input =
            Value::from_json_str(r#"{"items": {"x": "p"}}"#).expect("valid test JSON");
        let from_array = eval_expr(src, &array_input);
        let from_single = eval_expr(src, &single_input);
        assert!(
            from_array.deep_equal(&from_single),
            "array input {from_array:?} != single input {from_single:?}"
        );
        assert!(
            from_array.deep_equal(&Value::String("p/".into())),
            "context item not prepended: got {from_array:?}"
        );
    }

    /// The jsntrs-6wr.5 guard must not over-bail: a call that supplies every
    /// declared parameter never triggers the prepend rule, so it still lifts.
    #[test]
    fn fully_supplied_lambda_step_still_lifts() {
        let input =
            Value::from_json_str(r#"{"items": [{"x": "p", "y": "q"}]}"#).expect("valid test JSON");
        let got = eval_expr(r#"( $f := function($a){ $a & "!" }; items.$f(x) )"#, &input);
        assert!(
            got.deep_equal(&Value::String("p!".into())),
            "expected \"p!\", got {got:?}"
        );
    }

    #[test]
    fn round_fast_path_with_precision_matches_general_path() {
        let input = nums_input(&[0.25, 0.35, -0.25, 1.05]);
        let fast = eval_expr("nums.$round(x, 1)", &input);
        let general = eval_expr("nums.($round(x + 0, 1))", &input);
        assert!(
            fast.deep_equal(&general),
            "fast path {fast:?} != general path {general:?}"
        );
    }
}
