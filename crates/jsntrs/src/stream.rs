//! Streaming evaluator for multiple compiled expressions.
//!
//! Single-threaded design: each JSON stream gets its own evaluator on its own
//! thread. No locking, no atomic operations. Compile once, evaluate many.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crate::error::{JsonataError, JsonataResult};
use crate::expression::{CustomFunc, Expression};
use crate::value::Value;

/// Telemetry hook for [`StreamEvaluator`].
pub trait MetricsHook {
    /// Called after each expression evaluation with timing and path info.
    fn on_eval(
        &self,
        expr_index: usize,
        fast_path: bool,
        duration: Duration,
        err: Option<&JsonataError>,
    );
}

/// Cache statistics returned by [`StreamEvaluator::stats`].
#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    /// Number of expression slots (including removed).
    pub expressions: usize,
}

/// Evaluator for multiple compiled expressions against a single JSON stream.
///
/// Designed for single-threaded use: one evaluator per JSON source, pinned to
/// one thread/core. For multiple streams, create separate evaluators.
///
/// Go equivalent: `StreamEvaluator` in `stream.go`.
pub struct StreamEvaluator {
    exprs: Vec<Option<Expression>>,
    metrics: Option<Box<dyn MetricsHook>>,
    custom_funcs: Vec<(String, CustomFunc)>,
}

impl std::fmt::Debug for StreamEvaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamEvaluator")
            .field("expressions", &self.exprs.len())
            .field(
                "custom_funcs",
                &self.custom_funcs.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            )
            .field("metrics", &self.metrics.is_some())
            .finish()
    }
}

impl StreamEvaluator {
    /// Create a new evaluator with the given compiled expressions.
    pub fn new(expressions: Vec<Expression>) -> Self {
        let exprs = expressions.into_iter().map(Some).collect();
        Self {
            exprs,
            metrics: None,
            custom_funcs: Vec::new(),
        }
    }

    /// Register user-defined functions that extend the standard JSONata library.
    ///
    /// Functions are stored at construction time. During evaluation, a shared
    /// environment is created once per `eval_many` call (not per expression);
    /// each expression still evaluates in its own child scope, so top-level
    /// variable bindings never leak between batch expressions.
    /// Function names should not include the leading `$`.
    #[must_use]
    pub fn with_custom_functions(mut self, fns: Vec<(String, CustomFunc)>) -> Self {
        self.custom_funcs = fns;
        self
    }

    /// Attach a metrics hook for evaluation telemetry.
    #[must_use]
    pub fn with_metrics(mut self, hook: Box<dyn MetricsHook>) -> Self {
        self.metrics = Some(hook);
        self
    }

    /// Add a compiled expression and return its stable index.
    pub fn add(&mut self, expr: Expression) -> usize {
        let idx = self.exprs.len();
        self.exprs.push(Some(expr));
        idx
    }

    /// Compile a JSONata expression string, add it, and return its stable index.
    ///
    /// # Errors
    /// Returns parse or AST processing errors.
    pub fn compile(&mut self, src: &str) -> Result<usize, JsonataError> {
        let expr = Expression::compile(src)?;
        Ok(self.add(expr))
    }

    /// Replace the expression at the given index.
    ///
    /// # Errors
    /// Returns an error if the index is out of range.
    pub fn replace(&mut self, idx: usize, expr: Expression) -> Result<(), JsonataError> {
        if idx >= self.exprs.len() {
            return Err(JsonataError::new(
                "D0000",
                format!(
                    "expression index {idx} out of range [0, {})",
                    self.exprs.len()
                ),
            ));
        }
        self.exprs[idx] = Some(expr);
        Ok(())
    }

    /// Remove the expression at the given index. The index is NOT reused.
    ///
    /// # Errors
    /// Returns an error if the index is out of range.
    pub fn remove(&mut self, idx: usize) -> Result<(), JsonataError> {
        if idx >= self.exprs.len() {
            return Err(JsonataError::new(
                "D0000",
                format!(
                    "expression index {idx} out of range [0, {})",
                    self.exprs.len()
                ),
            ));
        }
        self.exprs[idx] = None;
        Ok(())
    }

