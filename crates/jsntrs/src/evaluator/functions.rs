//! Function types and call machinery for JSONata evaluation.
//!
//! Port of Go `internal/evaluator/eval_function.go` and `env.go` function types.

// Reachable only through #[doc(hidden)] re-exports for in-repo tooling;
// not part of the documented public API.
#![expect(missing_docs)]

use std::fmt;
use std::rc::Rc;

use crate::error::{JsonataError, JsonataResult};
use crate::parser::{AstArena, Expr, NodeId};
use crate::value::Value;

use super::environment::Environment;
use super::eval_no_stack_check;

/// A native Rust function implementing a JSONata built-in.
/// `args` are the evaluated arguments; `focus` is the current context value.
pub type BuiltinFn = dyn Fn(&[Value], &Value) -> JsonataResult;

/// An environment-aware built-in. Required for HOFs ($map, $filter, etc.)
/// and functions that create child scopes ($eval).
pub type EnvAwareBuiltinFn = dyn Fn(&[Value], &Value, &Rc<Environment>, &AstArena) -> JsonataResult;

/// Callable function value in JSONata.
#[derive(Clone)]
pub enum FunctionValue {
    /// Native built-in function.
    Builtin(Rc<BuiltinFn>),

    /// Environment-aware built-in (HOFs, $eval).
    EnvAwareBuiltin(Rc<EnvAwareBuiltinFn>),

    /// Built-in with type signature for arity/type validation.
    /// The signature is parsed once at registration, not per call.
    SignedBuiltin {
        func: Rc<BuiltinFn>,
        signature: std::sync::Arc<[super::ParamSpec]>,
    },

    /// User-defined lambda (function expression).
    Lambda(Rc<Lambda>),

    /// Partial application (pre-bound args with placeholder slots).
    Partial(Rc<BuiltinFn>),
}

impl fmt::Debug for FunctionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FunctionValue::Builtin(_) => write!(f, "Builtin(<fn>)"),
            FunctionValue::EnvAwareBuiltin(_) => write!(f, "EnvAwareBuiltin(<fn>)"),
            FunctionValue::SignedBuiltin { signature, .. } => {
                write!(f, "SignedBuiltin({} params)", signature.len())
            }
            FunctionValue::Lambda(l) => write!(f, "Lambda({:?})", l.params),
            FunctionValue::Partial(_) => write!(f, "Partial(<fn>)"),
        }
    }
}

/// User-defined function (lambda expression).
#[derive(Debug)]
pub struct Lambda {
    pub params: Vec<String>,
    pub body: NodeId,
    pub closure: Rc<Environment>,
    pub thunk: bool,
    /// Parsed at compile time; `None` for an untyped lambda.
    pub signature: Option<std::sync::Arc<[super::ParamSpec]>>,
    pub captured_focus: Value,
    /// Does the body hand a callee's result straight back? See
    /// [`body_is_tail_call`].
    pub tail_call_body: bool,
}

/// Is the lambda body a call in tail position — i.e. does the call's result
/// become the lambda's result untouched?
///
/// The reference implementation replaces such a body with a thunk that
/// `apply()`'s trampoline invokes *outside* `evaluate()`, so whatever the
/// callee produced — an uncollapsed sequence included — comes back raw. Any
/// other body goes through `evaluate()`, which collapses. jsntrs marks the
/// very same positions in `mark_tail_calls`; this walk mirrors that marking
/// arm for arm. A `:=` right-hand side is deliberately absent from both:
/// the reference does not treat it as a tail position, so
/// `function($x){ $z := $keys($x) }` collapses there (jsntrs-p0v.15).
fn body_is_tail_call(arena: &AstArena, node: NodeId) -> bool {
    if node.is_empty() {
        return false;
    }
    match arena.get(node) {
        Expr::Function { group, .. } => group.is_none(),
        Expr::Condition { then, else_, .. } => {
            body_is_tail_call(arena, *then) || else_.is_some_and(|e| body_is_tail_call(arena, e))
        }
        Expr::Block { expressions, .. } => expressions
            .last()
            .is_some_and(|&last| body_is_tail_call(arena, last)),
        _ => false,
    }
}

