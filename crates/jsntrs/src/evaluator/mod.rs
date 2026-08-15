//! Core JSONata evaluator: tree-walking dispatch over the AST arena.
//!
//! Port of Go `internal/evaluator/evaluator.go` and supporting files.

mod binary;
pub mod environment;
pub mod functions;
mod group;
mod path;
pub mod signature;
mod subscript;

pub use environment::Environment;
pub use functions::{
    BuiltinFn, EnvAwareBuiltinFn, FunctionValue, Lambda, TailCall, call_function, eval_function,
    eval_lambda, eval_partial,
};
pub use signature::{ParamSpec, parse_signature, process_call_args};

pub(crate) use binary::apply_arithmetic;
use binary::{eval_binary, flag_keep_array};
use group::eval_group_by;
use path::{
    collapse_val, eval_descendant_step, eval_path, eval_path_step, flatten_to_vec,
    get_step_bindings, node_has_parent_ref, path_has_tuple_step, value_ptr_eq,
};

use std::rc::Rc;

use crate::error::{JsonataError, JsonataResult};
use crate::parser::{AstArena, BinaryOp, Expr, NodeId, UnaryOp};
use crate::value::{Sequence, Value};

/// Environment variable name for the parent context (`%` operator).
const PARENT_BINDING: &str = "%%";
/// Environment variable name for the join-mode flag.
const JOIN_FLAG: &str = "%%j";
/// Environment variable name for the group-by parent shadow.
///
/// Bound alongside an undefined [`PARENT_BINDING`] in the frame that
/// evaluates group-by key/value pairs: inside such a pair a `%` that finds
/// no nearer parent frame yields undefined instead of **S0217**.
const PARENT_SHADOW: &str = "%%g";

/// Does the nearest [`PARENT_BINDING`] frame visible from `env` carry the
/// group-by shadow marker?
///
/// jsonata-js processes group-by pairs without resolving their ancestor slots
/// (`processAST` case `'{'` never calls `pushAncestry`), so a `%` at the head
/// of a pair expression stays unbound and evaluates to undefined; the
/// undefined-key rule then skips the item and the group yields `{}`. A `%`
/// that resolves *within* the pair (`review.%.genre`) is unaffected, because
/// the inner step rebinds `%%` in a nearer frame than the shadow.
fn parent_shadowed(env: &Rc<Environment>) -> bool {
    Environment::lookup_with_env(env, PARENT_BINDING).is_some_and(|(val, frame)| {
        val.is_undefined() && frame.lookup_direct(PARENT_SHADOW).is_some()
    })
}

/// Build the error a `%` raises when it cannot reach a parent frame.
fn parent_out_of_context() -> JsonataError {
    JsonataError::new("S0217", "% operator used outside of a valid path context")
}

/// Evaluate an AST node against input data in the given environment.
///
/// Returns `Value::Undefined` for missing/undefined results.
/// This is the main entry point — all node types dispatch through here.
///
/// # Errors
/// Returns JSONata-spec error codes for type mismatches, undefined references,
/// stack overflows, cancellation, and other evaluation failures.
/// Public eval entry point. Checks stack on first call, then dispatches
/// to eval_inner which is used for all internal recursive calls (no stack check overhead).
pub fn eval(arena: &AstArena, node: NodeId, input: &Value, env: &Rc<Environment>) -> JsonataResult {
    #[cfg(not(target_arch = "wasm32"))]
    let result = stacker::maybe_grow(crate::STACK_RED_ZONE, crate::STACK_GROW_SIZE, || {
        eval_inner(arena, node, input, env)
    });
    #[cfg(target_arch = "wasm32")]
    let result = eval_inner(arena, node, input, env);

    // Sequence is an internal representation — collapse it before it
    // crosses the public API boundary. Internal recursion goes through
    // eval_inner and never re-enters here.
    result.map(collapse_sequence)
}

/// Collapse an internal [`Sequence`] at a *consumer* boundary.
///
/// This is jsntrs's counterpart to the tail of jsonata-js `evaluate()`:
/// every value that reaches a syntactic position — a call argument, a
/// binary operand, a lambda's returned body value — has already been
/// through `evaluate()` there, so a sequence has already lost its
/// singleton (0 items → undefined, 1 → the item, more → the array). jsntrs
/// evaluates lazily and hands the uncollapsed [`Value::Sequence`] back up,
/// so the collapse has to happen explicitly at each of those boundaries;
/// a sequence must never be handed to a builtin or an operator, which have
/// no notion of it (jsntrs-p0v.6).
///
/// The check is a single discriminant compare on the hot argument path.
#[inline]
pub(crate) fn collapse_sequence(value: Value) -> Value {
    match value {
        Value::Sequence(seq) => seq.into_value(),
        other => other,
    }
}

/// Evaluate a node in a *consumer* position: like [`eval_no_stack_check`],
/// but collapsing a sequence result the way [`collapse_sequence`] describes.
///
/// The result is edited through `&mut` rather than rebuilt with
/// `Result::map`: `JsonataResult` is ~112 bytes (the error carries three
/// heap strings), and moving one through a combinator on every operand cost
/// ~8% on the subscript-predicate benchmark. In this form a non-sequence
/// operand — the overwhelming majority — pays one discriminant compare and
/// no move at all.
#[inline]
pub(crate) fn eval_operand(
    arena: &AstArena,
    node: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    let mut result = eval_inner(arena, node, input, env);
    if let Ok(value) = &mut result {
        collapse_sequence_in_place(value);
    }
    result
}

/// Mark a value as keep-singleton *without* materialising the wrap.
///
/// The counterpart of [`collapse_sequence`], and jsntrs's spelling of
/// jsonata-js's `result.keepSingleton = true` — set by `evaluatePath` for a
/// path's `keepSingletonArray` and by the tail of `evaluate()` for a node's
/// own `keepArray`. It is a *flag*, read only when something later collapses
/// the sequence (the `eval()` boundary, an operand, a call argument).
///
/// Wrapping eagerly in a `Value::Array` instead made the wrap
/// indistinguishable from a real array value, so an enclosing path could no
/// longer drop it while re-sequencing its steps and `x.(a[])` answered `[1]`
/// where the reference answers `1` (jsntrs-p0v.19).
///
/// `Value::Array` is left alone: the reference sets the property on a plain
/// array too, but a plain array is not a sequence there, so nothing reads it.
pub(crate) fn mark_keep_singleton(result: Value) -> Value {
    match result {
        Value::Undefined | Value::Array(_) => result,
        Value::Sequence(mut seq) => {
            seq.keep_singleton = true;
            Value::Sequence(seq)
        }
        scalar => {
            let mut seq = Sequence::with_items(vec![scalar]);
            seq.keep_singleton = true;
            Value::Sequence(Box::new(seq))
        }
    }
}

/// In-place [`collapse_sequence`], for the hot paths that already hold the
/// value behind a `&mut` and must not pay for a move to reach it.
#[inline]
pub(crate) fn collapse_sequence_in_place(value: &mut Value) {
    if matches!(value, Value::Sequence(_)) {
        let taken = std::mem::replace(value, Value::Undefined);
        *value = collapse_sequence(taken);
    }
}

/// Internal eval without stack check. Used for all recursive calls within
/// the evaluator. Stack growth is handled at deep-recursion entry points
/// (call_function for lambda bodies).
pub(crate) fn eval_no_stack_check(
    arena: &AstArena,
    node: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    eval_inner(arena, node, input, env)
}

/// Check remaining stack and grow if needed. Called from deep-recursion
/// entry points (call_function lambda body, etc.).
pub(crate) fn eval_with_stack_check(
    arena: &AstArena,
    node: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    #[cfg(not(target_arch = "wasm32"))]
    {
        stacker::maybe_grow(crate::STACK_RED_ZONE, crate::STACK_GROW_SIZE, || {
            eval_inner(arena, node, input, env)
        })
    }
    #[cfg(target_arch = "wasm32")]
    {
        eval_inner(arena, node, input, env)
    }
}

#[expect(clippy::too_many_lines, clippy::needless_continue)]
fn eval_inner(
    arena: &AstArena,
    node: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    // Iterative evaluation loop with tail-call optimization.
    // Tail positions (Block last expr, Condition then/else, Binary ?:/??/~>)
    // update `cur_node`/`cur_env` and continue the loop instead of recursing.
    //
    // PERF: Single arena.get() + single match per iteration. Group-by check is
    // merged into the dispatch arms to avoid a second match. Rc::clone(env)
    // is deferred to the TCO path — non-looping arms use env directly.
    let mut cur_node = node;
    let mut cur_env_owned: Option<Rc<Environment>> = None;

    loop {
        if cur_node.is_empty() {
            return Ok(Value::Undefined);
        }

        let cur_env = cur_env_owned.as_ref().unwrap_or(env);

        match arena.get(cur_node) {
            // ── Leaf nodes ──
            Expr::ValueLit { value, .. } => return Ok(eval_value_lit(value)),
            Expr::StringLit { value, .. } => return Ok(Value::String(value.clone().into())),
            Expr::NumberLit { value: n, .. } => return Ok(Value::Number(*n)),
            Expr::Variable { name, group, .. } => {
                if group.is_some() {
                    return eval_group_by(arena, cur_node, input, cur_env);
                }
                return Ok(eval_variable(name, input, cur_env));
            }
            Expr::Name { value, group, .. } => {
                if group.is_some() {
                    return eval_group_by(arena, cur_node, input, cur_env);
                }
                return eval_name(value, input);
            }
            Expr::Wildcard { .. } => return eval_wildcard(input),
            // `**` is context-independent: jsonata-js runs the same
            // `evaluateDescendants` here and for a path step, and it always
            // pushes the input itself before recursing. Calling the rootless
            // `descendant_lookup` here dropped the document from a bare `**`
            // (`{"a": 1}` → `1` instead of `[{"a": 1}, 1]`) and made `**` on a
            // scalar or an empty object undefined (jsntrs-p0v.22).
            Expr::Descendant { .. } => {
                return match eval_descendant_step(input) {
                    Value::Sequence(seq) => Ok(seq.into_value()),
                    other => Ok(other),
                };
            }
            Expr::Regex { pattern, flags, .. } => return Ok(eval_regex(pattern, flags)),
            Expr::Parent { .. } => {
                return match cur_env.lookup(PARENT_BINDING) {
                    Some(val) if !val.is_null() && !val.is_undefined() => Ok(val),
                    _ if parent_shadowed(cur_env) => Ok(Value::Undefined),
                    _ => Err(parent_out_of_context()),
                };
            }
            Expr::Placeholder { .. } => return Ok(Value::Undefined),

            // ── Tail-call optimized: Block ──
            Expr::Block { expressions, .. } => {
                let expressions = expressions.clone();
                let child_env = Rc::new(Environment::new_child(Rc::clone(cur_env)));
                if expressions.is_empty() {
                    return Ok(Value::Undefined);
                }
                for &expr in &expressions[..expressions.len() - 1] {
                    eval_no_stack_check(arena, expr, input, &child_env)?;
                }
                cur_node = expressions[expressions.len() - 1];
                cur_env_owned = Some(child_env);
                continue;
            }

            // ── Tail-call optimized: Condition ──
            Expr::Condition {
                condition,
                then,
                else_,
                ..
            } => {
                let (cond_id, then_id, else_id) = (*condition, *then, *else_);
                let cond_val = eval_no_stack_check(arena, cond_id, input, cur_env)?;
                if cond_val.to_boolean()? {
                    cur_node = then_id;
                    continue;
                } else if let Some(e) = else_id {
                    cur_node = e;
                    continue;
                }
                return Ok(Value::Undefined);
            }

            // ── Tail-call optimized: Binary ?:, ??, ~> ──
            Expr::Binary { op, lhs, rhs, .. }
                if matches!(
                    op,
                    BinaryOp::CondTern | BinaryOp::NullCoal | BinaryOp::Chain
                ) =>
            {
                let (op, lhs, rhs) = (*op, *lhs, *rhs);
                match op {
                    BinaryOp::CondTern => {
                        let left = eval_operand(arena, lhs, input, cur_env)?;
                        if left.to_boolean()? {
                            return Ok(left);
                        }
                        cur_node = rhs;
                        continue;
                    }
                    BinaryOp::NullCoal => {
                        let left = eval_operand(arena, lhs, input, cur_env)?;
                        if left.is_undefined() {
                            cur_node = rhs;
                            continue;
                        }
                        return Ok(left);
                    }
                    BinaryOp::Chain => {
                        // The piped value becomes the callee's first
                        // argument, so it collapses like any other operand.
                        let piped = eval_operand(arena, lhs, input, cur_env)?;
                        return eval_chain(arena, rhs, &piped, input, cur_env);
                    }
                    _ => unreachable!("dispatch sends only Dot/Chain binaries here"),
                }
            }

            // ── Non-tail dispatch (group-by check merged in) ──
            Expr::Path { group, steps, .. } => {
                if group.is_some() && !path_has_tuple_step(arena, steps) {
                    return eval_group_by(arena, cur_node, input, cur_env);
                }
                return eval_path(arena, cur_node, input, cur_env);
            }
            Expr::Function { group, .. } => {
                if group.is_some() {
                    return eval_group_by(arena, cur_node, input, cur_env);
                }
                return eval_function(arena, cur_node, input, cur_env);
            }
            Expr::Binary { group, .. } => {
                if group.is_some() {
                    return eval_group_by(arena, cur_node, input, cur_env);
                }
                return eval_binary(arena, cur_node, input, cur_env);
            }
            Expr::Unary { group, .. } => {
                if group.is_some() {
                    return eval_group_by(arena, cur_node, input, cur_env);
                }
                return eval_unary(arena, cur_node, input, cur_env);
            }
            Expr::Grouped { .. } => return eval_group_by(arena, cur_node, input, cur_env),
            Expr::Bind { .. } => return eval_bind(arena, cur_node, input, cur_env),
            Expr::Lambda { .. } => return Ok(eval_lambda(arena, cur_node, input, cur_env)),
            Expr::Partial { .. } => return eval_partial(arena, cur_node, input, cur_env),
            Expr::Sort { .. } => return eval_sort(arena, cur_node, input, cur_env),
            Expr::Transform { .. } => return Ok(eval_transform(arena, cur_node, input, cur_env)),
        }
    }
}

