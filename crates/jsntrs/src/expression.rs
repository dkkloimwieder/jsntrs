//! Compiled JSONata expression with fast-path optimization.
//!
//! The `Expression` struct wraps a parsed + processed AST with optional
//! fast-path metadata for common expression patterns. This is the
//! recommended public API for evaluating JSONata expressions.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::error::JsonataResult;
use crate::evaluator::{Environment, FunctionValue};
use crate::fast_path::{self, FastPath};
use crate::parser::{AstArena, NodeId, Parser, process_ast};
use crate::value::Value;

/// A user-defined function that extends the standard JSONata library.
///
/// Receives evaluated arguments and the current context value (focus).
/// Must be `Send + Sync` for use with thread-safe `Expression` and `StreamEvaluator`.
///
/// Function names are registered without the leading `$` — users call them as
/// `$functionName()` in expressions.
pub type CustomFunc = Arc<dyn Fn(&[Value], &Value) -> JsonataResult + Send + Sync>;

/// Create a root environment with all standard library functions plus custom functions.
///
/// The returned environment can be reused across multiple evaluations via
/// [`Expression::evaluate_with_env`]. Reusing the environment avoids re-registering
/// stdlib on every call.
pub fn new_custom_env(custom_funcs: &[(String, CustomFunc)]) -> Rc<Environment> {
    let mut env = Environment::new();
    crate::stdlib::register_all(&mut env);
    for (name, func) in custom_funcs {
        let arc_fn = Arc::clone(func);
        let builtin: Rc<crate::evaluator::BuiltinFn> =
            Rc::new(move |args: &[Value], focus: &Value| arc_fn(args, focus));
        env.bind(
            name.clone(),
            Value::Function(Box::new(FunctionValue::Builtin(builtin))),
        );
    }
    Rc::new(env)
}

/// A compiled JSONata expression, ready for evaluation.
///
/// Use [`Expression::compile`] to parse and optimize, then [`Expression::evaluate`]
/// to run against input data. The compiled form can be reused across multiple inputs.
///
/// `Expression` is `Send + Sync`: the AST is wrapped in `Arc` so the same
/// compiled expression can be shared across threads. Per-evaluation state
/// (`Environment`, `Value`) is created on the calling thread and never escapes.
///
/// Cloning is cheap: the AST is shared via `Arc`, so a clone copies only the
/// fast-path metadata and the source string, never the parsed tree.
#[derive(Clone)]
pub struct Expression {
    arena: Arc<AstArena>,
    root: NodeId,
    fast_path: FastPath,
    source: String,
}

// Compile-time assertion: Expression must be Send + Sync.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Expression>();
};

impl Expression {
    /// Compile a JSONata expression string.
    ///
    /// Parses, post-processes the AST, and analyzes for fast-path optimization.
    ///
    /// # Errors
    /// Returns parse or AST processing errors.
    pub fn compile(expr: &str) -> JsonataResult<Self> {
        let (mut arena, root) = Parser::parse(expr)?;
        let root = process_ast(&mut arena, root)?;
        let fast_path = fast_path::analyze(&arena, root);

        Ok(Self {
            arena: Arc::new(arena),
            root,
            fast_path,
            source: expr.to_string(),
        })
    }

    /// Evaluate this expression against a JSON string.
    ///
    /// Automatically selects the fastest evaluation path:
    /// - Pure dotted paths (`a.b.c`) use simd-json's tape — no Value tree built
    /// - All other expressions parse to Value, then use the full evaluator
    ///
    /// This is the primary API. Use [`Self::evaluate_value`] if you already have a
    /// parsed `Value` (e.g., shared across multiple expression evaluations).
    ///
    /// # Errors
    /// Returns JSON parse errors or JSONata evaluation errors.
    pub fn evaluate(&self, json: &str) -> JsonataResult {
        if Self::input_is_absent(json.as_bytes()) {
            return self.evaluate_value(&Value::Undefined);
        }

        // Try tape-based evaluation for pure paths (no Value tree).
        if let Some(result) = fast_path::eval_tape_path(&self.fast_path, json.as_bytes()) {
            return result.map_err(|e| {
                crate::error::JsonataError::new("D0000", format!("JSON parse error: {e}"))
            });
        }

        // Parse to Value, then evaluate.
        let input = Self::parse_input(json)?;
        self.evaluate_value(&input)
    }