/// Tail-call sentinel returned by thunked function calls.
#[derive(Clone)]
pub struct TailCall {
    pub func: FunctionValue,
    pub args: Vec<Value>,
    /// Call-site name of the tail-called procedure, for error attribution.
    ///
    /// The reference's trampoline stamps it onto the procedure it is about
    /// to apply (`next.token = result.body.procedure.value`, jsonata 2.2.2
    /// `jsonata.js:4974`) so a failure inside the tail call is attributed to
    /// the *callee*, not to the frame that trampolined it: in
    /// `( $A := function(){$B()}; $B := function(){1 ~> 2}; $A() )` the
    /// T2006 carries token `"B"`. Empty when the callee is not a plain
    /// `$name` — the reference guards the assignment on
    /// `procedure.type === 'variable'`.
    pub token: compact_str::CompactString,
}

impl fmt::Debug for TailCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TailCall({:?}, {} args)", self.func, self.args.len())
    }
}

/// The name a call site invokes a function by, for error attribution.
///
/// The reference computes `expr.procedure.type === 'path' ?
/// expr.procedure.steps[0].value : expr.procedure.value` (jsonata 2.2.2
/// `jsonata.js:4935`) and attaches it to anything the invocation throws
/// that is not already attributed. Nodes `processAST` rebuilds without a
/// `value` — lambda literals and blocks — yield nothing, which is why
/// `(function($x)<n>{$x})('a')` reports T0410 with no token at all.
pub(super) fn call_site_name(arena: &AstArena, procedure: NodeId) -> &str {
    match arena.get(procedure) {
        Expr::Variable { name, .. } => name.as_str(),
        Expr::Name { value, .. } => value.as_str(),
        Expr::Path { steps, .. } => match steps.first().map(|&s| arena.get(s)) {
            Some(Expr::Variable { name, .. }) => name.as_str(),
            Some(Expr::Name { value, .. }) => value.as_str(),
            _ => "",
        },
        _ => "",
    }
}

/// Evaluate a function call node.
///
/// # Errors
/// Returns JSONata errors for undefined functions, type mismatches, or evaluation failures.
pub fn eval_function(
    arena: &AstArena,
    node: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    let (procedure, arguments, thunk, keep_array) = match arena.get(node) {
        Expr::Function {
            procedure,
            arguments,
            thunk,
            keep_array,
            ..
        } => (*procedure, arguments.clone(), *thunk, *keep_array),
        _ => unreachable!("eval_function called on non-Function node"),
    };

    // Resolve the function value.
    // When % is used as a function callee (e.g., %(1)), the parent context
    // error S0217 should become T1006 (not a function).
    let fn_val = match eval_no_stack_check(arena, procedure, input, env) {
        Ok(v) => v,
        Err(e) if e.code == "S0217" => Value::Undefined,
        Err(e) => return Err(e),
    };

    // The name this call site invokes the function by; every error the
    // invocation itself raises is attributed to it unless something nearer
    // already named a token (see [`call_site_name`]).
    let name = call_site_name(arena, procedure);

    let func = match fn_val {
        Value::Function(f) => f,
        Value::Undefined => {
            // Check if this is a Name procedure that exists in env as a function
            // but was accessed without $ → T1005.
            if let Expr::Name { value, .. } = arena.get(procedure)
                && let Some(Value::Function(_)) = env.lookup(value)
            {
                return Err(JsonataError::new(
                    "T1005",
                    format!("attempted to invoke a function that has no definition: {value}"),
                )
                .with_token(value.clone()));
            }
            return Err(
                JsonataError::new("T1006", "attempted to invoke undefined function").or_token(name),
            );
        }
        _ => {
            return Err(JsonataError::new("T1006", "not a function".to_string()).or_token(name));
        }
    };

    // Evaluate arguments. An argument is a consumer position: a sequence
    // collapses here, exactly as it would at the tail of the reference
    // implementation's `evaluate()` for the argument expression.
    let mut args = Vec::with_capacity(arguments.len());
    for &arg_node in &arguments {
        if matches!(arena.get(arg_node), Expr::Placeholder { .. }) {
            args.push(Value::Undefined);
            continue;
        }
        let val = super::eval_operand(arena, arg_node, input, env)?;
        args.push(val);
    }

    // Signature validation for SignedBuiltins at direct call site.
    // HOF callbacks bypass this (they go through call_function instead).
    if let FunctionValue::SignedBuiltin { signature, .. } = &*func {
        let (coerced, return_undefined) =
            super::process_call_args(signature, &args).map_err(|e| e.or_token(name))?;
        if return_undefined {
            return Ok(Value::Undefined);
        }
        if let Some(coerced) = coerced {
            args = coerced;
        }
    }

    // Tail-call optimization: if this call is in tail position within a
    // lambda body, return a TailCall sentinel instead of recursing. The
    // call-site name rides along so the trampoline that eventually applies
    // it can attribute failures to this callee.
    if thunk && let FunctionValue::Lambda(_) = &*func {
        return Ok(Value::TailCall(Box::new(TailCall {
            func: *func,
            args,
            token: name.into(),
        })));
    }

    let result = call_function(&func, &args, input, env, arena).map_err(|e| e.or_token(name))?;
    if keep_array && call_result_is_sequence(&result) {
        Ok(super::mark_keep_singleton(result))
    } else {
        Ok(result)
    }
}