// ── Literal evaluators ──────────────────────────────────────────────

// Consistent return type with eval dispatch table.
fn eval_value_lit(value: &str) -> Value {
    match value {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "null" => Value::Null,
        _ => Value::Undefined,
    }
}

fn eval_variable(name: &str, input: &Value, env: &Rc<Environment>) -> Value {
    if name.is_empty() {
        // Bare $ — refers to the current input context.
        return input.clone();
    }
    env.lookup(name).unwrap_or(Value::Undefined)
}

// ── Name (field access) ─────────────────────────────────────────────

/// Look up a field name in input, with auto-mapping over arrays.
///
/// This is the authoritative implementation of path-step semantics.
/// Simplified versions exist in `fast_path::eval_pure_path` and
/// `fast_path::tape_walk` — update those if this logic changes.
fn eval_name(name: &str, input: &Value) -> JsonataResult {
    match input {
        Value::Object(obj) => match obj.get(name) {
            Some(val) => Ok(val.clone()),
            None => Ok(Value::Undefined),
        },
        Value::Array(arr) => {
            // JSONata auto-maps field lookups across arrays.
            let mut seq = Sequence::with_capacity(arr.len());
            let mut field_found = false;
            for item in arr.iter() {
                let val = eval_name(name, item)?;
                if val.is_undefined() {
                    continue;
                }
                field_found = true;
                // Flatten plain arrays from navigating through arrays.
                match val {
                    Value::Array(inner) => {
                        for sv in inner.iter() {
                            seq.values.push(if sv.is_undefined() {
                                Value::Null
                            } else {
                                sv.clone()
                            });
                        }
                    }
                    other => {
                        seq.append(other);
                    }
                }
            }
            if seq.values.is_empty() {
                if field_found {
                    // Field exists but was empty array — return empty array.
                    return Ok(Value::Array(Rc::from(vec![])));
                }
                return Ok(Value::Undefined);
            }
            Ok(seq.into_value())
        }
        Value::Sequence(s) => eval_name(name, &s.collapse()),
        _ => Ok(Value::Undefined),
    }
}

// ── Wildcard (*) ────────────────────────────────────────────────────

fn eval_wildcard(input: &Value) -> JsonataResult {
    match input {
        Value::Object(obj) => {
            if obj.is_empty() {
                return Ok(Value::Undefined);
            }
            let mut seq = Sequence::new();
            for (_, val) in obj.iter() {
                match val {
                    Value::Array(arr) => seq.values.extend(arr.iter().cloned()),
                    other => seq.values.push(other.clone()),
                }
            }
            if seq.values.is_empty() {
                Ok(Value::Undefined)
            } else {
                Ok(seq.into_value())
            }
        }
        Value::Array(arr) => {
            let mut seq = Sequence::with_capacity(arr.len());
            for item in arr.iter() {
                if item.is_object() {
                    let val = eval_wildcard(item)?;
                    if !val.is_undefined() {
                        seq.append(val);
                    }
                } else if !item.is_undefined() {
                    seq.values.push(item.clone());
                }
            }
            Ok(seq.into_value())
        }
        _ => Ok(Value::Undefined),
    }
}

// ── Descendant (**) ─────────────────────────────────────────────────

fn descendant_lookup(input: &Value) -> Value {
    let mut seq = Sequence::new();
    collect_descendants(input, &mut seq);
    // Return as Sequence (not collapsed) so that appendToSequence callers
    // can flatten properly. collapse() is called by the caller when needed.
    Value::Sequence(Box::new(seq))
}