    /// Evaluate this expression against a pre-parsed Value.
    ///
    /// Use this when you already have a `Value` — e.g., when evaluating
    /// multiple expressions against the same input (parse once, eval many).
    ///
    /// # Errors
    /// Returns JSONata evaluation errors.
    pub fn evaluate_value(&self, input: &Value) -> JsonataResult {
        if let Some(result) = fast_path::eval_fast(&self.fast_path, input) {
            return Ok(result);
        }

        let env = Rc::new(Environment::new_eval_child(crate::stdlib::cached_root_env()));
        if !input.is_undefined() {
            env.bind("$", input.clone());
        }
        let result = crate::evaluator::eval(&self.arena, self.root, input, &env);
        env.teardown_cycles();
        result
    }

    /// Evaluate against raw bytes. Equivalent to [`Self::evaluate`] but takes `&[u8]`.
    ///
    /// # Errors
    /// Returns JSON parse errors or JSONata evaluation errors.
    pub fn evaluate_bytes(&self, json_bytes: &[u8]) -> JsonataResult {
        if Self::input_is_absent(json_bytes) {
            return self.evaluate_value(&Value::Undefined);
        }

        if let Some(result) = fast_path::eval_tape_path(&self.fast_path, json_bytes) {
            return result.map_err(|e| {
                crate::error::JsonataError::new("D0000", format!("JSON parse error: {e}"))
            });
        }

        let json = std::str::from_utf8(json_bytes)
            .map_err(|e| crate::error::JsonataError::new("D0000", format!("invalid UTF-8: {e}")))?;
        let input = Self::parse_input(json)?;
        self.evaluate_value(&input)
    }

    /// Evaluate with user-defined custom functions.
    ///
    /// Binds the custom functions into a per-evaluation scope over the
    /// thread-cached stdlib environment. For repeated evaluations with the
    /// same custom functions, prefer [`new_custom_env`] +
    /// [`Expression::evaluate_with_env`] to avoid re-wrapping the functions
    /// on every call.
    ///
    /// # Errors
    /// Returns JSONata evaluation errors.
    pub fn evaluate_with_custom_funcs(
        &self,
        json: &str,
        custom_funcs: &[(String, CustomFunc)],
    ) -> JsonataResult {
        let input = Self::parse_input(json)?;
        if let Some(result) = fast_path::eval_fast(&self.fast_path, &input) {
            return Ok(result);
        }
        let env = Rc::new(Environment::new_eval_child(crate::stdlib::cached_root_env()));
        for (name, func) in custom_funcs {
            let arc_fn = Arc::clone(func);
            let builtin: Rc<crate::evaluator::BuiltinFn> =
                Rc::new(move |args: &[Value], focus: &Value| arc_fn(args, focus));
            env.bind(
                name.clone(),
                Value::Function(Box::new(FunctionValue::Builtin(builtin))),
            );
        }
        if !input.is_undefined() {
            env.bind("$", input.clone());
        }
        let result = crate::evaluator::eval(&self.arena, self.root, &input, &env);
        env.teardown_cycles();
        result
    }

    /// Evaluate with extra variable bindings.
    ///
    /// Variables are bound in the environment alongside `$` (the input).
    /// Reference them as `$varName` in expressions. Names should not include
    /// the leading `$`.
    ///
    /// # Errors
    /// Returns JSONata evaluation errors.
    pub fn evaluate_with_vars(&self, json: &str, vars: &[(String, Value)]) -> JsonataResult {
        let input = Self::parse_input(json)?;
        if let Some(result) = fast_path::eval_fast(&self.fast_path, &input) {
            return Ok(result);
        }
        let env = Rc::new(Environment::new_eval_child(crate::stdlib::cached_root_env()));
        if !input.is_undefined() {
            env.bind("$", input.clone());
        }
        for (name, value) in vars {
            env.bind(name.clone(), value.clone());
        }
        let result = crate::evaluator::eval(&self.arena, self.root, &input, &env);
        env.teardown_cycles();
        result
    }