/// Does a call's result stand in for a JSONata *sequence*?
///
/// The `[]` (keep-array) postfix only ever applies to sequences: the
/// reference implementation guards `expr.keepArray` with `isSequence(result)`
/// in `evaluate()`, so `$sum(x)[]` stays the scalar `60` while
/// `$map(x, fn)[]` keeps its singleton wrapped as `[y]` (jsntrs-e8l).
///
/// Every builtin the reference builds with `createSequence()` now returns an
/// uncollapsed [`Value::Sequence`] in jsntrs too, so the answer is simply
/// whether the call handed one back — no per-function identity table is
/// needed any more (jsntrs-p0v.6). A lambda qualifies exactly when its own
/// body was a tail-position call that returned a sequence, which is what the
/// reference's trampoline does; a body that goes through `evaluate()` has
/// already collapsed and cannot be re-wrapped.
pub(super) fn call_result_is_sequence(result: &Value) -> bool {
    matches!(result, Value::Sequence(_))
}

/// Evaluate a lambda expression node, creating a closure.
pub fn eval_lambda(arena: &AstArena, node: NodeId, input: &Value, env: &Rc<Environment>) -> Value {
    let (params, body, sig, thunk) = match arena.get(node) {
        Expr::Lambda {
            params,
            body,
            signature,
            thunk,
            ..
        } => (
            params.clone(),
            *body,
            signature.as_ref().map(|s| std::sync::Arc::clone(&s.params)),
            *thunk,
        ),
        _ => unreachable!("eval_lambda called on non-Lambda node"),
    };

    let param_names: Vec<String> = params
        .iter()
        .map(|&p| match arena.get(p) {
            Expr::Variable { name, .. } => name.clone(),
            _ => String::new(),
        })
        .collect();

    // The closure capture below can form an Rc cycle once the lambda is
    // bound into `env`'s scope chain; the API boundary breaks survivors.
    env.note_closure_env(env);
    Value::Function(Box::new(FunctionValue::Lambda(Rc::new(Lambda {
        params: param_names,
        tail_call_body: body_is_tail_call(arena, body),
        body,
        closure: Rc::clone(env),
        thunk,
        signature: sig,
        captured_focus: input.clone(),
    }))))
}