/// Recursively collect all values at all depths from objects and arrays.
///
/// Matches Go's `descendantLookup`: for objects, each field value is added and
/// recursed into (with arrays expanded to individual items). For arrays, each
/// element is added and recursed into.
fn collect_descendants(input: &Value, seq: &mut Sequence) {
    match input {
        Value::Object(obj) => {
            for (_, val) in obj.iter() {
                if val.is_undefined() {
                    continue;
                }
                // When a field value is an array, iterate its elements directly
                // (add each + recurse), matching Go's descendantLookup which
                // treats arrays as transparent containers.
                if let Value::Array(arr) = val {
                    for item in arr.iter() {
                        seq.append(item.clone());
                        collect_descendants(item, seq);
                    }
                } else {
                    seq.append(val.clone());
                    collect_descendants(val, seq);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter() {
                seq.append(item.clone());
                collect_descendants(item, seq);
            }
        }
        _ => {}
    }
}

// ── Regex ───────────────────────────────────────────────────────────

fn eval_regex(pattern: &str, flags: &str) -> Value {
    // A regex literal evaluates to a {pattern, flags} object. The object is
    // immutable (any mutation path copies via Rc::make_mut), so a literal
    // inside a predicate over 100k items can share one allocation instead of
    // rebuilding the map per element. Bounded like the stdlib regex cache:
    // cleared wholesale when full.
    thread_local! {
        static LITERAL_CACHE: std::cell::RefCell<
            std::collections::HashMap<compact_str::CompactString, Value, foldhash::fast::RandomState>,
        > = std::cell::RefCell::new(std::collections::HashMap::default());
    }
    const LITERAL_CACHE_CAP: usize = 256;
    let mut key = compact_str::CompactString::with_capacity(flags.len() + 1 + pattern.len());
    key.push_str(flags);
    key.push('\0');
    key.push_str(pattern);
    LITERAL_CACHE.with(|cache| {
        if let Some(v) = cache.borrow().get(key.as_str()) {
            return v.clone();
        }
        let mut obj = crate::value::ObjectMap::default();
        obj.insert("pattern".into(), Value::String(pattern.into()));
        obj.insert("flags".into(), Value::String(flags.into()));
        let v = Value::Object(Rc::new(obj));
        let mut map = cache.borrow_mut();
        if map.len() >= LITERAL_CACHE_CAP {
            map.clear();
        }
        map.insert(key, v.clone());
        v
    })
}

// ── Chain operator (~>) ─────────────────────────────────────────────

fn eval_chain(
    arena: &AstArena,
    rhs: NodeId,
    piped: &Value,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    // Flatten right-associative chain: a ~> (f ~> g) → [f, g], then iterate.
    let mut steps = Vec::new();
    let mut current = rhs;
    while let Expr::Binary {
        op, lhs, rhs: rr, ..
    } = arena.get(current)
        && *op == BinaryOp::Chain
    {
        steps.push(*lhs);
        current = *rr;
    }
    steps.push(current);

    // Every stage runs unconditionally, even when the previous one produced
    // undefined: `~>` is sugar for nested application, so
    // `x ~> $f() ~> $g()` must equal `$g($f(x))`. Builtins that reject or
    // propagate undefined do so on their own (jsonata-js
    // `evaluateApplyExpression` behaves the same). Short-circuiting here made
    // `[1,2,3] ~> $filter(…) ~> $count()` yield undefined while the
    // parenthesized form yielded 0.
    let mut result = piped.clone();
    for step in steps {
        result = eval_chain_step(arena, step, &result, input, env)?;
    }
    Ok(result)
}

fn eval_chain_step(
    arena: &AstArena,
    rhs: NodeId,
    piped: &Value,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    // Function call node: prepend piped as first argument.
    if let Expr::Function {
        procedure,
        arguments,
        keep_array,
        ..
    } = arena.get(rhs)
    {
        let procedure = *procedure;
        let arguments = arguments.clone();
        let keep_array = *keep_array;
        let fn_val = eval_no_stack_check(arena, procedure, input, env)?;
        // `x ~> $f(…)` is a function *invocation* in the reference too
        // (`evaluateApplyExpression` delegates to `evaluateFunction` when the
        // right side is a call), so it attributes errors to the callee's name
        // exactly like a direct call: `[1,2] ~> $map(3)` reports token "map".
        let name = functions::call_site_name(arena, procedure);
        let Value::Function(func) = fn_val else {
            return Err(
                JsonataError::new("T1006", "attempted to invoke undefined function").or_token(name),
            );
        };
        let mut args = vec![piped.clone()];
        for &arg_node in &arguments {
            if matches!(arena.get(arg_node), Expr::Placeholder { .. }) {
                args.push(Value::Undefined);
                continue;
            }
            args.push(eval_operand(arena, arg_node, input, env)?);
        }
        let result =
            call_function(&func, &args, input, env, arena).map_err(|e| e.or_token(name))?;
        // Apply keep_array wrapping if the [] suffix is present *and* the
        // result stands in for a sequence — same rule as a direct call
        // (`call_result_is_sequence`), so `x ~> $sum()[]` stays a scalar
        // while `x ~> $map($f)[]` keeps its singleton wrapped.
        if keep_array && functions::call_result_is_sequence(&result) {
            return Ok(flag_keep_array(result, Value::Undefined));
        }
        // A chain stage feeds the next one, so its result is a consumer
        // position too.
        return Ok(collapse_sequence(result));
    }

    // Otherwise evaluate right side and call it.
    let fn_val = eval_no_stack_check(arena, rhs, input, env)?;

    // If right side is a regex object, apply regex test (like $contains).
    if let Value::Object(ref obj) = fn_val
        && obj.contains_key("pattern")
    {
        return apply_regex_chain(piped, obj);
    }

    match &fn_val {
        Value::Function(func) => {
            // If piped is itself a function, compose them rather than calling fn(piped).
            if let Value::Function(piped_fn) = piped {
                let outer = func.clone();
                let inner = piped_fn.clone();
                let env_clone = Rc::clone(env);
                let composed: Rc<crate::evaluator::EnvAwareBuiltinFn> = Rc::new(
                    move |args: &[Value],
                          focus: &Value,
                          _env: &Rc<Environment>,
                          arena: &AstArena| {
                        // Collapse sequences between composition steps.
                        let intermediate = collapse_sequence(call_function(
                            &inner, args, focus, &env_clone, arena,
                        )?);
                        call_function(&outer, &[intermediate], focus, &env_clone, arena)
                    },
                );
                return Ok(Value::Function(Box::new(FunctionValue::EnvAwareBuiltin(
                    composed,
                ))));
            }
            {
                let result = call_function(func, std::slice::from_ref(piped), input, env, arena)?;
                Ok(collapse_sequence(result))
            }
        }
        _ => Err(JsonataError::new(
            "T2006",
            "the right-hand side of the ~> operator must be a function",
        )),
    }
}

/// Apply a regex test to a piped value in chain context (~> /regex/).
/// Returns the first match object if the regex matches, or Undefined if not.
fn apply_regex_chain(piped: &Value, regex_obj: &crate::value::ObjectMap) -> JsonataResult {
    let s = match piped {
        Value::String(s) => &**s,
        _ => return Ok(Value::Undefined),
    };
    let pattern = match regex_obj.get("pattern") {
        Some(Value::String(p)) => &**p,
        _ => return Ok(Value::Undefined),
    };
    let flags = match regex_obj.get("flags") {
        Some(Value::String(f)) => &**f,
        _ => "",
    };
    let re = crate::stdlib::regex::compile_regex(pattern, flags)
        .map_err(|e| JsonataError::new("D1002", format!("invalid regex: {}", e.message)))?;
    if let Some(caps) = re.captures(s) {
        let m = caps.get(0).ok_or_else(|| {
            JsonataError::new(
                "D0000",
                "capture group 0 always exists when captures succeed",
            )
        })?;
        // Build match object similar to $match.
        let mut obj = crate::value::ObjectMap::default();
        obj.insert("match".into(), Value::String(m.as_str().into()));
        let start = s[..m.start()].chars().count();
        let end = s[..m.end()].chars().count();
        obj.insert("start".into(), Value::Number(start as f64));
        obj.insert("end".into(), Value::Number(end as f64));
        // Collect capture groups (skip group 0 which is the full match).
        let mut groups = Vec::new();
        for i in 1..caps.len() {
            match caps.get(i) {
                Some(g) => groups.push(Value::String(g.as_str().into())),
                None => groups.push(Value::String("".into())),
            }
        }
        obj.insert("groups".into(), Value::Array(Rc::from(groups)));
        Ok(Value::Object(Rc::new(obj)))
    } else {
        Ok(Value::Undefined)
    }
}

// ── Unary operators ─────────────────────────────────────────────────

fn eval_unary(
    arena: &AstArena,
    node: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    // The arena is only ever borrowed shared during evaluation, so the
    // constructor child lists are iterated in place — no per-eval Vec clone.
    let (op, operand, expressions, lhs_nodes) = match arena.get(node) {
        Expr::Unary {
            op,
            operand,
            expressions,
            lhs,
            ..
        } => (*op, *operand, expressions, lhs),
        _ => unreachable!("eval_unary is dispatched only for Expr::Unary nodes"),
    };

    match op {
        UnaryOp::Negate => {
            let val = eval_operand(arena, operand, input, env)?;
            if val.is_undefined() {
                return Ok(Value::Undefined);
            }
            // The reference attaches the operator itself (`token: expr.value`,
            // jsonata 2.2.2 jsonata.js:3991).
            let n = val.as_f64().ok_or_else(|| {
                JsonataError::new("D1002", "cannot negate a non-numeric value").with_token("-")
            })?;
            Ok(Value::Number(-n))
        }
        UnaryOp::ArrayCons => {
            // Array constructor.
            let mut result = Vec::new();
            for &expr in expressions {
                // Each item is a consumer position, so a sequence collapses
                // *before* it is spread: `[$map([1], function($x){[1,2]})]`
                // spreads the collapsed `[1,2]`, not the one-item sequence
                // that wraps it (jsntrs-p0v.6).
                let val = eval_operand(arena, expr, input, env)?;
                if val.is_undefined() {
                    continue;
                }
                // Explicit inner array constructors [expr] are preserved as nested elements.
                // All other arrays are spread (flattened).
                let is_explicit_array = matches!(
                    arena.get(expr),
                    Expr::Unary { op, .. } if *op == UnaryOp::ArrayCons
                );
                match val {
                    Value::Array(arr) => {
                        if is_explicit_array {
                            result.push(Value::Array(arr));
                        } else {
                            result.extend(arr.iter().cloned());
                        }
                    }
                    other => result.push(other),
                }
            }
            Ok(Value::Array(Rc::from(result)))
        }
        UnaryOp::ObjCons => {
            // Object constructor.
            let mut obj = crate::value::ObjectMap::default();
            // lhs is flat [k0,v0,k1,v1,...]
            let mut i = 0;
            while i + 1 < lhs_nodes.len() {
                let key_val = eval_operand(arena, lhs_nodes[i], input, env)?;
                // Skip if key is undefined.
                if key_val.is_undefined() {
                    i += 2;
                    continue;
                }
                let key: compact_str::CompactString = match &key_val {
                    Value::String(s) => s.clone(),
                    _ => {
                        return Err(JsonataError::new(
                            "T1003",
                            format!("key expression must evaluate to a string, got {key_val:?}"),
                        ));
                    }
                };
                if obj.contains_key(key.as_str()) {
                    return Err(JsonataError::new(
                        "D1009",
                        format!("duplicate key: \"{key}\""),
                    ));
                }
                let val_val = eval_operand(arena, lhs_nodes[i + 1], input, env)?;
                // Skip if value is undefined.
                if val_val.is_undefined() {
                    i += 2;
                    continue;
                }
                obj.insert(key, val_val);
                i += 2;
            }
            Ok(Value::Object(Rc::new(obj)))
        }
    }
}

// ── Block, condition, bind ──────────────────────────────────────────

// eval_block and eval_condition are handled inline in eval() loop for TCO.

fn eval_bind(
    arena: &AstArena,
    node: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    let (lhs, rhs) = match arena.get(node) {
        Expr::Bind { lhs, rhs, .. } => (*lhs, *rhs),
        _ => unreachable!("eval_bind is dispatched only for Expr::Bind nodes"),
    };

    let val = eval_no_stack_check(arena, rhs, input, env)?;

    // The LHS is a Variable node — extract the name.
    let name = match arena.get(lhs) {
        Expr::Variable { name, .. } => name.clone(),
        _ => {
            return Err(JsonataError::new("D3001", "bind target must be a variable"));
        }
    };

    // Bind in the current environment (uses RefCell for interior mutability).
    env.bind(name, val.clone());
    Ok(val)
}

// ── Sort expression (^) ─────────────────────────────────────────────

fn eval_sort(
    arena: &AstArena,
    node: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    let (sort_expr, terms) = match arena.get(node) {
        Expr::Sort { expr, terms, .. } => (*expr, terms.clone()),
        _ => unreachable!("eval_sort is dispatched only for Expr::Sort nodes"),
    };

    // If any sort term references % (parent), use parent-tracking sort.
    let needs_parent = terms
        .iter()
        .any(|t| node_has_parent_ref(arena, t.expression));
    if needs_parent {
        return eval_sort_with_parent_tracking(arena, sort_expr, &terms, input, env);
    }

    let mut items = eval_no_stack_check(arena, sort_expr, input, env)?;
    if items.is_undefined() {
        return Ok(Value::Undefined);
    }

    // jsonata-js sorts through `fn.sort`, which hands back the *same* array
    // when it holds fewer than two items — so a keep-singleton flag on the
    // sorted sequence survives the sort, and `x.(a[]^(b))` still lets the
    // enclosing path drop it. Longer inputs come back as a fresh plain array
    // there, which is what the un-flagged result already is (jsntrs-p0v.19).
    let keep_singleton = match &mut items {
        Value::Sequence(seq) if seq.keep_singleton => {
            seq.keep_singleton = false;
            true
        }
        _ => false,
    };
    let sorted = sort_evaluated_items(arena, items, &terms, env)?;
    Ok(if keep_singleton {
        mark_keep_singleton(sorted)
    } else {
        sorted
    })
}

/// Order the values a `^(…)` sort expression produced, applying JSONata's
/// singleton rules: a non-array input of one item stays unwrapped, anything
/// array-shaped comes back as an array.
fn sort_evaluated_items(
    arena: &AstArena,
    items: Value,
    terms: &[crate::parser::SortTerm],
    env: &Rc<Environment>,
) -> JsonataResult {
    let (mut arr, was_array) = match items {
        Value::Array(a) => (a.to_vec(), true),
        Value::Sequence(seq) => {
            let collapsed = seq.into_value();
            match collapsed {
                Value::Undefined => return Ok(Value::Undefined),
                Value::Array(a) => (a.to_vec(), true),
                other => (vec![other], false),
            }
        }
        other => (vec![other], false),
    };

    if terms.is_empty() {
        if !was_array && arr.len() == 1 {
            return arr
                .into_iter()
                .next()
                .ok_or_else(|| JsonataError::new("D0000", "len is 1 but next() returned None"));
        }
        return Ok(Value::Array(Rc::from(arr)));
    }

    // Fast path: if every sort term is a simple Name node, extract field names
    // and use direct field comparison (no evaluator dispatch per comparison).
    let simple_fields: Option<Vec<(&str, bool)>> =
        if crate::fast_path::testing::fast_paths_disabled() {
            None
        } else {
            terms
                .iter()
                .map(|t| match arena.get(t.expression) {
                    Expr::Name { value, .. } => Some((value.as_str(), t.descending)),
                    _ => None,
                })
                .collect()
        };

    if let Some(fields) = simple_fields {
        crate::fast_path::testing::record_hit();
        arr = crate::try_sort::try_sort_by(arr, |a, b| {
            for &(field, descending) in &fields {
                match crate::stdlib::hof_fast::compare_by_field_checked(a, b, field)? {
                    std::cmp::Ordering::Equal => {}
                    ord => return Ok(if descending { ord.reverse() } else { ord }),
                }
            }
            Ok(std::cmp::Ordering::Equal)
        })?;
    } else {
        // Full evaluator path for complex sort term expressions.
        arr = crate::try_sort::try_sort_by(arr, |a, b| {
            compare_sort_terms(arena, terms, a, b, env, env).map(|c| c.cmp(&0))
        })?;
    }

    if !was_array && arr.len() == 1 {
        return arr
            .into_iter()
            .next()
            .ok_or_else(|| JsonataError::new("D0000", "len is 1 but next() returned None"));
    }
    Ok(Value::Array(Rc::from(arr)))
}

/// Sort with parent-tracking: when sort terms reference %, we need to build
/// tuple contexts so each item has its parent environment for % evaluation.
fn eval_sort_with_parent_tracking(
    arena: &AstArena,
    sort_expr: NodeId,
    terms: &[crate::parser::SortTerm],
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    // Build tuple contexts from the sort expression's inner path.
    let ctxs = build_sort_ctxs(arena, sort_expr, input, env)?;
    if ctxs.is_empty() {
        return Ok(Value::Undefined);
    }

    let sorted = crate::try_sort::try_sort_by(ctxs, |a, b| {
        compare_sort_terms(arena, terms, &a.0, &b.0, &a.1, &b.1).map(|c| c.cmp(&0))
    })?;

    let mut seq = Sequence::new();
    for (val, _) in &sorted {
        seq.append(val.clone());
    }
    Ok(seq.into_value())
}

/// Build pathCtx tuples for a sort expression, splitting paths into prefix + lastStep
/// so that parent bindings are preserved.
fn build_sort_ctxs(
    arena: &AstArena,
    sort_expr: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> Result<Vec<(Value, Rc<Environment>)>, JsonataError> {
    if let Expr::Path { steps, .. } = arena.get(sort_expr) {
        let steps = steps.clone();
        if steps.is_empty() {
            return Ok(vec![]);
        }
        // Walk prefix steps in tuple mode to build parent contexts.
        let prefix_ctxs = walk_prefix_steps(arena, &steps[..steps.len() - 1], input, env)?;
        // Expand the last step with parent tracking.
        return expand_last_step(arena, steps[steps.len() - 1], &prefix_ctxs);
    }

    // Non-path expression: evaluate normally and wrap results.
    let result = eval_no_stack_check(arena, sort_expr, input, env)?;
    if result.is_undefined() {
        return Ok(vec![]);
    }
    match result {
        Value::Array(arr) => Ok(arr.iter().cloned().map(|v| (v, env.clone())).collect()),
        other => Ok(vec![(other, env.clone())]),
    }
}

/// Walk prefix path steps in tuple mode, binding parent context at each step.
fn walk_prefix_steps(
    arena: &AstArena,
    steps: &[NodeId],
    input: &Value,
    env: &Rc<Environment>,
) -> Result<Vec<(Value, Rc<Environment>)>, JsonataError> {
    let mut ctxs: Vec<(Value, Rc<Environment>)> = vec![(input.clone(), env.clone())];
    for &step in steps {
        let mut next: Vec<(Value, Rc<Environment>)> = Vec::new();
        for (val, ctx_env) in &ctxs {
            let val = collapse_val(val);
            if val.is_undefined() {
                continue;
            }
            let result = eval_path_step(arena, step, &val, ctx_env, false, false)?;
            if result.is_undefined() {
                continue;
            }
            let (index_var, focus_var) = get_step_bindings(arena, step);
            let is_join = focus_var.is_some();
            let items = flatten_to_vec(result);
            for (j, elem) in items.iter().enumerate() {
                let child_env = Environment::new_child(Rc::clone(ctx_env));
                child_env.bind(PARENT_BINDING, val.clone());
                if is_join {
                    child_env.bind(JOIN_FLAG, Value::Bool(true));
                }
                if let Some(ref var_name) = index_var {
                    child_env.bind(var_name.clone(), Value::Number(j as f64));
                }
                if let Some(ref var_name) = focus_var {
                    child_env.bind(var_name.clone(), elem.clone());
                }
                let ctx_value = if is_join { val.clone() } else { elem.clone() };
                next.push((ctx_value, Rc::new(child_env)));
            }
        }
        ctxs = next;
        if ctxs.is_empty() {
            return Ok(vec![]);
        }
    }
    Ok(ctxs)
}

/// Expand prefix contexts via the final step with parent tracking.
fn expand_last_step(
    arena: &AstArena,
    last_step: NodeId,
    prefix_ctxs: &[(Value, Rc<Environment>)],
) -> Result<Vec<(Value, Rc<Environment>)>, JsonataError> {
    let mut ctxs: Vec<(Value, Rc<Environment>)> = Vec::new();
    for (val, ctx_env) in prefix_ctxs {
        let val = collapse_val(val);
        if val.is_undefined() {
            continue;
        }
        let result = eval_path_step(arena, last_step, &val, ctx_env, false, false)?;
        if result.is_undefined() {
            continue;
        }
        let (index_var, focus_var) = get_step_bindings(arena, last_step);
        let is_join = focus_var.is_some();
        let items = flatten_to_vec(result);
        for (j, elem) in items.iter().enumerate() {
            let child_env = Environment::new_child(Rc::clone(ctx_env));
            child_env.bind(PARENT_BINDING, val.clone());
            if is_join {
                child_env.bind(JOIN_FLAG, Value::Bool(true));
            }
            if let Some(ref var_name) = index_var {
                child_env.bind(var_name.clone(), Value::Number(j as f64));
            }
            if let Some(ref var_name) = focus_var {
                child_env.bind(var_name.clone(), elem.clone());
            }
            let ctx_value = if is_join { val.clone() } else { elem.clone() };
            ctxs.push((ctx_value, Rc::new(child_env)));
        }
    }
    Ok(ctxs)
}

fn compare_sort_terms(
    arena: &AstArena,
    terms: &[crate::parser::SortTerm],
    a: &Value,
    b: &Value,
    a_env: &Rc<Environment>,
    b_env: &Rc<Environment>,
) -> Result<i8, JsonataError> {
    for term in terms {
        // Sort keys are consumer positions: a sequence collapses before the
        // comparison, or `^($map(…))` compares a sequence and errors T2008.
        let av = eval_operand(arena, term.expression, a, a_env)?;
        let bv = eval_operand(arena, term.expression, b, b_env)?;
        let cmp = av.compare_order(&bv)?;
        if cmp != 0 {
            return if term.descending { Ok(-cmp) } else { Ok(cmp) };
        }
    }
    Ok(0)
}

// ── Transform expression (|pattern|update,delete|) ──────────────────

// Consistent return type with eval dispatch table.
fn eval_transform(arena: &AstArena, node: NodeId, _input: &Value, env: &Rc<Environment>) -> Value {
    // Transform returns a function that, when applied to input, performs the transformation.
    let (pattern, update, delete) = match arena.get(node) {
        Expr::Transform {
            pattern,
            update,
            delete,
            ..
        } => (*pattern, *update, *delete),
        _ => unreachable!("eval_transform is dispatched only for Expr::Transform nodes"),
    };

    // Return a function value that performs the transform when called.
    // When called via the pipe operator (~>), the piped value is args[0].
    let env_for_fn = Rc::clone(env);
    let transform_fn: Rc<crate::evaluator::EnvAwareBuiltinFn> = Rc::new(
        move |args: &[Value], _focus: &Value, _env: &Rc<Environment>, arena: &AstArena| {
            let doc = transform_document(args)?;
            apply_transform(arena, pattern, update, delete, &doc, &env_for_fn)
        },
    );

    Value::Function(Box::new(FunctionValue::EnvAwareBuiltin(transform_fn)))
}

/// Type-check a transform's document argument the way its signature does.
///
/// `evaluateTransformExpression` ends with
/// `return defineFunction(transformer, '<(oa):o>')`, so `apply()` validates
/// the document *before* the transformer body runs: an object or an array
/// passes, everything else — a number, a string, a boolean, `null`, a
/// function, or no argument at all — is `T0410` on argument 1. Being a
/// signature check, it also beats the clauses' own errors: `5 ~> |$|5|` is
/// T0410, not the update clause's T2011.
///
/// `undefined` is the one value that passes: the reference's `getSymbol`
/// maps it to the wildcard `'m'`, and the transformer's first line returns
/// undefined for it, which is what `nope ~> |$|{"z": 1}|` answers.
///
/// jsntrs used to hand every value straight to [`apply_transform`], which
/// returned a non-object unchanged, and to substitute the context node when
/// the call supplied no argument at all — a Go-reference habit the JS
/// signature has no room for (jsntrs-2bc).
fn transform_document(args: &[Value]) -> JsonataResult {
    let Some(arg) = args.first() else {
        return Err(transform_signature_error());
    };
    let doc = collapse_sequence(arg.clone());
    match doc {
        Value::Undefined | Value::Object(_) | Value::Array(_) => Ok(doc),
        _ => Err(transform_signature_error()),
    }
}

/// The `T0410` a transform raises for a document that is not an object or an
/// array — jsonata-js's `throwValidationError` reports the first argument.
fn transform_signature_error() -> JsonataError {
    JsonataError::new(
        "T0410",
        "argument 1 does not match function signature: the transform operator \
         needs an object or an array",
    )
}

fn apply_transform(
    arena: &AstArena,
    pattern: NodeId,
    update: NodeId,
    delete: Option<NodeId>,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    if input.is_undefined() {
        return Ok(Value::Undefined);
    }
    // Shallow clone: Rc refcount bumps only. Rc::make_mut in replace_in_value
    // handles copy-on-write — only containers along the mutation path are cloned.
    let cloned = input.clone();

    let matched = eval_no_stack_check(arena, pattern, &cloned, env)?;

    // Collect target objects from the matched result.
    // These are VALUE copies of the objects found at matched positions.
    // Sequences may appear if the pattern expression produces one as its final result.
    let matched = match matched {
        Value::Sequence(seq) => seq.into_value(),
        other => other,
    };
    let mut targets: Vec<Value> = Vec::new();
    match &matched {
        Value::Object(_) => targets.push(matched.clone()),
        Value::Array(arr) => {
            for item in arr.iter() {
                if item.is_object() {
                    targets.push(item.clone());
                }
            }
        }
        _ => {}
    }

    if targets.is_empty() && !matched.is_undefined() {
        // Non-object match: validate clauses but don't mutate.
        validate_transform_clauses(arena, update, delete, &cloned, env)?;
        return Ok(cloned);
    }

    // For each matched target, compute what it looks like AFTER applying the transform.
    // Then walk the clone and replace every occurrence of the original target value
    // with its updated version. This mirrors Go's pointer-based mutation: since Go
    // eval returns pointers into the clone, mutating them mutates the clone directly.
    // In Rust we use value equality to locate the same structural positions.
    let mut replacements: Vec<(Value, Value)> = Vec::new();
    for target in &targets {
        let updated = compute_updated_object(arena, update, delete, target, env)?;
        replacements.push((target.clone(), updated));
    }

    let mut result = cloned;
    replace_all_in_value(&mut result, &replacements);
    Ok(result)
}

/// Compute the updated form of a single matched object by applying update and delete clauses.
/// The update/delete expressions are evaluated with the original (pre-update) object as context,
/// matching Go's behavior where Eval(node.Update, target, env) uses the unmodified target.
fn compute_updated_object(
    arena: &AstArena,
    update: NodeId,
    delete: Option<NodeId>,
    target: &Value,
    env: &Rc<Environment>,
) -> Result<Value, JsonataError> {
    let mut result = target.clone();

    if !update.is_empty() {
        // Evaluate update expression with the original target as context.
        let update_val = eval_operand(arena, update, target, env)?;
        if !update_val.is_undefined() && !update_val.is_null() {
            if let Value::Object(updates) = update_val {
                if let Value::Object(ref mut obj) = result {
                    let obj = Rc::make_mut(obj);
                    for (k, v) in updates.iter() {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            } else {
                return Err(JsonataError::new(
                    "T2011",
                    "the insert/update clause of the transform expression must evaluate to an object",
                ));
            }
        }
    }

    if let Some(del) = delete {
        // Evaluate delete expression with the original target as context (pre-update),
        // matching Go: Eval(node.Delete, target, env) where target is the original object.
        let delete_val = eval_operand(arena, del, target, env)?;
        if !delete_val.is_undefined() && !delete_val.is_null() {
            check_delete_clause(&delete_val)?;
            match delete_val {
                Value::String(key) => {
                    if let Value::Object(ref mut obj) = result {
                        Rc::make_mut(obj).shift_remove(&*key);
                    }
                }
                Value::Array(keys) => {
                    if let Value::Object(ref mut obj) = result {
                        let obj = Rc::make_mut(obj);
                        for k in keys.iter() {
                            if let Value::String(key) = k {
                                obj.shift_remove(&**key);
                            }
                        }
                    }
                }
                // `check_delete_clause` already rejected every other shape.
                _ => {}
            }
        }
    }

    Ok(result)
}

/// The transform delete clause must be a string or an array of strings, the
/// reference's `isArrayOfStrings` gate. A non-string array element is
/// **T2012**, not a key that is quietly skipped (jsntrs-p0v.4).
fn check_delete_clause(delete_val: &Value) -> Result<(), JsonataError> {
    let ok = match delete_val {
        Value::String(_) => true,
        Value::Array(keys) => keys.iter().all(|k| matches!(k, Value::String(_))),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(JsonataError::new(
            "T2012",
            "the delete clause of the transform expression must evaluate to a string or array of strings",
        ))
    }
}

/// Single-pass replacement: walk `value` once and replace every occurrence of any
/// target in `replacements` (matched by Rc pointer identity). A single walk is
/// required because `Rc::make_mut` on non-target containers during the walk would
/// COW-copy their children, destroying the Rc identity that later per-target walks
/// would need.
fn replace_all_in_value(value: &mut Value, replacements: &[(Value, Value)]) {
    for (original, replacement) in replacements {
        if value_ptr_eq(value, original) {
            *value = replacement.clone();
            return;
        }
    }
    match value {
        Value::Array(arr) => {
            let mut vec = arr.to_vec();
            for item in &mut vec {
                replace_all_in_value(item, replacements);
            }
            *arr = Rc::from(vec);
        }
        Value::Object(obj) => {
            for v in Rc::make_mut(obj).values_mut() {
                replace_all_in_value(v, replacements);
            }
        }
        _ => {}
    }
}

fn validate_transform_clauses(
    arena: &AstArena,
    update: NodeId,
    delete: Option<NodeId>,
    target: &Value,
    env: &Rc<Environment>,
) -> Result<(), JsonataError> {
    if !update.is_empty() {
        let update_val = eval_operand(arena, update, target, env)?;
        if !update_val.is_undefined() && !update_val.is_null() && !update_val.is_object() {
            return Err(JsonataError::new(
                "T2011",
                "the insert/update clause of the transform expression must evaluate to an object",
            ));
        }
    }
    if let Some(del) = delete {
        let delete_val = eval_operand(arena, del, target, env)?;
        if !delete_val.is_undefined() && !delete_val.is_null() {
            check_delete_clause(&delete_val)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::parser::ast::push_children;

    use super::group::uses_group_bindings;
    use super::path::{node_has_index_binding, subtree_any};
    use super::*;
    use crate::parser::{Parser, process_ast};
    use std::rc::Rc;

    /// Every node reachable from `root` via the canonical child edges.
    fn all_reachable(arena: &AstArena, root: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if id.is_empty() {
                continue;
            }
            out.push(id);
            push_children(arena.get(id), &mut stack);
        }
        out
    }

    /// Test-local reimplementation of the path-local index/focus-binding
    /// reachability, used as ground truth against the precomputed table.
    fn walk_index_binding(arena: &AstArena, node: NodeId) -> bool {
        if node.is_empty() {
            return false;
        }
        let own = matches!(
            arena.get(node),
            Expr::Name { index: Some(_), .. }
                | Expr::Name { focus: Some(_), .. }
                | Expr::Variable { index: Some(_), .. }
                | Expr::Variable { focus: Some(_), .. }
                | Expr::Binary { index: Some(_), .. }
                | Expr::Binary { focus: Some(_), .. }
                | Expr::Sort { index: Some(_), .. }
                | Expr::Sort { focus: Some(_), .. }
                | Expr::Block { index: Some(_), .. }
                | Expr::Block { focus: Some(_), .. }
        );
        if own {
            return true;
        }
        match arena.get(node) {
            Expr::Binary {
                op: BinaryOp::Subscript,
                lhs,
                rhs,
                ..
            } => walk_index_binding(arena, *lhs) || walk_index_binding(arena, *rhs),
            Expr::Sort { expr, .. } => walk_index_binding(arena, *expr),
            Expr::Path { steps, .. } => steps.iter().any(|&s| walk_index_binding(arena, s)),
            _ => false,
        }
    }

    /// The `process_ast` flag table must agree with independent subtree
    /// walks on every reachable node, across the property-relevant shapes.
    #[test]
    fn static_flags_agree_with_subtree_walks() {
        let exprs = [
            "a.b.c",
            "a@$v.$v",
            "a#$i.$i",
            "a@$v#$i.[$v, $i]",
            "(a)@$v.$v",
            "a.($ * 2)#$i.$i",
            "a[0]#$i.$i",
            "data^(<p)#$i.$i",
            "a.b.%",
            "items[%.total > 5]",
            "a.b[%.c].d",
            "a.{\"x\": %.y}",
            "a{\"k\": $index}",
            "a{$string(b): $key & \"x\"}",
            "a{\"k\": v}",
            "function($x) { $x.b[%.c] }",
            "$map(a, function($v, $i) { $v[%.x] })",
            "|a|{\"x\": %}|",
            "a^(<b, >c).d",
            "**.x[%.y]",
            "a[b[c > 1][%.d]]",
            "($x := a; $x[%.b])",
        ];
        for src in exprs {
            let (mut arena, root) = Parser::parse(src).expect("parse failed");
            let root = process_ast(&mut arena, root).expect("process failed");
            for id in all_reachable(&arena, root) {
                assert!(
                    arena.node_static_flags(id).is_some(),
                    "{src}: reachable node {id:?} has no computed flags"
                );
                assert_eq!(
                    node_has_parent_ref(&arena, id),
                    subtree_any(&arena, id, |e| matches!(e, Expr::Parent { .. })),
                    "{src}: parent-ref flag mismatch at {id:?}"
                );
                assert_eq!(
                    uses_group_bindings(&arena, id),
                    subtree_any(
                        &arena,
                        id,
                        |e| matches!(e, Expr::Variable { name, .. } if name == "index" || name == "key"),
                    ),
                    "{src}: group-bindings flag mismatch at {id:?}"
                );
                assert_eq!(
                    node_has_index_binding(&arena, id),
                    walk_index_binding(&arena, id),
                    "{src}: index-binding flag mismatch at {id:?}"
                );
            }
        }
    }

    /// Helper: parse, process, and evaluate.
    fn eval_expr(src: &str, input: &Value) -> JsonataResult {
        let (mut arena, root) = Parser::parse(src).expect("parse failed");
        let root = process_ast(&mut arena, root).expect("process failed");
        let mut env = Environment::new();
        crate::stdlib::register_all(&mut env);
        if !input.is_undefined() {
            env.bind("$", input.clone());
        }
        let env = Rc::new(env);
        eval(&arena, root, input, &env)
    }

    fn eval_simple(src: &str) -> Value {
        collapse_result(eval_expr(src, &Value::Undefined).expect("eval failed"))
    }

    fn eval_with_data(src: &str, json: &str) -> Value {
        let input = Value::from_json_str(json).expect("invalid JSON");
        collapse_result(eval_expr(src, &input).expect("eval failed"))
    }

    /// Collapse Sequence values for test comparison (mirrors Expression API behavior).
    fn collapse_result(val: Value) -> Value {
        match val {
            Value::Sequence(seq) => seq.into_value(),
            other => other,
        }
    }

    /// Assert a value is undefined (cannot use assert_eq because Undefined != Undefined in JSONata).
    fn assert_undefined(val: &Value) {
        assert!(val.is_undefined(), "expected Undefined, got {val:?}");
    }

    // ── Literals ────────────────────────────────────────────────

    #[test]
    fn eval_number() {
        assert_eq!(eval_simple("42"), Value::Number(42.0));
    }

    #[test]
    fn eval_string() {
        assert_eq!(eval_simple(r#""hello""#), Value::String("hello".into()));
    }

    #[test]
    fn eval_true() {
        assert_eq!(eval_simple("true"), Value::Bool(true));
    }

    #[test]
    fn eval_false() {
        assert_eq!(eval_simple("false"), Value::Bool(false));
    }

    #[test]
    fn eval_null() {
        assert_eq!(eval_simple("null"), Value::Null);
    }

    // ── Variables ───────────────────────────────────────────────

    #[test]
    fn bare_dollar_returns_input() {
        let input = Value::Number(99.0);
        let result = eval_expr("$", &input).unwrap();
        assert_eq!(result, Value::Number(99.0));
    }

    #[test]
    fn undefined_variable() {
        assert!(eval_simple("$x").is_undefined());
    }

    // ── Name (field access) ─────────────────────────────────────

    #[test]
    fn field_access_object() {
        assert_eq!(
            eval_with_data("name", r#"{"name": "Alice"}"#),
            Value::String("Alice".into())
        );
    }

    #[test]
    fn field_access_missing() {
        assert!(eval_with_data("age", r#"{"name": "Alice"}"#).is_undefined());
    }

    #[test]
    fn field_access_array_auto_map() {
        let result = eval_with_data("name", r#"[{"name": "A"}, {"name": "B"}]"#);
        assert_eq!(
            result,
            Value::Array(Rc::from(vec![
                Value::String("A".into()),
                Value::String("B".into())
            ]))
        );
    }

    // ── Path ────────────────────────────────────────────────────

    #[test]
    fn dot_path() {
        assert_eq!(
            eval_with_data("a.b", r#"{"a": {"b": 42}}"#),
            Value::Number(42.0)
        );
    }

    #[test]
    fn deep_path() {
        assert_eq!(
            eval_with_data("a.b.c", r#"{"a": {"b": {"c": "deep"}}}"#),
            Value::String("deep".into())
        );
    }

    #[test]
    fn path_auto_mapping() {
        // Field access on array of objects maps across elements.
        let result = eval_with_data(
            "Account.Order.Product.Price",
            r#"{"Account": {"Order": [{"Product": {"Price": 10}}, {"Product": {"Price": 20}}]}}"#,
        );
        assert_eq!(
            result,
            Value::Array(Rc::from(vec![Value::Number(10.0), Value::Number(20.0)]))
        );
    }

    #[test]
    fn path_string_in_dot() {
        // String literal in path is a field name lookup.
        assert_eq!(
            eval_with_data(r#"a."b c""#, r#"{"a": {"b c": 99}}"#),
            Value::Number(99.0)
        );
    }

    #[test]
    fn path_undefined_short_circuits() {
        // If any intermediate step is undefined, the whole path is undefined.
        assert!(eval_with_data("a.b.c", r#"{"a": {"x": 1}}"#).is_undefined());
    }

    #[test]
    fn path_subscript_filter() {
        let result = eval_with_data("nums[$ > 2]", r#"{"nums": [1, 2, 3, 4]}"#);
        assert_eq!(
            result,
            Value::Array(Rc::from(vec![Value::Number(3.0), Value::Number(4.0)]))
        );
    }

    #[test]
    fn path_subscript_index() {
        assert_eq!(
            eval_with_data("items[0]", r#"{"items": ["a", "b", "c"]}"#),
            Value::String("a".into())
        );
    }

    #[test]
    fn path_subscript_negative_index() {
        assert_eq!(
            eval_with_data("items[-1]", r#"{"items": ["a", "b", "c"]}"#),
            Value::String("c".into())
        );
    }

    /// A numeric-field predicate probes against the FIRST item (Go:
    /// rightCtx := items[0]) and selects that single index — probing the
    /// whole array auto-maps the field into an all-numeric array and
    /// wrongly selects one element per item.
    #[test]
    fn path_subscript_numeric_field_probes_first_item() {
        assert_eq!(
            eval_with_data("a[i]", r#"{"a": [{"i": 0}, {"i": 0}]}"#),
            eval_with_data("$", r#"{"i": 0}"#)
        );
        assert_eq!(
            eval_with_data("a[i]", r#"{"a": [{"i": 0}, {"i": 1}]}"#),
            eval_with_data("$", r#"{"i": 0}"#)
        );
        assert_eq!(
            eval_with_data("a[b]", r#"{"a": [{"b": 1}, {"b": 0}]}"#),
            eval_with_data("$", r#"{"b": 0}"#)
        );
    }

    /// Group-by `{...}` also attaches to Function/Binary/Unary nodes: the
    /// pairs must be processed (dots → paths) and dispatch must route to
    /// eval_group_by. Expectations verified against the Go engine, except
    /// the dotted-pairs Function case where Go itself errors ("unknown
    /// binary operator: .") — that one follows jsonata-js.
    #[test]
    fn group_by_on_function_binary_unary_nodes() {
        // Unary array constructor.
        assert_eq!(
            eval_simple(r#"[1,2,3]{"num": $}"#),
            eval_simple(r#"{"num": [1,2,3]}"#)
        );
        assert_eq!(
            eval_simple(r#"[{"a":1},{"a":2}]{"n": a}"#),
            eval_simple(r#"{"n": [1,2]}"#)
        );
        // Unary negation.
        assert_eq!(
            eval_with_data(r#"-a{"k": $}"#, r#"{"a":3}"#),
            eval_simple(r#"{"k": -3}"#)
        );
        // Binary subscript.
        assert_eq!(
            eval_simple(r#"[{"a":1},{"a":2}][0]{"n": a}"#),
            eval_simple(r#"{"n": 1}"#)
        );
        // Function call, with and without dots in the pairs.
        assert_eq!(
            eval_simple(r#"$string(1){"k": $}"#),
            eval_simple(r#"{"k": "1"}"#)
        );
        assert_eq!(
            eval_simple(r#"$map([{"a":{"b":1}}], function($x){$x}){"k": a.b}"#),
            eval_simple(r#"{"k": 1}"#)
        );
    }

    /// Group-by on nodes with no inline group slot — Block and literals —
    /// goes through the Grouped wrapper (gnata-eci.8). All expectations
    /// Go-verified 2026-08-07; the parser used to drop these groups
    /// silently, so (1+2){"k": $} evaluated to 3.
    #[test]
    fn group_by_on_block_and_literal_nodes() {
        assert_eq!(eval_simple(r#"(1+2){"k": $}"#), eval_simple(r#"{"k": 3}"#));
        assert_eq!(eval_simple(r#"-3{"k": $}"#), eval_simple(r#"{"k": -3}"#));
        assert_eq!(eval_simple(r#""s"{"k": $}"#), eval_simple(r#"{"k": "s"}"#));
        assert_eq!(
            eval_simple(r#"true{"k": $}"#),
            eval_simple(r#"{"k": true}"#)
        );
        assert_eq!(
            eval_simple(r#"null{"k": $}"#),
            eval_simple(r#"{"k": null}"#)
        );
        // A block yielding a sequence groups over its items, and dotted
        // pairs inside the group must still be processed into paths.
        assert_eq!(
            eval_with_data(r#"(a){"k": $}"#, r#"{"a": [1, 2]}"#),
            eval_simple(r#"{"k": [1, 2]}"#)
        );
        assert_eq!(
            eval_simple(r#"([{"a":{"b":1}},{"a":{"b":2}}]){"k": a.b}"#),
            eval_simple(r#"{"k": [1, 2]}"#)
        );
        // Chained access into the grouped result — including the Grouped
        // node standing as the first step of a path.
        assert_eq!(eval_simple(r#"((1+2){"k": $}).k"#), Value::Number(3.0));
        assert_eq!(eval_simple(r#"(1+2){"k": $}.k"#), Value::Number(3.0));
    }

    /// Focus/index bindings on Block path steps evaluate like Go instead
    /// of being silently discarded by the parser (gnata-0mb.4). All five
    /// expectations Go-verified 2026-08-07.
    #[test]
    fn focus_and_index_bindings_on_block_steps() {
        let data = r#"{"a": [1, 2]}"#;
        for (src, expected) in [
            ("a.($ * 2)@$v.$v", "[2, 4]"),
            ("(a)@$v.$v", "[1, 2]"),
            ("a@$v.$v", "[1, 2]"),
            ("a.($ * 2)#$i.$i", "[0, 0]"),
            ("(a)#$i.$i", "[0, 1]"),
        ] {
            assert_eq!(
                eval_with_data(src, data),
                eval_simple(expected),
                "for {src}"
            );
        }
    }

    /// Sort comparators that raise a JSONata error mid-sort must surface
    /// the error, never a total-order violation panic from std sort —
    /// large arrays exercise the merge path (gnata-emj.9).
    #[test]
    fn sort_error_on_large_arrays_returns_error_without_panic() {
        let nums: Vec<String> = (0..100).map(|i| i.to_string()).collect();
        let mixed = format!(
            "[{}, \"x\", {}]",
            nums[..50].join(","),
            nums[50..].join(",")
        );
        for expr in [format!("{mixed}^($)"), format!("$sort({mixed})")] {
            let err = eval_expr(&expr, &Value::Undefined).unwrap_err();
            assert_eq!(err.code, "T2007", "for {expr}");
        }
        // The $sort comparator protocol never returns Greater; a
        // degenerate always-true comparator must also stay panic-free
        // (order is unspecified, as in Go's sort.SliceStable).
        let count = eval_simple(&format!(
            "$count($sort([{}], function($a,$b){{true}}))",
            nums.join(",")
        ));
        assert_eq!(count, Value::Number(100.0));
    }

    /// Undefined group keys skip the item; null (like any non-string)
    /// raises T1003. Go-verified 2026-08-07: nil continues,
    /// jsonNullType errors (gnata-eci.5).
    #[test]
    fn group_by_key_null_vs_undefined() {
        // Absent key: item skipped, others grouped.
        assert_eq!(
            eval_simple(r#"[{"a":"x"},{"b":9},{"a":"y"}]{a: a}"#),
            eval_simple(r#"{"x": "x", "y": "y"}"#)
        );
        // Null (or any non-string) key: T1003, not a skip.
        for expr in [
            r#"[{"a":"x"},{"a":null},{"a":"y"}]{a: a}"#,
            r#"[{"a":"x"},{"a":true}]{a: a}"#,
        ] {
            let err = eval_expr(expr, &Value::Undefined).unwrap_err();
            assert_eq!(err.code, "T1003", "expected T1003 for {expr}");
        }
    }

    /// Tuple mode (`#$v` / `@$v` / `%` anywhere in the path) applies the same
    /// key rules as the standard group path: undefined skips the item, null
    /// and every other non-string raises T1003, and a group whose keys all
    /// resolve to undefined yields `{}` rather than undefined.
    /// jsonata-js 2.x-verified 2026-08-14 (jsntrs-p0v.1).
    #[test]
    fn tuple_group_by_key_null_vs_undefined() {
        let data = r#"{"lib":{"books":[{"g":"a","t":"t1"},{"t":"t2"}]}}"#;
        let none = r#"{"lib":{"books":[{"t":"t1"},{"t":"t2"}]}}"#;
        // Undefined key: item skipped, not an error — same as `lib.books{g: t}`.
        for expr in ["lib.books#$i{g: t}", "lib.books@$b{$b.g: $b.t}"] {
            assert_eq!(
                eval_with_data(expr, data),
                eval_simple(r#"{"a": "t1"}"#),
                "for {expr}"
            );
            assert_eq!(
                eval_with_data(expr, none),
                eval_simple("{}"),
                "all-undefined keys must yield an empty object for {expr}"
            );
        }
        // Null (or any other non-string) key: T1003, not a skip.
        for (expr, json) in [
            (
                "lib.books#$i{g: t}",
                r#"{"lib":{"books":[{"g":"a"},{"g":null}]}}"#,
            ),
            (
                "lib.books@$b{$b.g: $b.t}",
                r#"{"lib":{"books":[{"g":"a"},{"g":true}]}}"#,
            ),
        ] {
            let input = Value::from_json_str(json).expect("invalid JSON");
            let err = eval_expr(expr, &input).unwrap_err();
            assert_eq!(err.code, "T1003", "expected T1003 for {expr}");
        }
    }

    /// S0210 (one group per step) and S0209 (no predicate after group)
    /// apply to Grouped wrappers exactly as to inline groups (Go-verified).
    #[test]
    fn grouped_wrapper_keeps_parse_errors() {
        for (expr, code) in [
            (r#"(1+2){"a":1}{"b":2}"#, "S0210"),
            (r#"(1+2){"k": $}[0]"#, "S0209"),
        ] {
            let err = crate::Expression::compile(expr).expect_err("should not parse");
            assert_eq!(err.code, code, "for {expr}");
        }
    }

    // ── Wildcard ────────────────────────────────────────────────

    #[test]
    fn wildcard_object() {
        let result = eval_with_data("*", r#"{"a": 1, "b": 2}"#);
        assert_eq!(
            result,
            Value::Array(Rc::from(vec![Value::Number(1.0), Value::Number(2.0)]))
        );
    }

    // ── Arithmetic ──────────────────────────────────────────────

    #[test]
    fn addition() {
        assert_eq!(eval_simple("2 + 3"), Value::Number(5.0));
    }

    #[test]
    fn subtraction() {
        assert_eq!(eval_simple("10 - 4"), Value::Number(6.0));
    }

    #[test]
    fn multiplication() {
        assert_eq!(eval_simple("3 * 7"), Value::Number(21.0));
    }

    #[test]
    fn division() {
        assert_eq!(eval_simple("15 / 3"), Value::Number(5.0));
    }

    #[test]
    fn power() {
        assert_eq!(eval_simple("2 ** 10"), Value::Number(1024.0));
    }

    // ── Comparison ──────────────────────────────────────────────

    #[test]
    fn equals_true() {
        assert_eq!(eval_simple("1 = 1"), Value::Bool(true));
    }

    #[test]
    fn equals_false() {
        assert_eq!(eval_simple("1 = 2"), Value::Bool(false));
    }

    #[test]
    fn not_equals() {
        assert_eq!(eval_simple("1 != 2"), Value::Bool(true));
    }

    #[test]
    fn less_than() {
        assert_eq!(eval_simple("1 < 2"), Value::Bool(true));
    }

    #[test]
    fn greater_than() {
        assert_eq!(eval_simple("2 > 1"), Value::Bool(true));
    }

    // ── Boolean operators ───────────────────────────────────────

    #[test]
    fn and_true() {
        assert_eq!(eval_simple("true and true"), Value::Bool(true));
    }

    #[test]
    fn and_false() {
        assert_eq!(eval_simple("true and false"), Value::Bool(false));
    }

    #[test]
    fn or_true() {
        assert_eq!(eval_simple("false or true"), Value::Bool(true));
    }

    // ── String concatenation ────────────────────────────────────

    #[test]
    fn string_concat() {
        assert_eq!(
            eval_simple(r#""hello" & " " & "world""#),
            Value::String("hello world".into())
        );
    }

    // ── Condition ───────────────────────────────────────────────

    #[test]
    fn ternary_true() {
        assert_eq!(eval_simple("true ? 1 : 2"), Value::Number(1.0));
    }

    #[test]
    fn ternary_false() {
        assert_eq!(eval_simple("false ? 1 : 2"), Value::Number(2.0));
    }

    // ── Block ───────────────────────────────────────────────────

    #[test]
    fn block_returns_last() {
        assert_eq!(eval_simple("(1; 2; 3)"), Value::Number(3.0));
    }

    // ── Variable binding ────────────────────────────────────────

    // Note: bind tests are limited until RefCell is added for env mutation.

    // ── Array constructor ───────────────────────────────────────

    #[test]
    fn array_constructor() {
        assert_eq!(
            eval_simple("[1, 2, 3]"),
            Value::Array(Rc::from(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0)
            ]))
        );
    }

    // ── Object constructor ──────────────────────────────────────

    #[test]
    fn object_constructor() {
        let result = eval_simple(r#"{"a": 1, "b": 2}"#);
        match result {
            Value::Object(obj) => {
                assert_eq!(obj.get("a"), Some(&Value::Number(1.0)));
                assert_eq!(obj.get("b"), Some(&Value::Number(2.0)));
            }
            other => panic!("expected Object, got {:?}", other),
        }
    }

    // ── Range ───────────────────────────────────────────────────

    #[test]
    fn range_operator() {
        assert_eq!(
            eval_simple("[1..5]"),
            Value::Array(Rc::from(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(4.0),
                Value::Number(5.0),
            ]))
        );
    }

    // ── Unary minus ─────────────────────────────────────────────

    #[test]
    fn unary_minus() {
        assert_eq!(eval_simple("-5"), Value::Number(-5.0));
    }

    // ── Lambda ──────────────────────────────────────────────────

    #[test]
    fn lambda_creation() {
        let result = eval_simple("function($x){$x}");
        assert!(result.is_function());
    }

    // ── Descendant ──────────────────────────────────────────────

    #[test]
    fn descendant_lookup_test() {
        let result = eval_with_data("**", r#"{"a": {"b": 1}, "c": 2}"#);
        // The document itself, then every value recursively (jsntrs-p0v.22).
        match result {
            Value::Array(arr) => {
                assert_eq!(arr.len(), 4); // {"a":…,"c":2}, {"b":1}, 1, 2
                assert!(arr[0].is_object());
            }
            _ => panic!("expected Array, got {:?}", result),
        }
    }

    /// jsntrs-p0v.22: a bare `**` used the rootless `descendant_lookup`, so a
    /// scalar or empty-object document came back undefined instead of itself.
    #[test]
    fn descendant_includes_a_scalar_or_empty_root() {
        assert_eq!(eval_with_data("**", "5"), Value::Number(5.0));
        assert!(eval_with_data("**", "{}").is_object());
        assert_eq!(eval_with_data("**", "null"), Value::Null);
        // An array root contributes its members, not itself.
        match eval_with_data("**", "[1, 2]") {
            Value::Array(arr) => assert_eq!(arr.len(), 2),
            other => panic!("expected Array, got {other:?}"),
        }
    }

    // ── Membership ──────────────────────────────────────────────

    #[test]
    fn in_operator() {
        assert_eq!(eval_simple("2 in [1, 2, 3]"), Value::Bool(true));
    }

    #[test]
    fn not_in_operator() {
        assert_eq!(eval_simple("4 in [1, 2, 3]"), Value::Bool(false));
    }

    // ── Elvis and null-coalescing ───────────────────────────────

    #[test]
    fn elvis_defined() {
        assert_eq!(eval_simple("42 ?: 0"), Value::Number(42.0));
    }

    #[test]
    fn null_coalescing() {
        // null is a value, not undefined — ?? returns null.
        assert_eq!(eval_simple("null ?? 0"), Value::Null);
    }

    // ── Stdlib functions ────────────────────────────────────────

    #[test]
    fn stdlib_string() {
        assert_eq!(eval_simple(r#"$string(42)"#), Value::String("42".into()));
    }

    #[test]
    fn stdlib_length() {
        assert_eq!(eval_simple(r#"$length("hello")"#), Value::Number(5.0));
    }

    #[test]
    fn stdlib_uppercase() {
        assert_eq!(
            eval_simple(r#"$uppercase("hello")"#),
            Value::String("HELLO".into())
        );
    }

    #[test]
    fn stdlib_lowercase() {
        assert_eq!(
            eval_simple(r#"$lowercase("HELLO")"#),
            Value::String("hello".into())
        );
    }

    #[test]
    fn stdlib_trim() {
        assert_eq!(
            eval_simple(r#"$trim("  hello   world  ")"#),
            Value::String("hello world".into())
        );
    }

    #[test]
    fn stdlib_substring() {
        assert_eq!(
            eval_simple(r#"$substring("hello", 1, 3)"#),
            Value::String("ell".into())
        );
    }

    #[test]
    fn stdlib_contains() {
        assert_eq!(
            eval_simple(r#"$contains("hello world", "world")"#),
            Value::Bool(true)
        );
    }

    #[test]
    fn stdlib_split() {
        assert_eq!(
            eval_simple(r#"$split("a,b,c", ",")"#),
            Value::Array(Rc::from(vec![
                Value::String("a".into()),
                Value::String("b".into()),
                Value::String("c".into()),
            ]))
        );
    }

    #[test]
    fn stdlib_join() {
        assert_eq!(
            eval_simple(r#"$join(["a", "b", "c"], "-")"#),
            Value::String("a-b-c".into())
        );
    }

    #[test]
    fn stdlib_number() {
        assert_eq!(eval_simple(r#"$number("42")"#), Value::Number(42.0));
    }

    #[test]
    fn stdlib_abs() {
        assert_eq!(eval_simple("$abs(-5)"), Value::Number(5.0));
    }

    #[test]
    fn stdlib_floor() {
        assert_eq!(eval_simple("$floor(3.7)"), Value::Number(3.0));
    }

    #[test]
    fn stdlib_ceil() {
        assert_eq!(eval_simple("$ceil(3.2)"), Value::Number(4.0));
    }

    #[test]
    fn stdlib_round() {
        assert_eq!(eval_simple("$round(3.456, 2)"), Value::Number(3.46));
    }

    #[test]
    fn stdlib_sum() {
        assert_eq!(eval_simple("$sum([1, 2, 3])"), Value::Number(6.0));
    }

    #[test]
    fn stdlib_max() {
        assert_eq!(eval_simple("$max([1, 5, 3])"), Value::Number(5.0));
    }

    #[test]
    fn stdlib_min() {
        assert_eq!(eval_simple("$min([1, 5, 3])"), Value::Number(1.0));
    }

    #[test]
    fn stdlib_average() {
        assert_eq!(eval_simple("$average([2, 4, 6])"), Value::Number(4.0));
    }

    #[test]
    fn stdlib_count() {
        assert_eq!(eval_simple("$count([1, 2, 3])"), Value::Number(3.0));
    }

    #[test]
    fn stdlib_append() {
        assert_eq!(
            eval_simple("$append([1, 2], [3, 4])"),
            Value::Array(Rc::from(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(4.0),
            ]))
        );
    }

    #[test]
    fn stdlib_reverse() {
        assert_eq!(
            eval_simple("$reverse([1, 2, 3])"),
            Value::Array(Rc::from(vec![
                Value::Number(3.0),
                Value::Number(2.0),
                Value::Number(1.0),
            ]))
        );
    }

    #[test]
    fn stdlib_keys() {
        let result = eval_with_data("$keys($)", r#"{"a": 1, "b": 2}"#);
        assert_eq!(
            result,
            Value::Array(Rc::from(vec![
                Value::String("a".into()),
                Value::String("b".into()),
            ]))
        );
    }

    #[test]
    fn stdlib_values() {
        let result = eval_with_data("$values($)", r#"{"a": 1, "b": 2}"#);
        assert_eq!(
            result,
            Value::Array(Rc::from(vec![Value::Number(1.0), Value::Number(2.0)]))
        );
    }

    #[test]
    fn stdlib_boolean() {
        assert_eq!(eval_simple("$boolean(1)"), Value::Bool(true));
        assert_eq!(eval_simple("$boolean(0)"), Value::Bool(false));
        assert_eq!(eval_simple(r#"$boolean("")"#), Value::Bool(false));
    }

    #[test]
    fn stdlib_not() {
        assert_eq!(eval_simple("$not(true)"), Value::Bool(false));
        assert_eq!(eval_simple("$not(false)"), Value::Bool(true));
    }

    #[test]
    fn stdlib_exists() {
        assert_eq!(eval_simple("$exists(42)"), Value::Bool(true));
        assert_eq!(eval_simple("$exists($nothing)"), Value::Bool(false));
    }

    #[test]
    fn stdlib_type() {
        assert_eq!(eval_simple(r#"$type(42)"#), Value::String("number".into()));
        assert_eq!(
            eval_simple(r#"$type("hi")"#),
            Value::String("string".into())
        );
        assert_eq!(
            eval_simple(r#"$type(true)"#),
            Value::String("boolean".into())
        );
    }

    #[test]
    fn stdlib_map() {
        // $map returns a Sequence that collapses to Array for 3+ elements.
        let result = eval_simple("$map([1, 2, 3], function($v){$v * 2})");
        let collapsed = match result {
            Value::Sequence(seq) => seq.into_value(),
            other => other,
        };
        assert_eq!(
            collapsed,
            Value::Array(Rc::from(vec![
                Value::Number(2.0),
                Value::Number(4.0),
                Value::Number(6.0),
            ]))
        );
    }

    #[test]
    fn stdlib_filter() {
        assert_eq!(
            eval_simple("$filter([1, 2, 3, 4], function($v){$v > 2})"),
            Value::Array(Rc::from(vec![Value::Number(3.0), Value::Number(4.0)]))
        );
    }

    #[test]
    fn stdlib_reduce() {
        assert_eq!(
            eval_simple("$reduce([1, 2, 3], function($prev, $curr){$prev + $curr})"),
            Value::Number(6.0)
        );
    }

    #[test]
    fn stdlib_sort_default() {
        assert_eq!(
            eval_simple("$sort([3, 1, 2])"),
            Value::Array(Rc::from(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
            ]))
        );
    }

    #[test]
    fn stdlib_distinct() {
        assert_eq!(
            eval_simple("$distinct([1, 2, 2, 3, 1])"),
            Value::Array(Rc::from(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
            ]))
        );
    }

    #[test]
    fn stdlib_merge() {
        let result = eval_simple(r#"$merge([{"a": 1}, {"b": 2}])"#);
        match result {
            Value::Object(obj) => {
                assert_eq!(obj.get("a"), Some(&Value::Number(1.0)));
                assert_eq!(obj.get("b"), Some(&Value::Number(2.0)));
            }
            other => panic!("expected Object, got {:?}", other),
        }
    }

    #[test]
    fn stdlib_flatten() {
        assert_eq!(
            eval_simple("$flatten([[1, 2], [3, [4, 5]]])"),
            Value::Array(Rc::from(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(4.0),
                Value::Number(5.0),
            ]))
        );
    }

    #[test]
    fn stdlib_base64() {
        assert_eq!(
            eval_simple(r#"$base64encode("hello")"#),
            Value::String("aGVsbG8=".into())
        );
        assert_eq!(
            eval_simple(r#"$base64decode("aGVsbG8=")"#),
            Value::String("hello".into())
        );
    }

    // ── Sort expression ─────────────────────────────────────────

    #[test]
    fn sort_ascending() {
        let result = eval_with_data(
            "items^(value)",
            r#"{"items": [{"value": 3}, {"value": 1}, {"value": 2}]}"#,
        );
        match result {
            Value::Array(arr) => {
                assert_eq!(arr.len(), 3);
                // Check first and last.
                assert_eq!(arr[0], Value::from_json(serde_json::json!({"value": 1})));
                assert_eq!(arr[2], Value::from_json(serde_json::json!({"value": 3})));
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn sort_descending() {
        let result = eval_with_data(
            "items^(>value)",
            r#"{"items": [{"value": 1}, {"value": 3}, {"value": 2}]}"#,
        );
        match result {
            Value::Array(arr) => {
                assert_eq!(arr.len(), 3);
                assert_eq!(arr[0], Value::from_json(serde_json::json!({"value": 3})));
                assert_eq!(arr[2], Value::from_json(serde_json::json!({"value": 1})));
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    /// Regression tests for gnata-bec.3: the `^()` fast path (simple Name
    /// terms) silently sorted instead of raising T2007/T2008 like the
    /// general path.
    #[test]
    fn sort_mixed_string_number_raises_t2007() {
        let input =
            Value::from_json_str(r#"{"items": [{"value": "foo"}, {"value": 2}, {"value": 1}]}"#)
                .expect("invalid JSON");
        let err = eval_expr("items^(value)", &input).unwrap_err();
        assert_eq!(err.code, "T2007");
    }

    #[test]
    fn sort_boolean_key_raises_t2008() {
        let input =
            Value::from_json_str(r#"{"items": [{"value": true}, {"value": 2}, {"value": 1}]}"#)
                .expect("invalid JSON");
        let err = eval_expr("items^(value)", &input).unwrap_err();
        assert_eq!(err.code, "T2008");
    }

    #[test]
    fn sort_null_key_raises_t2008() {
        let input =
            Value::from_json_str(r#"{"items": [{"value": 2}, {"value": null}, {"value": 1}]}"#)
                .expect("invalid JSON");
        let err = eval_expr("items^(value)", &input).unwrap_err();
        assert_eq!(err.code, "T2008");
    }

    #[test]
    fn sort_missing_key_sorts_last_without_error() {
        let result = eval_with_data(
            "items^(value)",
            r#"{"items": [{"value": 2}, {"other": 9}, {"value": 1}]}"#,
        );
        match result {
            Value::Array(arr) => {
                assert_eq!(arr[0], Value::from_json(serde_json::json!({"value": 1})));
                assert_eq!(arr[2], Value::from_json(serde_json::json!({"other": 9})));
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    // ── Group-by expression ─────────────────────────────────────

    #[test]
    fn group_by_simple() {
        let result = eval_with_data(
            "items{color: $}",
            r#"{"items": [
                {"color": "red", "name": "a"},
                {"color": "blue", "name": "b"},
                {"color": "red", "name": "c"}
            ]}"#,
        );
        match result {
            Value::Object(obj) => {
                assert!(obj.contains_key("red"));
                assert!(obj.contains_key("blue"));
                // red group has 2 items.
                match obj.get("red") {
                    Some(Value::Array(arr)) => assert_eq!(arr.len(), 2),
                    other => panic!("expected Array for red group, got {:?}", other),
                }
            }
            other => panic!("expected Object, got {:?}", other),
        }
    }

    #[test]
    fn group_by_path() {
        // Test group-by on a path expression: Account.Order{OrderID: ...}
        let data = r#"{"Account": {"Order": [
            {"OrderID": "A", "Value": 10},
            {"OrderID": "B", "Value": 20},
            {"OrderID": "A", "Value": 30}
        ]}}"#;
        let result = eval_with_data(r#"Account.Order{OrderID: Value}"#, data);
        match &result {
            Value::Object(obj) => {
                assert!(obj.contains_key("A"), "expected key A, got {:?}", result);
                assert!(obj.contains_key("B"), "expected key B, got {:?}", result);
            }
            other => panic!("expected Object, got {:?}", other),
        }
    }

    #[test]
    fn group_by_variable() {
        // Test group-by on a $$ variable (case026)
        let result = eval_expr(r#"$${id: value}"#, &Value::from_json_str("[]").unwrap()).unwrap();
        assert_eq!(
            result,
            Value::Object(Rc::new(crate::value::ObjectMap::default()))
        );
    }

    // ── Variable binding in blocks ──────────────────────────────

    #[test]
    fn variable_binding_in_block() {
        // Test that $x := 5 works in a block context.
        assert_eq!(eval_simple("($x := 5; $x + 1)"), Value::Number(6.0));
    }

    #[test]
    fn lambda_with_recursion() {
        // Test tail-call optimized recursion.
        assert_eq!(
            eval_simple("($f := function($n){$n <= 0 ? 0 : $f($n - 1)}; $f(100))"),
            Value::Number(0.0)
        );
    }

    // ── HOF: $map ──────────────────────────────────────────────────

    #[test]
    fn map_basic() {
        assert_eq!(
            eval_simple("$map([1,2,3], function($v){$v * 2})"),
            Value::Array(Rc::from(vec![
                Value::Number(2.0),
                Value::Number(4.0),
                Value::Number(6.0)
            ]))
        );
    }

    #[test]
    fn map_undefined_input() {
        assert_undefined(&eval_simple("$map(nothing, function($v){$v})"));
    }

    #[test]
    fn map_scalar_input() {
        // Scalar is wrapped in array
        assert_eq!(
            eval_simple("$map(5, function($v){$v * 2})"),
            Value::Number(10.0)
        );
    }

    #[test]
    fn map_with_index() {
        assert_eq!(
            eval_simple("$map([10,20,30], function($v, $i){$i})"),
            Value::Array(Rc::from(vec![
                Value::Number(0.0),
                Value::Number(1.0),
                Value::Number(2.0)
            ]))
        );
    }

    #[test]
    fn map_empty_array() {
        // Map over empty array — all results are undefined, sequence collapses
        assert_undefined(&eval_simple("$map([], function($v){$v})"));
    }

    // ── HOF: $filter ───────────────────────────────────────────────

    #[test]
    fn filter_basic() {
        assert_eq!(
            eval_simple("$filter([1,2,3,4,5], function($v){$v > 3})"),
            Value::Array(Rc::from(vec![Value::Number(4.0), Value::Number(5.0)]))
        );
    }

    #[test]
    fn filter_single_result() {
        // Single match returns scalar, not array
        assert_eq!(
            eval_simple("$filter([1,2,3], function($v){$v = 2})"),
            Value::Number(2.0)
        );
    }

    #[test]
    fn filter_no_matches() {
        assert_undefined(&eval_simple("$filter([1,2,3], function($v){$v > 10})"));
    }

    #[test]
    fn filter_undefined_input() {
        assert_undefined(&eval_simple("$filter(nothing, function($v){true})"));
    }

    // ── HOF: $reduce ──────────────────────────────────────────────

    #[test]
    fn reduce_sum() {
        assert_eq!(
            eval_simple("$reduce([1,2,3,4], function($acc, $val){$acc + $val})"),
            Value::Number(10.0)
        );
    }

    #[test]
    fn reduce_with_init() {
        assert_eq!(
            eval_simple("$reduce([1,2,3], function($acc, $val){$acc + $val}, 100)"),
            Value::Number(106.0)
        );
    }

    #[test]
    fn reduce_single_element_no_init() {
        assert_eq!(
            eval_simple("$reduce([42], function($acc, $val){$acc + $val})"),
            Value::Number(42.0)
        );
    }

    #[test]
    fn reduce_empty_with_init() {
        assert_eq!(
            eval_simple("$reduce([], function($acc, $val){$acc + $val}, 99)"),
            Value::Number(99.0)
        );
    }

    #[test]
    fn reduce_undefined_input() {
        assert_undefined(&eval_simple(
            "$reduce(nothing, function($acc, $val){$acc + $val})",
        ));
    }

    // ── HOF: $sort ────────────────────────────────────────────────

    #[test]
    fn sort_default() {
        assert_eq!(
            eval_simple("$sort([3,1,2])"),
            Value::Array(Rc::from(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0)
            ]))
        );
    }

    #[test]
    fn sort_strings() {
        assert_eq!(
            eval_simple(r#"$sort(["banana","apple","cherry"])"#),
            Value::Array(Rc::from(vec![
                Value::String("apple".into()),
                Value::String("banana".into()),
                Value::String("cherry".into()),
            ]))
        );
    }

    #[test]
    fn sort_with_comparator() {
        // Comparator: fn($a,$b) returns true when $a should come after $b
        // $a > $b → ascending order (swap when a > b)
        assert_eq!(
            eval_simple("$sort([1,3,2], function($a,$b){$a > $b})"),
            Value::Array(Rc::from(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0)
            ]))
        );
    }

    #[test]
    fn sort_single_element() {
        assert_eq!(
            eval_simple("$sort([42])"),
            Value::Array(Rc::from(vec![Value::Number(42.0)]))
        );
    }

    #[test]
    fn sort_undefined() {
        assert_undefined(&eval_simple("$sort(nothing)"));
    }

    // ── HOF: $single ──────────────────────────────────────────────

    #[test]
    fn single_one_element() {
        assert_eq!(eval_simple("$single([42])"), Value::Number(42.0));
    }

    #[test]
    fn single_with_predicate() {
        assert_eq!(
            eval_simple("$single([1,2,3], function($v){$v = 2})"),
            Value::Number(2.0)
        );
    }

    #[test]
    fn single_multiple_matches_error() {
        let err = eval_expr("$single([1,2,3])", &Value::Undefined).unwrap_err();
        assert_eq!(err.code, "D3138");
    }

    #[test]
    fn single_no_matches_error() {
        let err =
            eval_expr("$single([1,2,3], function($v){$v > 10})", &Value::Undefined).unwrap_err();
        assert_eq!(err.code, "D3139");
    }

    // ── HOF: $each ────────────────────────────────────────────────

    #[test]
    fn each_basic() {
        let result = eval_with_data(
            "$each($, function($v, $k){$k & '=' & $v})",
            r#"{"a":"1","b":"2"}"#,
        );
        // Result is a sequence collapsed to array
        assert!(result.is_array());
    }

    #[test]
    fn each_undefined_input() {
        assert_undefined(&eval_simple("$each(nothing, function($v,$k){$v})"));
    }

    // ── HOF: $sift ────────────────────────────────────────────────

    #[test]
    fn sift_basic() {
        let result = eval_with_data("$sift($, function($v){$v > 1})", r#"{"a":1,"b":2,"c":3}"#);
        let obj = result.as_object().expect("should be object");
        assert_eq!(obj.len(), 2);
        assert_eq!(obj.get("b"), Some(&Value::Number(2.0)));
        assert_eq!(obj.get("c"), Some(&Value::Number(3.0)));
    }

    #[test]
    fn sift_no_matches() {
        assert_undefined(&eval_with_data(
            "$sift($, function($v){$v > 100})",
            r#"{"a":1}"#,
        ));
    }

    // ── Regex: $match ─────────────────────────────────────────────

    #[test]
    fn match_basic() {
        let result = eval_simple(r#"$match("hello world", /wo/)"#);
        let obj = result.as_object().expect("should be match object");
        assert_eq!(obj.get("match"), Some(&Value::String("wo".into())));
    }

    #[test]
    fn match_no_match() {
        assert_undefined(&eval_simple(r#"$match("hello", /xyz/)"#));
    }

    #[test]
    fn match_with_groups() {
        let result = eval_simple(r#"$match("2024-01-15", /(\d{4})-(\d{2})-(\d{2})/)"#);
        let obj = result.as_object().expect("should be match object");
        assert_eq!(obj.get("match"), Some(&Value::String("2024-01-15".into())));
        let groups = obj
            .get("groups")
            .and_then(|v| v.as_array())
            .expect("groups");
        assert_eq!(groups.len(), 3);
    }

    // ── Regex: $replace ───────────────────────────────────────────

    #[test]
    fn replace_string() {
        assert_eq!(
            eval_simple(r#"$replace("hello world", "world", "rust")"#),
            Value::String("hello rust".into())
        );
    }

    #[test]
    fn replace_regex() {
        assert_eq!(
            eval_simple(r#"$replace("hello 123 world", /\d+/, "NUM")"#),
            Value::String("hello NUM world".into())
        );
    }

    #[test]
    fn replace_with_limit() {
        assert_eq!(
            eval_simple(r#"$replace("aaa", /a/, "b", 2)"#),
            Value::String("bba".into())
        );
    }

    // ── $eval ─────────────────────────────────────────────────────

    #[test]
    fn eval_simple_expr() {
        assert_eq!(eval_simple(r#"$eval("1 + 2")"#), Value::Number(3.0));
    }

    #[test]
    fn eval_with_context() {
        assert_eq!(
            eval_with_data(r#"$eval("name")"#, r#"{"name":"test"}"#),
            Value::String("test".into())
        );
    }

    #[test]
    fn eval_undefined_input() {
        assert_undefined(&eval_simple("$eval(nothing)"));
    }

    // ── Numeric edge cases ────────────────────────────────────────

    #[test]
    fn round_bankers_rounding() {
        // JSONata uses banker's rounding (round-half-to-even)
        assert_eq!(eval_simple("$round(0.5)"), Value::Number(0.0));
        assert_eq!(eval_simple("$round(1.5)"), Value::Number(2.0));
        assert_eq!(eval_simple("$round(2.5)"), Value::Number(2.0));
        assert_eq!(eval_simple("$round(3.5)"), Value::Number(4.0));
        assert_eq!(eval_simple("$round(-0.5)"), Value::Number(0.0));
        assert_eq!(eval_simple("$round(-7.5)"), Value::Number(-8.0));
        assert_eq!(eval_simple("$round(-8.5)"), Value::Number(-8.0));
    }

    #[test]
    fn round_with_scale() {
        assert_eq!(eval_simple("$round(1.2345, 2)"), Value::Number(1.23));
        assert_eq!(eval_simple("$round(1.235, 2)"), Value::Number(1.24));
    }

    #[test]
    fn sqrt_negative_error() {
        let err = eval_expr("$sqrt(-1)", &Value::Undefined).unwrap_err();
        assert_eq!(err.code, "D3060");
    }

    #[test]
    fn sum_empty_returns_zero() {
        assert_eq!(eval_simple("$sum([])"), Value::Number(0.0));
    }

    #[test]
    fn format_base_binary() {
        assert_eq!(
            eval_simple("$formatBase(10, 2)"),
            Value::String("1010".into())
        );
    }

    #[test]
    fn format_base_hex() {
        assert_eq!(
            eval_simple("$formatBase(255, 16)"),
            Value::String("ff".into())
        );
    }

    // ── String edge cases ─────────────────────────────────────────

    #[test]
    fn length_unicode() {
        // $length counts characters, not bytes
        assert_eq!(eval_simple(r#"$length("café")"#), Value::Number(4.0));
    }

    #[test]
    fn substring_negative_start() {
        // Negative start counts from end
        assert_eq!(
            eval_simple(r#"$substring("hello", -2, 4)"#),
            Value::String("lo".into())
        );
    }

    #[test]
    fn trim_collapses_whitespace() {
        assert_eq!(
            eval_simple(r#"$trim("  hello   world  ")"#),
            Value::String("hello world".into())
        );
    }

    #[test]
    fn split_empty_separator() {
        // Empty string separator splits into characters
        assert_eq!(
            eval_simple(r#"$split("abc", "")"#),
            Value::Array(Rc::from(vec![
                Value::String("a".into()),
                Value::String("b".into()),
                Value::String("c".into()),
            ]))
        );
    }

    #[test]
    fn join_basic() {
        assert_eq!(
            eval_simple(r#"$join(["a","b","c"], "-")"#),
            Value::String("a-b-c".into())
        );
    }

    #[test]
    fn join_no_separator() {
        assert_eq!(
            eval_simple(r#"$join(["a","b","c"])"#),
            Value::String("abc".into())
        );
    }

    #[test]
    fn base64_roundtrip() {
        assert_eq!(
            eval_simple(r#"$base64decode($base64encode("hello world"))"#),
            Value::String("hello world".into())
        );
    }

    // ── Array functions ───────────────────────────────────────────

    #[test]
    fn count_array() {
        assert_eq!(eval_simple("$count([1,2,3])"), Value::Number(3.0));
    }

    #[test]
    fn count_scalar() {
        assert_eq!(eval_simple("$count(42)"), Value::Number(1.0));
    }

    #[test]
    fn count_undefined() {
        assert_eq!(eval_simple("$count(nothing)"), Value::Number(0.0));
    }

    #[test]
    fn append_arrays() {
        assert_eq!(
            eval_simple("$append([1,2], [3,4])"),
            Value::Array(Rc::from(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(4.0),
            ]))
        );
    }

    #[test]
    fn reverse_array() {
        assert_eq!(
            eval_simple("$reverse([1,2,3])"),
            Value::Array(Rc::from(vec![
                Value::Number(3.0),
                Value::Number(2.0),
                Value::Number(1.0)
            ]))
        );
    }

    #[test]
    fn distinct_removes_dupes() {
        assert_eq!(
            eval_simple("$distinct([1,2,2,3,3,3])"),
            Value::Array(Rc::from(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0)
            ]))
        );
    }

    #[test]
    fn flatten_nested() {
        assert_eq!(
            eval_simple("$flatten([[1,2],[3,[4,5]]])"),
            Value::Array(Rc::from(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
                Value::Number(4.0),
                Value::Number(5.0),
            ]))
        );
    }

    #[test]
    fn zip_basic() {
        let result = eval_simple("$zip([1,2],[3,4])");
        let arr = result.as_array().expect("should be array");
        assert_eq!(arr.len(), 2);
    }

    // ── Object functions ──────────────────────────────────────────

    #[test]
    fn keys_basic() {
        let result = eval_with_data("$keys($)", r#"{"a":1,"b":2}"#);
        let arr = result.as_array().expect("should be array");
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn values_basic() {
        let result = eval_with_data("$values($)", r#"{"a":1,"b":2}"#);
        let arr = result.as_array().expect("should be array");
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn merge_objects() {
        let result = eval_simple(r#"$merge([{"a":1},{"b":2}])"#);
        let obj = result.as_object().expect("should be object");
        assert_eq!(obj.get("a"), Some(&Value::Number(1.0)));
        assert_eq!(obj.get("b"), Some(&Value::Number(2.0)));
    }

    #[test]
    fn lookup_basic() {
        assert_eq!(
            eval_with_data(r#"$lookup($, "a")"#, r#"{"a":42}"#),
            Value::Number(42.0)
        );
    }

    #[test]
    fn lookup_missing() {
        assert_undefined(&eval_with_data(r#"$lookup($, "z")"#, r#"{"a":42}"#));
    }

    // ── Boolean/type functions ────────────────────────────────────

    #[test]
    fn boolean_coercion() {
        assert_eq!(eval_simple("$boolean(0)"), Value::Bool(false));
        assert_eq!(eval_simple("$boolean(1)"), Value::Bool(true));
        assert_eq!(eval_simple(r#"$boolean("")"#), Value::Bool(false));
        assert_eq!(eval_simple(r#"$boolean("0")"#), Value::Bool(true)); // "0" is truthy!
        assert_eq!(eval_simple("$boolean(null)"), Value::Bool(false));
        assert_eq!(eval_simple("$boolean(false)"), Value::Bool(false));
    }

    #[test]
    fn not_function() {
        assert_eq!(eval_simple("$not(true)"), Value::Bool(false));
        assert_eq!(eval_simple("$not(false)"), Value::Bool(true));
    }

    #[test]
    fn exists_function() {
        assert_eq!(eval_simple("$exists(42)"), Value::Bool(true));
        assert_eq!(eval_simple("$exists(nothing)"), Value::Bool(false));
        assert_eq!(eval_simple("$exists(null)"), Value::Bool(true));
    }

    #[test]
    fn type_function() {
        assert_eq!(eval_simple(r#"$type(42)"#), Value::String("number".into()));
        assert_eq!(
            eval_simple(r#"$type("hi")"#),
            Value::String("string".into())
        );
        assert_eq!(
            eval_simple(r#"$type(true)"#),
            Value::String("boolean".into())
        );
        assert_eq!(eval_simple(r#"$type(null)"#), Value::String("null".into()));
        assert_eq!(eval_simple(r#"$type([1])"#), Value::String("array".into()));
    }

    // ── Error function ────────────────────────────────────────────

    #[test]
    fn error_function() {
        let err = eval_expr(r#"$error("custom error")"#, &Value::Undefined).unwrap_err();
        assert_eq!(err.code, "D3137");
        assert!(err.message.contains("custom error"));
    }

    // ── Tail position + group-by (jsntrs-5lw.1) ───────────────────

    /// A call in tail position with a group-by attached must not be
    /// thunked: `eval_group_by` calls `eval_function` directly and has no
    /// trampoline, so the `TailCall` sentinel used to be grouped as an
    /// ordinary item and serialized as `""`. The group applies to the
    /// call's real result, exactly as it does outside tail position.
    ///
    /// Deliberate deviation from jsonata-js 2.x, whose trampoline drops
    /// the group entirely (`{"v":1}` here) and can even leak its internal
    /// lambda object into the output; jsntrs already applies the group for
    /// builtin callees in the same position.
    #[test]
    fn tail_position_call_with_group_applies_the_group() {
        let f = r#"$f := function($x){{"v": $x}}; "#;
        // Plain tail position.
        assert_eq!(
            eval_simple(&format!(
                r#"({f}$g := function($n){{ $f($n){{"k": $}} }}; $g(1))"#
            )),
            eval_simple(r#"{"k": {"v": 1}}"#)
        );
        // Tail position reached through a conditional branch.
        assert_eq!(
            eval_simple(&format!(
                r#"({f}$g := function($n){{ $n > 0 ? $f($n){{"k": $}} : "none" }}; $g(1))"#
            )),
            eval_simple(r#"{"k": {"v": 1}}"#)
        );
        // Through a HOF, which calls the lambda via call_function.
        assert_eq!(
            eval_simple(&format!(
                r#"({f}$g := function($n){{ $f($n){{"k": $}} }}; $map([1,2], $g))"#
            )),
            eval_simple(r#"[{"k": {"v": 1}}, {"k": {"v": 2}}]"#)
        );
        // An ungrouped tail call is still thunked, so deep recursion runs
        // on the trampoline rather than the stack.
        assert_eq!(
            eval_simple(
                r"($f := function($n, $acc){ $n <= 0 ? $acc : $f($n - 1, $acc + $n) }; $f(5000, 0))"
            ),
            Value::Number(12_502_500.0)
        );
    }

    // ── Mid-path group-by hoisting (jsntrs-5lw.3) ─────────────────

    /// `{` binds looser than `.`, so `a.b{"k": $}.c` parses with the group
    /// on an inner dot node, where path flattening used to drop it (the
    /// expression evaluated as a plain `a.b.c`). The group hoists onto the
    /// flattened path and applies once, to the whole path's result — all
    /// expectations jsonata-js 2.x-verified.
    #[test]
    fn group_by_on_a_mid_path_dot_applies_to_the_whole_path() {
        let d1 = r#"{"a": {"b": {"c": 7}}}"#;
        let d2 = r#"{"a": [{"b": {"c": 1}}, {"b": {"c": 2}}]}"#;
        assert_eq!(
            eval_with_data(r#"a.b{"k": $}.c"#, d1),
            eval_simple(r#"{"k": 7}"#)
        );
        // Grouping happens after the last step, so all items land on "k".
        assert_eq!(
            eval_with_data(r#"a.b{"k": $}.c"#, d2),
            eval_simple(r#"{"k": [1, 2]}"#)
        );
        // A predicate on a later step still runs before the group.
        assert_eq!(
            eval_with_data(r#"a.b{"k": $}.c[0]"#, d2),
            eval_simple(r#"{"k": [1, 2]}"#)
        );
        // The grouped object is what the path yields.
        assert_eq!(
            eval_with_data(r#"(a.b{"k": $}.c).k"#, d1),
            Value::Number(7.0)
        );
        // A trailing group on the whole chain is unchanged.
        assert_eq!(
            eval_with_data(r#"a.b.c{"k": $}"#, d1),
            eval_simple(r#"{"k": 7}"#)
        );
        // Two groups in one chain are S0210, wherever they sit.
        for expr in [r#"a.b{"k": $}.c{"j": $}"#, r#"a.b{"k": $}.c.d{"j": $}"#] {
            let err = crate::Expression::compile(expr).expect_err("should not compile");
            assert_eq!(err.code, "S0210", "for {expr}");
        }
    }
}