    /// Evaluate with a cancellation token.
    ///
    /// Setting the `AtomicBool` to `true` from another thread will cause the
    /// evaluator to return error code `D3001` at the next function call
    /// boundary or hot-loop poll (function-free HOF/auto-map/predicate loops
    /// check every 1024 items). Expressions served entirely by the compiled
    /// fast path complete without checking cancellation.
    ///
    /// # Errors
    /// Returns `D3001` if cancelled, or other JSONata evaluation errors.
    pub fn evaluate_with_cancel(&self, json: &str, cancel: Arc<AtomicBool>) -> JsonataResult {
        let input = Self::parse_input(json)?;
        if let Some(result) = fast_path::eval_fast(&self.fast_path, &input) {
            return Ok(result);
        }
        let mut env = Environment::new_eval_child(crate::stdlib::cached_root_env());
        env.set_cancel(cancel);
        if !input.is_undefined() {
            env.bind("$", input.clone());
        }
        let env = Rc::new(env);
        let result = crate::evaluator::eval(&self.arena, self.root, &input, &env);
        env.teardown_cycles();
        result
    }

    /// Evaluate with a pre-configured environment.
    ///
    /// The input is bound as `$` in a per-evaluation child scope, so `$$`
    /// resolves to the current input and the shared `env` is never mutated.
    ///
    /// # Errors
    /// Returns JSONata evaluation errors.
    pub fn evaluate_with_env(&self, input: &Value, env: &Rc<Environment>) -> JsonataResult {
        if let Some(result) = fast_path::eval_fast(&self.fast_path, input) {
            return Ok(result);
        }
        let eval_env = Rc::new(Environment::new_child(Rc::clone(env)));
        if !input.is_undefined() {
            eval_env.bind("$", input.clone());
        }
        let result = crate::evaluator::eval(&self.arena, self.root, input, &eval_env);
        eval_env.teardown_cycles();
        result
    }

    /// Empty input and the literal `null` document mean *no input*:
    /// evaluation runs against Undefined, not Null (`$exists($)` is false).
    fn input_is_absent(json_bytes: &[u8]) -> bool {
        json_bytes.is_empty() || json_bytes == b"null"
    }

    /// Parse JSON string to Value, treating empty/null as Undefined.
    fn parse_input(json: &str) -> JsonataResult<Value> {
        if Self::input_is_absent(json.as_bytes()) {
            return Ok(Value::Undefined);
        }
        Value::from_json_str(json)
    }

    /// Returns the fast-path classification for this expression.
    pub fn fast_path_info(&self) -> &FastPath {
        &self.fast_path
    }

    /// Returns true if this expression uses a fast path.
    pub fn is_fast_path(&self) -> bool {
        !matches!(self.fast_path, FastPath::None)
    }

    /// Returns the original source expression.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Access the underlying AST arena.
    ///
    /// Not part of the supported public API: the AST types are internal and
    /// carry no stability guarantees.
    #[doc(hidden)]
    pub fn arena(&self) -> &AstArena {
        &self.arena
    }

    /// Access the root AST node.
    ///
    /// Not part of the supported public API: the AST types are internal and
    /// carry no stability guarantees.
    #[doc(hidden)]
    pub fn root(&self) -> NodeId {
        self.root
    }
}