/// Evaluate a partial application node.
///
/// # Errors
/// Returns `T1007` or `T1008` for invalid partial application targets.
pub fn eval_partial(
    arena: &AstArena,
    node: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    let (procedure, arguments) = match arena.get(node) {
        Expr::Partial {
            procedure,
            arguments,
            ..
        } => (*procedure, arguments.clone()),
        _ => unreachable!("eval_partial called on non-Partial node"),
    };

    let fn_val = eval_no_stack_check(arena, procedure, input, env)?;
    // Same attribution rule as a full invocation (jsonata 2.2.2
    // `jsonata.js:5103` and `:5117` both name the call site).
    let name = call_site_name(arena, procedure);
    let func = match fn_val {
        Value::Function(f) => f,
        Value::Undefined => {
            // Distinguish T1007 vs T1008 based on whether the name exists in env.
            if let Expr::Name { value, .. } = arena.get(procedure) {
                if env.lookup(value).is_some() {
                    return Err(JsonataError::new(
                        "T1007",
                        "attempted to partially apply a function referenced without $",
                    )
                    .with_token(value.clone()));
                }
                return Err(JsonataError::new(
                    "T1008",
                    "cannot partially apply a non-function: the function is not defined",
                )
                .with_token(value.clone()));
            }
            // Not a bare `Name` step, so the callee was already written with
            // its `$`: T1007 is the "you forgot the $" variant and cannot
            // apply here. The documentation's own run-time-error example is
            // this shape for a full invocation — `$notafunction()` yields
            // `code: "T1006"`, the *generic* code, with `token:
            // "notafunction"` (docs.jsonata.org, Embedding and Extending
            // JSONata, "expression.evaluate"). T1007/T1008 are the partial-
            // application analogues of T1005/T1006, so this branch owes the
            // generic one.
            return Err(JsonataError::new(
                "T1008",
                "attempted to partially apply an undefined function",
            )
            .or_token(name));
        }
        _ => {
            return Err(
                JsonataError::new("T1008", "cannot partially apply a non-function").or_token(name),
            );
        }
    };

    // Evaluate bound args, tracking placeholders.
    let mut bound_args = Vec::with_capacity(arguments.len());
    let mut is_placeholder = Vec::with_capacity(arguments.len());
    for &arg_node in &arguments {
        if matches!(arena.get(arg_node), Expr::Placeholder { .. }) {
            is_placeholder.push(true);
            bound_args.push(Value::Undefined);
        } else {
            is_placeholder.push(false);
            let val = super::eval_operand(arena, arg_node, input, env)?;
            bound_args.push(val);
        }
    }

    let env_clone = Rc::clone(env);
    let partial_fn: Rc<super::EnvAwareBuiltinFn> = Rc::new(
        move |args: &[Value],
              focus: &Value,
              _env: &Rc<super::Environment>,
              arena: &crate::parser::AstArena| {
            let mut full_args = bound_args.clone();
            let mut arg_idx = 0;
            for (i, &placeholder) in is_placeholder.iter().enumerate() {
                if placeholder && arg_idx < args.len() {
                    full_args[i] = args[arg_idx].clone();
                    arg_idx += 1;
                }
            }
            super::call_function(&func, &full_args, focus, &env_clone, arena)
        },
    );

    Ok(Value::Function(Box::new(FunctionValue::EnvAwareBuiltin(
        partial_fn,
    ))))
}

/// Trampoline iteration budget per unit of configured recursion depth.
///
/// A tail-call chain may bounce `counter.max * this` times before the
/// trampoline reports `U1001`, matching the reference implementation's
/// cap of max iterations = depth * 10000.
const TAIL_CALL_ITERATIONS_PER_DEPTH: usize = 10_000;