    /// Remove all expressions.
    pub fn reset(&mut self) {
        self.exprs.clear();
    }

    /// Number of expression slots (including removed).
    pub fn len(&self) -> usize {
        self.exprs.len()
    }

    /// Returns true if no expressions are registered.
    pub fn is_empty(&self) -> bool {
        self.exprs.is_empty()
    }

    /// Evaluate multiple expressions against one input.
    ///
    /// Returns `results[i]` for each `expr_indices[i]`. Removed or out-of-range
    /// indices produce `None`. Undefined results also produce `None`.
    ///
    /// # Errors
    /// Returns the first evaluation error encountered (short-circuits).
    pub fn eval_many(
        &self,
        input: &Value,
        expr_indices: &[usize],
    ) -> Result<Vec<Option<Value>>, JsonataError> {
        self.eval_many_inner(input, expr_indices, None)
    }

    /// Evaluate multiple expressions with a cancellation token.
    ///
    /// # Errors
    /// Returns `D3001` if cancelled, or the first evaluation error.
    pub fn eval_many_with_cancel(
        &self,
        input: &Value,
        expr_indices: &[usize],
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<Option<Value>>, JsonataError> {
        self.eval_many_inner(input, expr_indices, Some(cancel))
    }

    fn eval_many_inner(
        &self,
        input: &Value,
        expr_indices: &[usize],
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<Vec<Option<Value>>, JsonataError> {
        if expr_indices.is_empty() {
            return Ok(Vec::new());
        }

        // Build shared env once per call if custom functions or cancel are set.
        let needs_env = !self.custom_funcs.is_empty() || cancel.is_some();
        let custom_env = if needs_env {
            let mut env =
                crate::evaluator::Environment::new_eval_child(crate::stdlib::cached_root_env());
            if let Some(cancel) = cancel {
                env.set_cancel(cancel);
            }
            for (name, func) in &self.custom_funcs {
                let arc_fn = Arc::clone(func);
                let builtin: std::rc::Rc<crate::evaluator::BuiltinFn> =
                    std::rc::Rc::new(move |args: &[Value], focus: &Value| arc_fn(args, focus));
                env.bind(
                    name.clone(),
                    Value::Function(Box::new(crate::evaluator::FunctionValue::Builtin(builtin))),
                );
            }
            if !input.is_undefined() {
                env.bind("$", input.clone());
            }
            Some(std::rc::Rc::new(env))
        } else {
            None
        };

        let mut results = Vec::with_capacity(expr_indices.len());

        for &idx in expr_indices {
            if idx >= self.exprs.len() {
                results.push(None);
                continue;
            }
            let Some(ref expr) = self.exprs[idx] else {
                results.push(None);
                continue;
            };

            let start = self.metrics.as_ref().map(|_| Instant::now());
            let eval_result = match custom_env {
                Some(ref env) => expr.evaluate_with_env(input, env),
                None => expr.evaluate_value(input),
            };
            match eval_result {
                Ok(val) => {
                    if let (Some(hook), Some(start)) = (&self.metrics, start) {
                        hook.on_eval(idx, expr.is_fast_path(), start.elapsed(), None);
                    }
                    results.push(if val.is_undefined() { None } else { Some(val) });
                }
                Err(e) => {
                    if let (Some(hook), Some(start)) = (&self.metrics, start) {
                        hook.on_eval(idx, false, start.elapsed(), Some(&e));
                    }
                    return Err(e);
                }
            }
        }
        Ok(results)
    }

    /// Evaluate a single expression against input data.
    ///
    /// # Errors
    /// Returns evaluation errors, or `Value::Undefined` for removed/out-of-range indices.
    pub fn eval_one(&self, input: &Value, expr_index: usize) -> JsonataResult {
        let results = self.eval_many(input, &[expr_index])?;
        Ok(results
            .into_iter()
            .next()
            .flatten()
            .unwrap_or(Value::Undefined))
    }

    /// Evaluate a single expression with cancellation support.
    ///
    /// # Errors
    /// Returns `D3001` if cancelled, or other evaluation errors.
    pub fn eval_one_with_cancel(
        &self,
        input: &Value,
        expr_index: usize,
        cancel: Arc<AtomicBool>,
    ) -> JsonataResult {
        let results = self.eval_many_with_cancel(input, &[expr_index], cancel)?;
        Ok(results
            .into_iter()
            .next()
            .flatten()
            .unwrap_or(Value::Undefined))
    }

    /// Returns cache statistics.
    pub fn stats(&self) -> StreamStats {
        StreamStats {
            expressions: self.exprs.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_and_eval_one() {
        let mut se = StreamEvaluator::new(Vec::new());
        let idx = se.compile("Account.Name").unwrap();
        let input =
            crate::value::Value::from_json_str(r#"{"Account": {"Name": "Firefly"}}"#).unwrap();
        let result = se.eval_one(&input, idx).unwrap();
        assert_eq!(result, Value::String("Firefly".into()));
    }

    #[test]
    fn eval_many_returns_results_per_index() {
        let mut se = StreamEvaluator::new(Vec::new());
        let i0 = se.compile("Account.Name").unwrap();
        let i1 = se.compile("Account.Order[0].OrderID").unwrap();
        let input = crate::value::Value::from_json_str(
            r#"{"Account": {"Name": "Firefly", "Order": [{"OrderID": "order103"}]}}"#,
        )
        .unwrap();
        let results = se.eval_many(&input, &[i0, i1]).unwrap();
        assert_eq!(results[0], Some(Value::String("Firefly".into())));
        assert_eq!(results[1], Some(Value::String("order103".into())));
    }

    /// Top-level `:=` in one batch expression must not be visible to the
    /// next, on both the plain path and the shared custom-env path, and
    /// `$$` must still resolve to the current input (gnata-eci.6).
    #[test]
    fn eval_many_isolates_bindings_between_expressions() {
        let input = crate::value::Value::from_json_str(r#"{"a": 1}"#).unwrap();

        // Plain path: evaluate_value builds a fresh env per expression.
        let mut se = StreamEvaluator::new(Vec::new());
        let i0 = se.compile("$x := 42").unwrap();
        let i1 = se.compile("$x").unwrap();
        let i2 = se.compile("$$.a").unwrap();
        let results = se.eval_many(&input, &[i0, i1, i2]).unwrap();
        assert_eq!(results[0], Some(Value::Number(42.0)));
        assert_eq!(results[1], None, ":= leaked across batch expressions");
        assert_eq!(results[2], Some(Value::Number(1.0)));

        // Custom-func path: expressions share one batch env; each eval
        // must get its own child scope (via evaluate_with_env).
        let double: crate::expression::CustomFunc = Arc::new(|args, _| {
            let n = args
                .first()
                .and_then(Value::as_f64)
                .ok_or_else(|| JsonataError::new("T0410", "number required"))?;
            Ok(Value::Number(n * 2.0))
        });
        let mut se = StreamEvaluator::new(Vec::new())
            .with_custom_functions(vec![("double".to_string(), double)]);
        let i0 = se.compile("$x := $double(21)").unwrap();
        let i1 = se.compile("$x").unwrap();
        let i2 = se.compile("$$.a").unwrap();
        let results = se.eval_many(&input, &[i0, i1, i2]).unwrap();
        assert_eq!(results[0], Some(Value::Number(42.0)));
        assert_eq!(
            results[1], None,
            ":= leaked across batch expressions (custom env)"
        );
        assert_eq!(results[2], Some(Value::Number(1.0)));
    }

    #[test]
    fn debug_summarizes_without_dumping_expressions() {
        let mut se = StreamEvaluator::new(Vec::new());
        se.compile("1+1").unwrap();
        let dbg = format!("{se:?}");
        assert!(dbg.contains("StreamEvaluator"));
        assert!(dbg.contains("expressions: 1"));
    }

    #[test]
    fn remove_produces_none() {
        let mut se = StreamEvaluator::new(Vec::new());
        let idx = se.compile("1+1").unwrap();
        se.remove(idx).unwrap();
        let input = Value::Undefined;
        let results = se.eval_many(&input, &[idx]).unwrap();
        assert_eq!(results[0], None);
    }

    #[test]
    fn replace_updates_expression() {
        let mut se = StreamEvaluator::new(Vec::new());
        let idx = se.compile("1+1").unwrap();
        let input = Value::Undefined;
        assert_eq!(se.eval_one(&input, idx).unwrap(), Value::Number(2.0));

        let new_expr = Expression::compile("2+2").unwrap();
        se.replace(idx, new_expr).unwrap();
        assert_eq!(se.eval_one(&input, idx).unwrap(), Value::Number(4.0));
    }

    #[test]
    fn reset_clears_all() {
        let mut se = StreamEvaluator::new(Vec::new());
        se.compile("1").unwrap();
        se.compile("2").unwrap();
        assert_eq!(se.len(), 2);
        se.reset();
        assert_eq!(se.len(), 0);
    }

    #[test]
    fn out_of_range_produces_none() {
        let se = StreamEvaluator::new(Vec::new());
        let results = se.eval_many(&Value::Undefined, &[999]).unwrap();
        assert_eq!(results[0], None);
    }

    // ── Ported from Go stream_test.go ────────────────────────────────

    const STREAM_TEST_DATA: &str = r#"{
        "data": {"action": "grant-access", "user_type": 2},
        "metadata": {"is_admin": true}
    }"#;

    fn test_input() -> Value {
        Value::from_json_str(STREAM_TEST_DATA).unwrap()
    }

    #[test]
    fn compile_sequential_indices() {
        let mut se = StreamEvaluator::new(Vec::new());
        let exprs = [
            r#"data.action = "grant-access""#,
            "data.user_type = 2",
            "metadata.is_admin = true",
        ];
        for (i, src) in exprs.iter().enumerate() {
            let idx = se.compile(src).unwrap();
            assert_eq!(idx, i, "expr {i} should get index {i}");
        }
        assert_eq!(se.len(), 3);

        let indices: Vec<usize> = (0..3).collect();
        let results = se.eval_many(&test_input(), &indices).unwrap();
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r, &Some(Value::Bool(true)), "result[{i}]");
        }
    }

    #[test]
    fn add_precompiled_expressions() {
        let cases: &[(&str, Value)] = &[
            (r#"data.action = "grant-access""#, Value::Bool(true)),
            ("data.user_type = 2", Value::Bool(true)),
            ("metadata.is_admin = true", Value::Bool(true)),
            (r#"data.action != "other""#, Value::Bool(true)),
            ("data.user_type != 99", Value::Bool(true)),
            ("data.user_type", Value::Number(2.0)),
            ("data.user_type > 1", Value::Bool(true)),
            (
                "data.user_type = 2 and metadata.is_admin = true",
                Value::Bool(true),
            ),
        ];
        let input = test_input();
        for (expr, want) in cases {
            let compiled = Expression::compile(expr).unwrap();
            let mut se = StreamEvaluator::new(Vec::new());
            let idx = se.add(compiled);
            assert_eq!(idx, 0);
            let got = se.eval_one(&input, idx).unwrap();
            assert_eq!(&got, want, "expr: {expr}");
        }
    }

    #[test]
    fn mixed_fast_path_and_full_eval() {
        let mut se = StreamEvaluator::new(Vec::new());
        let i0 = se.compile("data.user_type = 2").unwrap(); // comparison fast path
        let i1 = se.compile("data.user_type > 1").unwrap(); // full eval
        let i2 = se.compile("data.user_type").unwrap(); // pure-path fast path

        let results = se.eval_many(&test_input(), &[i0, i1, i2]).unwrap();
        assert_eq!(results[0], Some(Value::Bool(true)));
        assert_eq!(results[1], Some(Value::Bool(true)));
        assert_eq!(results[2], Some(Value::Number(2.0)));
    }

    #[test]
    fn index_stability_after_adds() {
        let mut se = StreamEvaluator::new(Vec::new());
        let i0 = se.compile(r#"data.action = "grant-access""#).unwrap();
        let i1 = se.compile("data.user_type = 2").unwrap();

        for i in 0..100 {
            se.compile(&format!("data.user_type = {}", i + 1000))
                .unwrap();
        }
        assert_eq!(se.len(), 102);

        let results = se.eval_many(&test_input(), &[i0, i1]).unwrap();
        assert_eq!(results[0], Some(Value::Bool(true)));
        assert_eq!(results[1], Some(Value::Bool(true)));
    }

    #[test]
    fn replace_swaps_expression() {
        let mut se = StreamEvaluator::new(Vec::new());
        let idx = se.compile("data.action").unwrap();
        let input = test_input();

        let got = se.eval_one(&input, idx).unwrap();
        assert_eq!(got, Value::String("grant-access".into()));

        let new_expr = Expression::compile("data.user_type").unwrap();
        se.replace(idx, new_expr).unwrap();

        let got = se.eval_one(&input, idx).unwrap();
        assert_eq!(got, Value::Number(2.0));
    }

    #[test]
    fn remove_returns_none_keeps_others() {
        let mut se = StreamEvaluator::new(Vec::new());
        let i0 = se.compile(r#"data.action = "grant-access""#).unwrap();
        let i1 = se.compile("data.user_type = 2").unwrap();

        se.remove(i0).unwrap();

        let results = se.eval_many(&test_input(), &[i0, i1]).unwrap();
        assert_eq!(results[0], None, "removed expr should be None");
        assert_eq!(results[1], Some(Value::Bool(true)), "kept expr should work");
    }

    #[test]
    fn reset_allows_reuse() {
        let mut se = StreamEvaluator::new(Vec::new());
        se.compile("data.action").unwrap();
        se.compile("data.user_type").unwrap();
        assert_eq!(se.len(), 2);

        se.reset();
        assert_eq!(se.len(), 0);

        let idx = se.compile("metadata.is_admin").unwrap();
        assert_eq!(idx, 0);
    }

    // ── WithCustomFunctions tests ───────────────────────────────────

    #[test]
    fn custom_func_in_stream() {
        let double: CustomFunc = Arc::new(|args: &[Value], _| {
            let n = args.first().and_then(Value::as_f64).unwrap_or(0.0);
            Ok(Value::Number(n * 2.0))
        });
        let mut se =
            StreamEvaluator::new(Vec::new()).with_custom_functions(vec![("double".into(), double)]);
        let idx = se.compile("$double(data.user_type)").unwrap();
        let result = se.eval_one(&test_input(), idx).unwrap();
        assert_eq!(result, Value::Number(4.0));
    }

    #[test]
    fn custom_func_with_stdlib_in_stream() {
        let greet: CustomFunc = Arc::new(|args: &[Value], _| {
            let name = args.first().and_then(Value::as_str).unwrap_or("?");
            Ok(Value::String(format!("hi {name}").into()))
        });
        let mut se =
            StreamEvaluator::new(Vec::new()).with_custom_functions(vec![("greet".into(), greet)]);
        let idx = se.compile("$uppercase($greet(data.action))").unwrap();
        let result = se.eval_one(&test_input(), idx).unwrap();
        assert_eq!(result, Value::String("HI GRANT-ACCESS".into()));
    }

    #[test]
    fn multiple_custom_funcs_in_stream() {
        let add: CustomFunc = Arc::new(|args: &[Value], _| {
            let a = args.first().and_then(Value::as_f64).unwrap_or(0.0);
            let b = args.get(1).and_then(Value::as_f64).unwrap_or(0.0);
            Ok(Value::Number(a + b))
        });
        let mul: CustomFunc = Arc::new(|args: &[Value], _| {
            let a = args.first().and_then(Value::as_f64).unwrap_or(0.0);
            let b = args.get(1).and_then(Value::as_f64).unwrap_or(0.0);
            Ok(Value::Number(a * b))
        });
        let mut se = StreamEvaluator::new(Vec::new())
            .with_custom_functions(vec![("add".into(), add), ("mul".into(), mul)]);
        let i0 = se.compile("$add(data.user_type, 10)").unwrap();
        let i1 = se.compile("$mul(data.user_type, 3)").unwrap();
        let results = se.eval_many(&test_input(), &[i0, i1]).unwrap();
        assert_eq!(results[0], Some(Value::Number(12.0)));
        assert_eq!(results[1], Some(Value::Number(6.0)));
    }

    /// A registered custom function that shadows a builtin must win over the
    /// top-level function fast path, which resolves the name at compile time
    /// (jsntrs-6wr.4). The batch env reaches evaluation through
    /// `Expression::evaluate_with_env`, so the gate lives there.
    #[test]
    fn stream_custom_func_overrides_builtin_fast_path() {
        let fake_sum: CustomFunc = Arc::new(|_args: &[Value], _| Ok(Value::Number(999.0)));
        let input = Value::from_json_str(r#"{"items": [1, 2, 3]}"#).unwrap();

        let mut se =
            StreamEvaluator::new(Vec::new()).with_custom_functions(vec![("sum".into(), fake_sum)]);
        let idx = se.compile("$sum(items)").unwrap();
        assert_eq!(
            se.eval_one(&input, idx).unwrap(),
            Value::Number(999.0),
            "batch env ignored the custom $sum"
        );

        // Without custom functions the stdlib $sum keeps its fast path.
        let mut plain = StreamEvaluator::new(Vec::new());
        let idx = plain.compile("$sum(items)").unwrap();
        assert_eq!(plain.eval_one(&input, idx).unwrap(), Value::Number(6.0));
    }

    #[test]
    fn stream_no_custom_funcs_unchanged() {
        let mut se = StreamEvaluator::new(Vec::new());
        let idx = se.compile("data.user_type + 1").unwrap();
        let result = se.eval_one(&test_input(), idx).unwrap();
        assert_eq!(result, Value::Number(3.0));
    }

    // ── Cancellation tests ──────────────────────────────────────────

    #[test]
    fn expression_cancel_returns_d3001() {
        use std::sync::atomic::AtomicBool;
        let cancel = Arc::new(AtomicBool::new(true));
        let expr =
            crate::expression::Expression::compile("$reduce([1,2,3], function($a,$b){$a+$b}, 0)")
                .unwrap();
        let err = expr.evaluate_with_cancel("", cancel).unwrap_err();
        assert_eq!(err.code, "D3001");
    }

    #[test]
    fn stream_cancel_returns_d3001() {
        use std::sync::atomic::AtomicBool;
        let cancel = Arc::new(AtomicBool::new(true));
        let mut se = StreamEvaluator::new(Vec::new());
        let idx = se
            .compile("$reduce([1,2,3], function($a,$b){$a+$b}, 0)")
            .unwrap();
        let err = se
            .eval_many_with_cancel(&test_input(), &[idx], cancel)
            .unwrap_err();
        assert_eq!(err.code, "D3001");
    }

    #[test]
    fn stream_cancel_not_set_works_normally() {
        use std::sync::atomic::AtomicBool;
        let cancel = Arc::new(AtomicBool::new(false));
        let mut se = StreamEvaluator::new(Vec::new());
        let idx = se.compile("data.user_type + 1").unwrap();
        let result = se.eval_one_with_cancel(&test_input(), idx, cancel).unwrap();
        assert_eq!(result, Value::Number(3.0));
    }
}