impl std::fmt::Debug for Expression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Expression")
            .field("source", &self.source)
            .field("fast_path", &self.fast_path)
            .field("root", &self.root)
            .field("arena_size", &self.arena.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The thread-cached stdlib root must be untouched by evaluations:
    /// top-level `:=` binds land on the per-eval child, sequential lambda
    /// evaluations (whose teardown clears eval-scope closure envs) leave
    /// the stdlib intact, and `$$` still resolves per evaluation.
    #[test]
    fn cached_root_is_isolated_across_evaluations() {
        let bind = Expression::compile("$x := 42").unwrap();
        assert_eq!(
            bind.evaluate("{}").unwrap().as_f64(),
            Some(42.0),
            "bare top-level bind evaluates to its value"
        );
        let read = Expression::compile("$x").unwrap();
        assert!(
            read.evaluate("{}").unwrap().is_undefined(),
            "$x must not leak into the next evaluation via the cached root"
        );

        let lambda = Expression::compile("($f := function($n) { $n + 1 }; $f(41))").unwrap();
        assert_eq!(lambda.evaluate("{}").unwrap().as_f64(), Some(42.0));
        assert_eq!(
            lambda.evaluate("{}").unwrap().as_f64(),
            Some(42.0),
            "second lambda evaluation works after the first teardown"
        );
        let stdlib = Expression::compile("$sum([40, 2])").unwrap();
        assert_eq!(
            stdlib.evaluate("{}").unwrap().as_f64(),
            Some(42.0),
            "stdlib survives closure-env teardown"
        );

        let root = Expression::compile("$$.x").unwrap();
        assert_eq!(
            root.evaluate(r#"{"x": 7}"#).unwrap().as_f64(),
            Some(7.0),
            "$$ binds in the per-eval child"
        );
    }

    /// A cancelled evaluation and a depth-exhausted evaluation leave no
    /// state behind for the next one: the cancel token and CallCounter are
    /// per-eval, never the cached root's.
    #[test]
    fn cancel_and_depth_state_do_not_stick_to_the_cached_root() {
        let expr = Expression::compile("($f := function($n) { $f($n + 1) }; $f(0))").unwrap();
        let err = expr.evaluate("{}").unwrap_err();
        assert_eq!(err.code, "U1001");
        let ok = Expression::compile("1 + 1").unwrap();
        assert_eq!(
            ok.evaluate("{}").unwrap().as_f64(),
            Some(2.0),
            "depth exhaustion must not poison later evaluations"
        );

        let cancelled = Arc::new(AtomicBool::new(true));
        let loops = Expression::compile("$sum([1..100000].($ + 1))").unwrap();
        let err = loops
            .evaluate_with_cancel("{}", Arc::clone(&cancelled))
            .unwrap_err();
        assert_eq!(err.code, "D3001");
        assert_eq!(
            loops.evaluate("{}").unwrap().as_f64(),
            Some(5_000_150_000.0),
            "the cancel token must not stick to the shared root"
        );
    }

    #[test]
    fn clone_shares_ast_and_outlives_original() -> JsonataResult<()> {
        let expr = Expression::compile("a.b + 1")?;
        let cloned = expr.clone();
        assert!(Arc::ptr_eq(&expr.arena, &cloned.arena));
        drop(expr);
        let result = cloned.evaluate(r#"{"a": {"b": 41}}"#)?;
        assert_eq!(result.as_f64(), Some(42.0));
        Ok(())
    }

    #[test]
    fn evaluate_with_env_binds_root_input_per_evaluation() {
        let env = new_custom_env(&[]);
        let expr = Expression::compile("$$.x").unwrap();
        let a = Value::from_json_str(r#"{"x": 1}"#).unwrap();
        let b = Value::from_json_str(r#"{"x": 2}"#).unwrap();
        assert_eq!(
            expr.evaluate_with_env(&a, &env).unwrap().as_f64(),
            Some(1.0)
        );
        // A second evaluation must see ITS input in $$, not the first one.
        assert_eq!(
            expr.evaluate_with_env(&b, &env).unwrap().as_f64(),
            Some(2.0)
        );
        // The shared env is never mutated: $$ stays unbound for absent input.
        let bare = Expression::compile("$$").unwrap();
        assert!(
            bare.evaluate_with_env(&Value::Undefined, &env)
                .unwrap()
                .is_undefined()
        );
    }

    #[test]
    fn evaluate_bytes_normalizes_absent_input_like_evaluate() {
        for expr_src in ["$exists($)", "$type($)", "\"ok\""] {
            let expr = Expression::compile(expr_src).unwrap();
            for input in ["", "null"] {
                let via_str = expr.evaluate(input).unwrap();
                let via_bytes = expr.evaluate_bytes(input.as_bytes()).unwrap();
                assert!(
                    crate::deep_equal(&via_str, &via_bytes)
                        || (via_str.is_undefined() && via_bytes.is_undefined()),
                    "{expr_src} on {input:?}: evaluate={via_str:?} evaluate_bytes={via_bytes:?}"
                );
            }
        }
        // The literal null document means no input, not Null.
        let exists = Expression::compile("$exists($)").unwrap();
        assert_eq!(exists.evaluate_bytes(b"null").unwrap(), Value::Bool(false));
    }

    #[test]
    fn custom_func_basic() {
        let double: CustomFunc = Arc::new(|args: &[Value], _focus: &Value| {
            let n = args.first().and_then(Value::as_f64).unwrap_or(0.0);
            Ok(Value::Number(n * 2.0))
        });
        let expr = Expression::compile("$double(21)").unwrap();
        let result = expr
            .evaluate_with_custom_funcs("", &[("double".into(), double)])
            .unwrap();
        assert_eq!(result.as_f64(), Some(42.0));
    }

    #[test]
    fn custom_func_with_focus() {
        let get_type: CustomFunc = Arc::new(|_args: &[Value], focus: &Value| {
            let t = if focus.is_object() { "object" } else { "other" };
            Ok(Value::String(t.into()))
        });
        let expr = Expression::compile("$getType()").unwrap();
        let result = expr
            .evaluate_with_custom_funcs(r#"{"a":1}"#, &[("getType".into(), get_type)])
            .unwrap();
        assert_eq!(result.as_str(), Some("object"));
    }

    #[test]
    fn custom_func_alongside_stdlib() {
        let greet: CustomFunc = Arc::new(|args: &[Value], _focus: &Value| {
            let name = args.first().and_then(Value::as_str).unwrap_or("world");
            Ok(Value::String(format!("hello {name}").into()))
        });
        let expr = Expression::compile("$uppercase($greet(name))").unwrap();
        let result = expr
            .evaluate_with_custom_funcs(r#"{"name":"alice"}"#, &[("greet".into(), greet)])
            .unwrap();
        assert_eq!(result.as_str(), Some("HELLO ALICE"));
    }

    #[test]
    fn custom_func_error_propagation() {
        let fail: CustomFunc = Arc::new(|_args: &[Value], _focus: &Value| {
            Err(crate::error::JsonataError::new("D3030", "custom error"))
        });
        let expr = Expression::compile("$fail()").unwrap();
        let err = expr
            .evaluate_with_custom_funcs("", &[("fail".into(), fail)])
            .unwrap_err();
        assert_eq!(err.code, "D3030");
    }

    #[test]
    fn new_custom_env_reusable() {
        let add_one: CustomFunc = Arc::new(|args: &[Value], _focus: &Value| {
            let n = args.first().and_then(Value::as_f64).unwrap_or(0.0);
            Ok(Value::Number(n + 1.0))
        });
        let env = new_custom_env(&[("addOne".into(), add_one)]);

        let expr1 = Expression::compile("$addOne(10)").unwrap();
        let expr2 = Expression::compile("$addOne(20)").unwrap();

        env.bind("$", Value::Undefined);
        let r1 = expr1.evaluate_with_env(&Value::Undefined, &env).unwrap();
        let r2 = expr2.evaluate_with_env(&Value::Undefined, &env).unwrap();

        assert_eq!(r1.as_f64(), Some(11.0));
        assert_eq!(r2.as_f64(), Some(21.0));
    }

    /// A custom function bound over a prepared-fast-path name ($round)
    /// must win even through lifted mapped-call dispatch — prepared state
    /// is gated on Rc identity with the canonical stdlib registration
    /// (gnata-dx5.13).
    #[test]
    fn custom_override_of_prepared_builtin_wins_on_fast_path() {
        let fake_round: CustomFunc = Arc::new(|args: &[Value], _| {
            let n = args.first().and_then(Value::as_f64).unwrap_or(0.0);
            Ok(Value::Number(n + 1000.0))
        });
        let env = new_custom_env(&[("round".into(), fake_round)]);
        let input = Value::from_json_str(r#"{"items": [{"x": 1.4}, {"x": 2.6}]}"#).unwrap();

        // Lifted path-step form and lifted $map form both resolve the
        // callee by name; pre-fix the prepared state ran stdlib $round.
        for src in ["items.$round(x)", "$map(items, function($v){$round($v.x)})"] {
            let expr = Expression::compile(src).unwrap();
            let result = expr.evaluate_with_env(&input, &env).unwrap();
            let expected = Value::from_json_str("[1001.4, 1002.6]").unwrap();
            assert!(
                result.deep_equal(&expected),
                "{src}: override lost to prepared state, got {result:?}"
            );
        }

        // The stdlib $round still takes its fast path in a plain env.
        let expr = Expression::compile("items.$round(x)").unwrap();
        let result = expr
            .evaluate(r#"{"items": [{"x": 1.4}, {"x": 2.6}]}"#)
            .unwrap();
        let expected = Value::from_json_str("[1, 3]").unwrap();
        assert!(result.deep_equal(&expected), "got {result:?}");
    }

    #[test]
    fn custom_func_multiple() {
        let add: CustomFunc = Arc::new(|args: &[Value], _focus: &Value| {
            let a = args.first().and_then(Value::as_f64).unwrap_or(0.0);
            let b = args.get(1).and_then(Value::as_f64).unwrap_or(0.0);
            Ok(Value::Number(a + b))
        });
        let mul: CustomFunc = Arc::new(|args: &[Value], _focus: &Value| {
            let a = args.first().and_then(Value::as_f64).unwrap_or(0.0);
            let b = args.get(1).and_then(Value::as_f64).unwrap_or(0.0);
            Ok(Value::Number(a * b))
        });
        let expr = Expression::compile("$mul($add(2, 3), 4)").unwrap();
        let result = expr
            .evaluate_with_custom_funcs("", &[("add".into(), add), ("mul".into(), mul)])
            .unwrap();
        assert_eq!(result.as_f64(), Some(20.0));
    }

    #[test]
    fn custom_func_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CustomFunc>();
    }

    /// A lambda bound into its own scope forms an Rc cycle; the API
    /// boundary must break it or every evaluation leaks its env chain —
    /// including the bound input (gnata-0mb.7: 200k evals leaked 2.5 GB).
    #[test]
    fn recursive_lambda_does_not_leak_the_input() {
        let expr = Expression::compile("($f := function($n){$n < 2 ? 1 : $n * $f($n - 1)}; $f(5))")
            .unwrap();
        let input = Value::from_json_str(r#"{"a": [1, 2, 3]}"#).unwrap();
        let Value::Object(obj) = &input else {
            panic!("expected object input")
        };
        let base = Rc::strong_count(obj);
        assert_eq!(expr.evaluate_value(&input).unwrap().as_f64(), Some(120.0));
        assert_eq!(
            Rc::strong_count(obj),
            base,
            "evaluation leaked the env chain holding the input"
        );
    }

    #[test]
    fn cancel_stops_evaluation() {
        use std::sync::atomic::AtomicBool;
        let cancel = Arc::new(AtomicBool::new(true));
        let expr = Expression::compile("$reduce([1,2,3], function($a,$b){$a+$b}, 0)").unwrap();
        let err = expr.evaluate_with_cancel("", cancel).unwrap_err();
        assert_eq!(err.code, "D3001");
    }

    /// Function-free fast loops (lifted $map/$reduce, predicate filters,
    /// auto-map steps) poll the cancellation flag at loop boundaries —
    /// they used to run to completion uncancellable (gnata-dx5.11).
    #[test]
    fn cancel_stops_function_free_loops() {
        use std::sync::atomic::AtomicBool;
        let data = r#"{"items": [{"a": 1}, {"a": 2}, {"a": 3}]}"#;
        for src in [
            "$map(items, function($v){$v.a})",
            "$filter(items, function($v){$v.a > 1})",
            "$reduce(items, function($p,$c){$p + $c.a}, 0)",
            "items[a > 0]",
            "items.(a + 1)",
        ] {
            let cancel = Arc::new(AtomicBool::new(true));
            let expr = Expression::compile(src).unwrap();
            let err = expr
                .evaluate_with_cancel(data, cancel)
                .expect_err(&format!("{src} should observe cancellation"));
            assert_eq!(err.code, "D3001", "for {src}");
        }
    }

    #[test]
    fn cancel_not_set_works_normally() {
        use std::sync::atomic::AtomicBool;
        let cancel = Arc::new(AtomicBool::new(false));
        let expr = Expression::compile("1 + 2").unwrap();
        let result = expr.evaluate_with_cancel("", cancel).unwrap();
        assert_eq!(result.as_f64(), Some(3.0));
    }

    #[test]
    fn eval_with_vars_basic() {
        let expr = Expression::compile("$x + $y").unwrap();
        let result = expr
            .evaluate_with_vars(
                "",
                &[
                    ("x".into(), Value::Number(10.0)),
                    ("y".into(), Value::Number(32.0)),
                ],
            )
            .unwrap();
        assert_eq!(result.as_f64(), Some(42.0));
    }

    #[test]
    fn eval_with_vars_and_input() {
        let expr = Expression::compile("name & ' ' & $suffix").unwrap();
        let result = expr
            .evaluate_with_vars(
                r#"{"name":"Alice"}"#,
                &[("suffix".into(), Value::String("Smith".into()))],
            )
            .unwrap();
        assert_eq!(result.as_str(), Some("Alice Smith"));
    }

    #[test]
    fn eval_with_vars_uses_stdlib() {
        let expr = Expression::compile("$uppercase($greeting)").unwrap();
        let result = expr
            .evaluate_with_vars("", &[("greeting".into(), Value::String("hello".into()))])
            .unwrap();
        assert_eq!(result.as_str(), Some("HELLO"));
    }
}