/// Call a function value with arguments. Contains the trampoline loop for TCO.
///
/// # Errors
/// Returns `U1001` on stack overflow, `D3001` on cancellation, or any error from the callee.
pub fn call_function(
    func: &FunctionValue,
    args: &[Value],
    focus: &Value,
    env: &Rc<Environment>,
    arena: &AstArena,
) -> JsonataResult {
    // Short-circuit builtins: they never produce TailCall, so skip the
    // trampoline setup (avoids cloning func + args.to_vec()).
    match func {
        FunctionValue::SignedBuiltin { func: f, .. } => return f(args, focus),
        FunctionValue::Builtin(f) | FunctionValue::Partial(f) => return f(args, focus),
        FunctionValue::EnvAwareBuiltin(f) => return f(args, focus, env, arena),
        FunctionValue::Lambda(_) => {}
    }

    let counter = env.call_counter();
    let max_iter = counter.max as usize * TAIL_CALL_ITERATIONS_PER_DEPTH;
    let mut current_func = func.clone();
    let mut current_args: Vec<Value> = args.to_vec();
    let mut iter = 0;
    // Attribution for the frame currently being applied. Empty on the first
    // pass — the caller (`eval_function`) owns that name — and replaced on
    // every bounce by the name the tail call was written with, mirroring
    // `next.token = result.body.procedure.value` in the reference's
    // trampoline (jsonata 2.2.2 `jsonata.js:4974`).
    let mut current_token = compact_str::CompactString::default();

    loop {
        if env.is_cancelled() {
            return Err(JsonataError::new("D3001", "evaluation cancelled"));
        }

        match &current_func {
            FunctionValue::SignedBuiltin { func: f, .. } => {
                return f(&current_args, focus).map_err(|e| e.or_token(&current_token));
            }
            FunctionValue::Builtin(f) | FunctionValue::Partial(f) => {
                return f(&current_args, focus).map_err(|e| e.or_token(&current_token));
            }
            FunctionValue::EnvAwareBuiltin(f) => {
                return f(&current_args, focus, env, arena).map_err(|e| e.or_token(&current_token));
            }
            FunctionValue::Lambda(lambda) => {
                // Lambda signature validation.
                if let Some(specs) = &lambda.signature {
                    let (coerced, return_undefined) =
                        super::process_call_args(specs, &current_args)
                            .map_err(|e| e.or_token(&current_token))?;
                    if return_undefined {
                        return Ok(Value::Undefined);
                    }
                    if let Some(coerced) = coerced {
                        current_args = coerced;
                    }
                }

                let depth = counter.depth.get() + 1;
                if depth > counter.max {
                    return Err(JsonataError::new(
                        "U1001",
                        format!(
                            "stack overflow error: evaluation exceeded stack depth {}",
                            counter.max
                        ),
                    )
                    .or_token(&current_token));
                }
                counter.depth.set(depth);

                let child_env = {
                    let ce = Environment::new_child(Rc::clone(&lambda.closure));
                    for (i, param) in lambda.params.iter().enumerate() {
                        let val = current_args.get(i).cloned().unwrap_or(Value::Undefined);
                        ce.bind(param.clone(), val);
                    }
                    Rc::new(ce)
                };

                // Zero-param closures use captured focus.
                let body_focus = if lambda.params.is_empty() && current_args.is_empty() {
                    &lambda.captured_focus
                } else {
                    focus
                };

                let result =
                    super::eval_with_stack_check(arena, lambda.body, body_focus, &child_env);
                counter.depth.set(depth - 1);

                match result {
                    Ok(Value::TailCall(tc)) => {
                        iter += 1;
                        if iter > max_iter {
                            return Err(JsonataError::new(
                                "U1001",
                                format!(
                                    "stack overflow error: evaluation exceeded stack depth {}",
                                    counter.max
                                ),
                            )
                            .or_token(&current_token));
                        }
                        current_func = tc.func;
                        current_args = tc.args;
                        current_token = tc.token;
                    }
                    // The body is a syntactic position: the reference
                    // evaluates it with `evaluate()`, so a sequence has
                    // already collapsed by the time `applyProcedure`
                    // returns — unless the body is a tail-position call,
                    // whose result the trampoline hands back raw
                    // (jsntrs-p0v.6).
                    other if lambda.tail_call_body => {
                        return other.map_err(|e| e.or_token(&current_token));
                    }
                    other => {
                        return other
                            .map(super::collapse_sequence)
                            .map_err(|e| e.or_token(&current_token));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Expression;

    /// The `[]` postfix on a call to a lambda must resolve identically in
    /// tail and non-tail position. The thunked call returns a `TailCall`
    /// through the trampoline, which never sees the postfix, so the direct
    /// path had to stop honouring it too — which is also what the reference
    /// does, since a lambda result is never a sequence (jsntrs-5lw.2).
    #[test]
    fn keep_array_on_a_lambda_call_is_position_independent() {
        // (direct form, same call in tail position inside another lambda)
        let pairs = [
            (
                "($id := function($x) { $x }; $id(5)[])",
                "($id := function($x) { $x }; $g := function($n) { $id($n)[] }; $g(5))",
            ),
            (
                "($id := function($x) { $x }; $id([1, 2])[])",
                "($id := function($x) { $x }; $g := function($n) { $id($n)[] }; $g([1, 2]))",
            ),
            (
                "($id := function($x) { $x }; $id(\"s\")[])",
                "($id := function($x) { $x }; $g := function($n) { $id($n)[] }; $g(\"s\"))",
            ),
            (
                "($id := function($x) { $x }; $id({\"a\": 1})[])",
                "($id := function($x) { $x }; $g := function($n) { $id($n)[] }; $g({\"a\": 1}))",
            ),
        ];
        for (direct, tail) in pairs {
            let d = Expression::compile(direct).unwrap().evaluate("{}").unwrap();
            let t = Expression::compile(tail).unwrap().evaluate("{}").unwrap();
            assert_eq!(d, t, "tail/non-tail mismatch:\n  {direct}\n  {tail}");
        }
    }

    /// A lambda body that is a tail-position call hands the callee's
    /// sequence back raw — the HOF then embeds it as a nested array — while
    /// any other body has already collapsed. The two `$keys` bodies below
    /// differ only in whether the call is in tail position, and that is the
    /// whole rule (jsntrs-p0v.6).
    #[test]
    fn a_tail_position_call_body_returns_the_callee_sequence_raw() {
        let cases = [
            // Tail-position call → raw sequence → nested per item.
            (
                r#"$map([{"a": 1}, {"b": 2}], function($x) { $keys($x) })"#,
                r#"[["a"], ["b"]]"#,
            ),
            // Same call wrapped by another expression → collapsed body.
            (
                r#"$map([{"a": 1}, {"b": 2}], function($x) { [$keys($x)] })"#,
                r#"[["a"], ["b"]]"#,
            ),
            // A path body is never a tail call → collapsed.
            (
                r#"$map([[{"a": 1}], [{"a": 2}, {"a": 3}]], function($x) { $x.a })"#,
                "[1, [2, 3]]",
            ),
            // A bind body is not a tail position in the reference either.
            (
                r#"$map([{"a": 1}, {"b": 2}], function($x) { $z := $keys($x) })"#,
                r#"["a", "b"]"#,
            ),
        ];
        for (expr, expected) in cases {
            let actual = Expression::compile(expr).unwrap().evaluate("{}").unwrap();
            let want = Expression::compile(expected)
                .unwrap()
                .evaluate("{}")
                .unwrap();
            assert_eq!(actual, want, "{expr}");
        }
    }

    /// A sequence must never reach a builtin: every argument position
    /// collapses it first (jsntrs-p0v.6).
    #[test]
    fn a_sequence_argument_collapses_before_the_callee_sees_it() {
        let cases = [
            ("$count($map([1, 2], function($x) { $x }))", "2"),
            ("$type($map([1], function($x) { $x }))", "\"number\""),
            (r#"$count($keys({"a": 1, "b": 2}))"#, "2"),
            ("$map([1], function($x) { $x }) = 1", "true"),
            ("$map([1], function($x) { $x }) + 1", "2"),
            ("$map([1, 2], function($x) { $x }) ~> $count()", "2"),
        ];
        for (expr, expected) in cases {
            let actual = Expression::compile(expr).unwrap().evaluate("{}").unwrap();
            let want = Expression::compile(expected)
                .unwrap()
                .evaluate("{}")
                .unwrap();
            assert_eq!(actual, want, "{expr}");
        }
    }
}
