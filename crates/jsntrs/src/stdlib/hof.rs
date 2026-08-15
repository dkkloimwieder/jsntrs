//! Higher-order functions: $map, $filter, $reduce, $each, $sift, $sort, $single.

use std::rc::Rc;

use crate::error::{JsonataError, JsonataResult};
use crate::evaluator::{Environment, FunctionValue, call_function};
use crate::parser::AstArena;
use crate::parser::ast::BinaryOp;
use crate::value::{Sequence, Value};

use super::context_arg_code;
use super::hof_fast::{self, SimpleLambda, analyze_lambda};

/// Wrap a filtered result in the sequence `$filter` is specified to build.
///
/// The reference implementation collects matches with `createSequence()`,
/// so the singleton collapse happens at the *call site*, not here — which
/// is what makes `$filter(a, fn)[]` keep its singleton wrapped.
fn filtered_sequence(result: Vec<Value>) -> Value {
    Value::Sequence(Box::new(Sequence::with_items(result)))
}

/// Materialise a callback's result for embedding in an HOF's own result.
///
/// A callback is invoked through `apply()`, not `evaluate()`, so the
/// reference never collapses what it returns: a sequence goes into the
/// HOF's result as the (flagged) array it already is. jsntrs has no
/// value-level sequence flag, and [`Value::Sequence`] may never be nested
/// inside a user-visible value, so the sequence is materialised as a plain
/// array here — the singleton is deliberately *not* unwrapped
/// (jsntrs-p0v.6).
fn embed_callback_result(value: Value) -> Value {
    match value {
        Value::Sequence(seq) => Value::Array(Rc::from(seq.values)),
        other => other,
    }
}

/// Try to analyze a FunctionValue into a SimpleLambda for fast dispatch.
fn try_fast_lambda(func: &FunctionValue, arena: &AstArena) -> Option<SimpleLambda> {
    if crate::fast_path::testing::fast_paths_disabled() {
        return None;
    }
    if let FunctionValue::Lambda(lambda) = func {
        // A typed lambda (function($x)<n:n>{...}) needs the general call
        // path for signature validation/coercion (T0410, array coercion).
        if lambda.signature.is_some() {
            return None;
        }
        let lifted = analyze_lambda(&lambda.params, lambda.body, arena);
        if lifted.is_some() {
            crate::fast_path::testing::record_hit();
        }
        lifted
    } else {
        None
    }
}

/// Build HOF callback args trimmed to the callee's declared arity.
///
/// Mirrors Go's `hofArgs` and jsonata-js `hofFuncArgs`: the value is always
/// passed, while `second` (the index for array HOFs, the key for the object
/// HOFs `$sift`/`$each`) and `third` (the whole array/object) are supplied
/// only when the callee declares enough parameters. `second` is a closure so
/// callers pay nothing to build it for arity-0/1 callbacks.
fn hof_args(
    func: &FunctionValue,
    item: Value,
    second: impl FnOnce() -> Value,
    third: &Value,
) -> Vec<Value> {
    let arity = match func {
        FunctionValue::Lambda(lam) => lam.params.len(),
        _ => 1, // builtins get (value) only to avoid arity rejections
    };
    match arity {
        0 => vec![],
        1 => vec![item],
        2 => vec![item, second()],
        _ => vec![item, second(), third.clone()],
    }
}

pub fn fn_map(
    args: &[Value],
    _focus: &Value,
    env: &Rc<Environment>,
    arena: &AstArena,
) -> JsonataResult {
    if args.len() < 2 {
        return Err(JsonataError::new("T0410", "$map: requires 2 arguments"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let arr = args[0].coerce_to_array();
    let func = args[1].require_function("$map")?;

    // Fast path: simple field access — function($v){$v.field}
    match try_fast_lambda(&func, arena) {
        Some(SimpleLambda::FieldAccess { field, .. }) => {
            let mut seq = Sequence::new();
            for (i, item) in arr.iter().enumerate() {
                env.poll_cancelled(i)?;
                let val = hof_fast::get_field(item, &field);
                if !val.is_undefined() {
                    seq.values.push(val);
                }
            }
            return Ok(Value::Sequence(Box::new(seq)));
        }
        Some(SimpleLambda::ConcatTemplate { ref pieces }) => {
            let mut seq = Sequence::new();
            for (i, item) in arr.iter().enumerate() {
                env.poll_cancelled(i)?;
                let val = hof_fast::eval_concat_template(item, pieces)?;
                if !val.is_undefined() {
                    seq.values.push(val);
                }
            }
            return Ok(Value::Sequence(Box::new(seq)));
        }
        _ => {}
    }

    // Lifted dispatch: if the lambda body is a function call with field/const args,
    // resolve the inner function once and dispatch directly per item. The
    // callee name resolves in the lambda's closure, not the $map call site —
    // the general path evaluates the body in a child of the closure.
    if let FunctionValue::Lambda(ref lambda) = *func
        && let Some(param) = lambda.params.first()
        && let Some(mc) =
            hof_fast::analyze_mapped_call(lambda.body, arena, Some(param), &lambda.closure)
    {
        let mut seq = Sequence::new();
        for (i, item) in arr.iter().enumerate() {
            env.poll_cancelled(i)?;
            let val = hof_fast::exec_mapped_call(&mc, item, &lambda.closure, arena)?;
            if !val.is_undefined() {
                seq.values.push(embed_callback_result(val));
            }
        }
        return Ok(Value::Sequence(Box::new(seq)));
    }

    let arr_val = Value::Array(arr.clone()); // clone once, reuse
    let mut seq = Sequence::new();
    for (i, item) in arr.iter().enumerate() {
        let call_args = hof_args(&func, item.clone(), || Value::Number(i as f64), &arr_val);
        let val = call_function(&func, &call_args, item, env, arena)?;
        if !val.is_undefined() {
            seq.values.push(embed_callback_result(val));
        }
    }
    // Return as Sequence — caller handles collapse with keep_array support.
    Ok(Value::Sequence(Box::new(seq)))
}

pub fn fn_filter(
    args: &[Value],
    _focus: &Value,
    env: &Rc<Environment>,
    arena: &AstArena,
) -> JsonataResult {
    if args.len() < 2 {
        return Err(JsonataError::new("T0410", "$filter: requires 2 arguments"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let arr = args[0].coerce_to_array();
    let func = args[1].require_function("$filter")?;

    // Fast path: field predicate — function($v){$v.field op literal}
    if let Some(ref fast) = try_fast_lambda(&func, arena) {
        match fast {
            SimpleLambda::FieldPredicate {
                field, op, literal, ..
            } => {
                let mut result = Vec::new();
                for (i, item) in arr.iter().enumerate() {
                    env.poll_cancelled(i)?;
                    let fv = hof_fast::get_field(item, field);
                    let val = hof_fast::eval_binary_simple(&fv, *op, literal)?;
                    if val.to_boolean()? {
                        result.push(item.clone());
                    }
                }
                return Ok(filtered_sequence(result));
            }
            SimpleLambda::TwoFieldPredicate {
                field1, op, field2, ..
            } => {
                let mut result = Vec::new();
                for (i, item) in arr.iter().enumerate() {
                    env.poll_cancelled(i)?;
                    let fv1 = hof_fast::get_field(item, field1);
                    let fv2 = hof_fast::get_field(item, field2);
                    let val = hof_fast::eval_binary_simple(&fv1, *op, &fv2)?;
                    if val.to_boolean()? {
                        result.push(item.clone());
                    }
                }
                return Ok(filtered_sequence(result));
            }
            SimpleLambda::CompoundPredicate {
                clauses, combiner, ..
            } => {
                let is_and = *combiner == BinaryOp::And;
                let mut result = Vec::new();
                'outer: for (i, item) in arr.iter().enumerate() {
                    env.poll_cancelled(i)?;
                    for clause in clauses {
                        let fv = hof_fast::get_field(item, &clause.field);
                        let pass = hof_fast::eval_binary_simple(&fv, clause.op, &clause.literal)?
                            .to_boolean()?;
                        if is_and && !pass {
                            continue 'outer;
                        }
                        if !is_and && pass {
                            result.push(item.clone());
                            continue 'outer;
                        }
                    }
                    if is_and {
                        result.push(item.clone());
                    }
                }
                return Ok(filtered_sequence(result));
            }
            _ => {}
        }
    }

    let arr_val = Value::Array(arr.clone()); // clone once, reuse
    let mut result = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        let call_args = hof_args(&func, item.clone(), || Value::Number(i as f64), &arr_val);
        let val = call_function(&func, &call_args, item, env, arena)?;
        if val.to_boolean()? {
            result.push(item.clone());
        }
    }
    Ok(filtered_sequence(result))
}

pub fn fn_reduce(
    args: &[Value],
    _focus: &Value,
    env: &Rc<Environment>,
    arena: &AstArena,
) -> JsonataResult {
    if args.len() < 2 {
        return Err(JsonataError::new("T0410", "$reduce: requires 2 arguments"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let arr = args[0].coerce_to_array();
    let func = args[1].require_function("$reduce")?;
    // Check that the function accepts at least 2 parameters.
    if let crate::evaluator::functions::FunctionValue::Lambda(lambda) = &*func
        && lambda.params.len() < 2
    {
        return Err(JsonataError::new(
            "D3050",
            "$reduce: function argument must accept at least 2 parameters",
        ));
    }
    let init = args.get(2).cloned();
    if arr.is_empty() {
        return Ok(init.unwrap_or(Value::Undefined));
    }
    let (mut acc, start) = match init {
        Some(v) => (v, 0),
        None => (arr[0].clone(), 1),
    };

    // Fast path: simple reduce — function($prev,$curr){$prev + $curr.field}
    match try_fast_lambda(&func, arena) {
        Some(SimpleLambda::ReduceAccum { field, op, .. }) => {
            for (i, item) in arr[start..].iter().enumerate() {
                env.poll_cancelled(i)?;
                let fv = hof_fast::get_field(item, &field);
                acc = hof_fast::eval_binary_simple(&acc, op, &fv)?;
            }
            return Ok(acc);
        }
        Some(SimpleLambda::ReduceCompoundAccum {
            field1,
            field2,
            outer_op,
            inner_op,
            ..
        }) => {
            for (i, item) in arr[start..].iter().enumerate() {
                env.poll_cancelled(i)?;
                let fv1 = hof_fast::get_field(item, &field1);
                let fv2 = hof_fast::get_field(item, &field2);
                let inner = hof_fast::eval_binary_simple(&fv1, inner_op, &fv2)?;
                acc = hof_fast::eval_binary_simple(&acc, outer_op, &inner)?;
            }
            return Ok(acc);
        }
        _ => {}
    }

    // Determine arity for passing index/array like Go does.
    let param_count = if let FunctionValue::Lambda(ref lam) = *func {
        lam.params.len()
    } else {
        2 // default: (acc, item)
    };
    let arr_val = Value::Array(arr.clone());
    for (idx, item) in arr[start..].iter().enumerate() {
        let call_args = match param_count {
            0 | 1 => vec![acc],
            2 => vec![acc, item.clone()],
            3 => vec![acc, item.clone(), Value::Number((start + idx) as f64)],
            _ => vec![
                acc,
                item.clone(),
                Value::Number((start + idx) as f64),
                arr_val.clone(),
            ],
        };
        // Unlike $map/$each, the accumulator is read back through `$a` on
        // the next iteration and returned as the call's own value — both
        // consumer positions where the reference's `evaluate()` collapses
        // — so it collapses here rather than being embedded as an array.
        acc = crate::evaluator::collapse_sequence(call_function(
            &func, &call_args, item, env, arena,
        )?);
    }
    Ok(acc)
}

pub fn fn_each(
    args: &[Value],
    focus: &Value,
    env: &Rc<Environment>,
    arena: &AstArena,
) -> JsonataResult {
    // When called with 1 arg (function), use focus as the object.
    let (obj_arg, func_arg, from_focus) = if args.len() >= 2 {
        (&args[0], &args[1], false)
    } else if args.len() == 1 && args[0].is_function() {
        (focus, &args[0], true)
    } else if args.len() == 1 {
        (&args[0], focus, false)
    } else {
        return Err(JsonataError::new("T0410", "$each: requires 2 arguments"));
    };
    if obj_arg.is_undefined() {
        return Ok(Value::Undefined);
    }
    let Value::Object(obj) = obj_arg else {
        return Err(JsonataError::new(
            context_arg_code(from_focus),
            "$each: first argument must be an object",
        ));
    };
    let func = func_arg.require_function("$each")?;
    let mut seq = Sequence::new();
    for (key, val) in obj.iter() {
        // (value, key, object), trimmed to the callback's arity: a 1-arg
        // builtin like $exists must not be handed the key (T0410).
        let call_args = hof_args(
            &func,
            val.clone(),
            || Value::String(key.as_str().into()),
            obj_arg,
        );
        let r = call_function(&func, &call_args, val, env, arena)?;
        if !r.is_undefined() {
            seq.values.push(embed_callback_result(r));
        }
    }
    Ok(Value::Sequence(Box::new(seq)))
}

pub fn fn_sift(
    args: &[Value],
    focus: &Value,
    env: &Rc<Environment>,
    arena: &AstArena,
) -> JsonataResult {
    // When called with 1 arg (function), use focus as the object.
    let (obj_arg, func_arg, from_focus) = if args.len() >= 2 {
        (&args[0], &args[1], false)
    } else if args.len() == 1 && args[0].is_function() {
        (focus, &args[0], true)
    } else {
        return Err(JsonataError::new("T0410", "$sift: requires 2 arguments"));
    };
    if obj_arg.is_undefined() {
        return Ok(Value::Undefined);
    }
    let func = func_arg.require_function("$sift")?;
    // An array is *not* siftable: the reference signature is `<o-f?:o>`, so
    // arrays fail argument validation like any other non-object. The Go
    // reference mapped `$sift` over an array's object elements instead; that
    // extension was dropped (jsntrs-p0v.11 decision, jsntrs-xoe) because the
    // mapping is already reachable — and idiomatic — as `a.$sift(fn)`, where
    // ordinary path mapping invokes `$sift` once per object, while the
    // extension additionally dropped non-object elements silently.
    let Value::Object(obj) = obj_arg else {
        return Err(JsonataError::new(
            context_arg_code(from_focus),
            "$sift: first argument must be an object",
        ));
    };
    sift_object(obj, &func, obj_arg, env, arena)
}

pub fn fn_sort(
    args: &[Value],
    focus: &Value,
    env: &Rc<Environment>,
    arena: &AstArena,
) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$sort: argument is required"));
    }
    // Resolve array and comparator, mirroring Go's makeFnSort logic:
    // - 1 arg that's a function → use focus as array, arg as comparator
    // - 1 arg that's not a function → arg is the array, no comparator
    // - 2+ args → args[0] is array, args[1] is comparator
    let (arr_val, comparator) = if args.len() == 1 && args[0].is_function() {
        let f = match &args[0] {
            Value::Function(f) => Some(f.clone()),
            _ => None,
        };
        (focus, f)
    } else {
        // Reference signature `<af?:a>`: the comparator slot matches `f?`,
        // which accepts a function or nothing — not `undefined`, and not a
        // value of some other type. Dropping a bad comparator silently would
        // sort by natural order and hide the mistake (jsntrs-p0v.4).
        let f = match args.get(1) {
            None => None,
            Some(Value::Function(f)) => Some(f.clone()),
            Some(_) => {
                return Err(JsonataError::new(
                    "T0410",
                    "$sort: argument 2 must be a function",
                ));
            }
        };
        (&args[0], f)
    };
    if arr_val.is_undefined() {
        return Ok(Value::Undefined);
    }
    let arr = arr_val.coerce_to_array().to_vec();
    if arr.len() <= 1 {
        return Ok(Value::Array(Rc::from(arr)));
    }

    // Fast path: sort by field — function($a,$b){$a.field op $b.field}.
    // Replicates the general comparator protocol below exactly (call
    // fn(b, a), truthy → Less, falsy → Equal, errors propagate), only
    // inlining the lambda body instead of dispatching call_function.
    if let Some(func) = &comparator
        && let Some(SimpleLambda::SortComparator { field, op }) = try_fast_lambda(func, arena)
    {
        let arr = crate::try_sort::try_sort_by(arr, |a, b| {
            // fn(b, a) binds $a := b, $b := a, so the body
            // `$a.field op $b.field` reads fields in that order.
            let lhs = hof_fast::get_field(b, &field);
            let rhs = hof_fast::get_field(a, &field);
            let val = hof_fast::eval_binary_simple(&lhs, op, &rhs)?;
            Ok(if val.to_boolean()? {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            })
        })?;
        return Ok(Value::Array(Rc::from(arr)));
    }

    // Sort with optional comparator.
    let arr = crate::try_sort::try_sort_by(arr, |a, b| match &comparator {
        Some(func) => {
            // Match Go: call fn(b, a) (swapped) and map true→Less, false→Equal.
            // JSONata comparator fn(a,b) returns true when a should sort AFTER b.
            // By calling fn(b,a): true means b sorts after a → a < b → Less.
            // false means equal or a sorts after b → preserve order → Equal.
            let val = call_function(func, &[b.clone(), a.clone()], a, env, arena)?;
            Ok(if val.to_boolean()? {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            })
        }
        None => {
            // Default: compare by value; remap T2008 to D3070 for the
            // $sort function context.
            match a.compare_order(b) {
                Ok(n) => Ok(n.cmp(&0)),
                Err(e) if e.code == "T2008" => Err(JsonataError::new("D3070", e.message.clone())),
                Err(e) => Err(e),
            }
        }
    })?;
    Ok(Value::Array(Rc::from(arr)))
}

type FastPredicate = Box<dyn Fn(&Value) -> JsonataResult<bool>>;

/// Build an inlined predicate for `$single`'s fast path, mirroring the
/// general path's short-circuit and error semantics.
fn fast_single_predicate(fast: &SimpleLambda) -> Option<FastPredicate> {
    match fast {
        SimpleLambda::FieldPredicate {
            field, op, literal, ..
        } => {
            let field = field.clone();
            let op = *op;
            let literal = literal.clone();
            Some(Box::new(move |item: &Value| {
                let fv = hof_fast::get_field(item, &field);
                hof_fast::eval_binary_simple(&fv, op, &literal)?.to_boolean()
            }))
        }
        SimpleLambda::CompoundPredicate {
            clauses, combiner, ..
        } => {
            let clauses = clauses.clone();
            let is_and = *combiner == BinaryOp::And;
            Some(Box::new(move |item: &Value| {
                for clause in &clauses {
                    let fv = hof_fast::get_field(item, &clause.field);
                    let pass = hof_fast::eval_binary_simple(&fv, clause.op, &clause.literal)?
                        .to_boolean()?;
                    if is_and && !pass {
                        return Ok(false);
                    }
                    if !is_and && pass {
                        return Ok(true);
                    }
                }
                Ok(is_and)
            }))
        }
        _ => None,
    }
}

pub fn fn_single(
    args: &[Value],
    _focus: &Value,
    env: &Rc<Environment>,
    arena: &AstArena,
) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$single: argument is required"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let arr = args[0].coerce_to_array();
    // Reference signature `<af?>`: same `f?` rule as `$sort` — a non-function
    // predicate is T0410, not an ignored argument that turns `$single` into
    // "the one and only element" (jsntrs-p0v.4).
    let func = match args.get(1) {
        None => None,
        Some(Value::Function(f)) => Some(f.clone()),
        Some(_) => {
            return Err(JsonataError::new(
                "T0410",
                "$single: argument 2 must be a function",
            ));
        }
    };

    // Fast path: field predicate or compound predicate
    if let Some(f) = &func
        && let Some(ref fast) = try_fast_lambda(f, arena)
        && let Some(pred) = fast_single_predicate(fast)
    {
        let mut matches = Vec::new();
        for (i, item) in arr.iter().enumerate() {
            env.poll_cancelled(i)?;
            if pred(item)? {
                matches.push(item.clone());
                if matches.len() > 1 {
                    return Err(JsonataError::new(
                        "D3138",
                        "$single: expected 1 match, found multiple",
                    ));
                }
            }
        }
        return match matches.len() {
            0 => Err(JsonataError::new(
                "D3139",
                "$single: expected 1 match, found 0",
            )),
            _ => Ok(matches.swap_remove(0)),
        };
    }

    let mut matches = Vec::new();
    let arr_val = Value::Array(arr.clone());
    for (i, item) in arr.iter().enumerate() {
        let keep = match &func {
            Some(f) => {
                let call_args = hof_args(f, item.clone(), || Value::Number(i as f64), &arr_val);
                call_function(f, &call_args, item, env, arena)?.to_boolean()?
            }
            None => true,
        };
        if keep {
            matches.push(item.clone());
            if matches.len() > 1 {
                return Err(JsonataError::new(
                    "D3138",
                    "$single: expected 1 match, found multiple",
                ));
            }
        }
    }
    match matches.len() {
        0 => Err(JsonataError::new(
            "D3139",
            "$single: expected 1 match, found 0",
        )),
        _ => Ok(matches.swap_remove(0)),
    }
}

/// Helper: sift a single object, passing (value, key, object) to the
/// predicate, trimmed to the predicate's declared arity.
fn sift_object(
    obj: &Rc<crate::value::ObjectMap>,
    func: &crate::evaluator::functions::FunctionValue,
    obj_val: &Value,
    env: &Rc<Environment>,
    arena: &AstArena,
) -> JsonataResult {
    let mut result = crate::value::ObjectMap::default();
    for (key, val) in obj.iter() {
        // Trimming matters for builtin predicates: $exists rejects a second
        // argument with T0410, so it must be called with the value alone.
        let call_args = hof_args(
            func,
            val.clone(),
            || Value::String(key.as_str().into()),
            obj_val,
        );
        let keep = call_function(func, &call_args, val, env, arena)?;
        if keep.to_boolean()? {
            result.insert(key.clone(), val.clone());
        }
    }
    if result.is_empty() {
        return Ok(Value::Undefined);
    }
    Ok(Value::Object(Rc::new(result)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::eval;
    use crate::parser::{Parser, process_ast};

    /// Helper: parse, process, and evaluate a full expression.
    fn eval_expr(src: &str) -> Value {
        let (mut arena, root) = Parser::parse(src).expect("parse failed");
        let root = process_ast(&mut arena, root).expect("process failed");
        let mut env = Environment::new();
        crate::stdlib::register_all(&mut env);
        let env = Rc::new(env);
        eval(&arena, root, &Value::Undefined, &env).expect("eval failed")
    }

    fn eval_err(src: &str) -> JsonataError {
        let (mut arena, root) = Parser::parse(src).expect("parse failed");
        let root = process_ast(&mut arena, root).expect("process failed");
        let mut env = Environment::new();
        crate::stdlib::register_all(&mut env);
        let env = Rc::new(env);
        match eval(&arena, root, &Value::Undefined, &env) {
            Err(e) => e,
            Ok(v) => panic!("expected error, got {v:?}"),
        }
    }

    fn assert_evals_to(src: &str, expected: &str) {
        let actual = eval_expr(src);
        let expected = eval_expr(expected);
        assert!(
            actual.deep_equal(&expected),
            "{src}: got {actual:?}, expected {expected:?}"
        );
    }

    #[test]
    fn map_passes_value_and_index() {
        assert_evals_to("$map([1,2,3], function($v){$v*2})", "[2,4,6]");
        assert_evals_to("$map([10,20], function($v,$i){$i})", "[0,1]");
    }

    /// Zero-arity lambdas must not panic in the lifted-dispatch analysis
    /// (it used to index params[0] unconditionally).
    #[test]
    fn map_accepts_zero_arity_lambda() {
        assert_evals_to("$map([1,2,3], function(){5})", "[5,5,5]");
    }

    #[test]
    fn filter_and_sift_select_matching_entries() {
        assert_evals_to("$filter([1,2,3,4], function($v){$v > 2})", "[3,4]");
        assert_evals_to(
            r#"$sift({"a":1, "b":10}, function($v){$v > 5})"#,
            r#"{"b":10}"#,
        );
    }

    #[test]
    fn reduce_folds_with_optional_init() {
        assert_evals_to("$reduce([1,2,3,4], function($p,$c){$p+$c})", "10");
        assert_evals_to("$reduce([1,2,3], function($p,$c){$p+$c}, 10)", "16");
    }

    #[test]
    fn each_maps_value_key_pairs() {
        assert_evals_to(
            r#"$each({"a":1, "b":2}, function($v,$k){$k & $v})"#,
            r#"["a1", "b2"]"#,
        );
    }

    /// jsntrs-p0v.2: `$sift`/`$each` must trim callback args to the callee's
    /// arity like `$map`/`$filter` do, or single-argument builtins reject the
    /// extra key/object arguments with T0410.
    #[test]
    fn sift_and_each_trim_args_for_builtin_callbacks() {
        assert_evals_to(r#"$sift({"a":1, "b":2}, $exists)"#, r#"{"a":1, "b":2}"#);
        assert_evals_to(r#"$each({"a":1, "b":2}, $exists)"#, "[true, true]");
        assert_evals_to(r#"$sift({"a":false, "b":1}, $not)"#, r#"{"a":false}"#);
        assert_evals_to(r#"$each({"a":false, "b":1}, $not)"#, "[true, false]");
    }

    /// jsntrs-p0v.2: a three-parameter `$each` callback receives the whole
    /// object as its third argument (it used to see undefined); a callback
    /// declaring more parameters than that gets undefined for the surplus.
    #[test]
    fn each_supplies_object_by_arity() {
        assert_evals_to(
            r#"$each({"a":1, "b":2}, function($v,$k,$o){$count($keys($o))})"#,
            "[2, 2]",
        );
        assert_evals_to(
            r#"$each({"a":1}, function($v,$k,$o){$o})"#,
            r#"{"a":1}"#, // single result collapses out of the sequence
        );
        assert_evals_to(
            r#"$each({"a":1, "b":2}, function($v,$k,$o,$x){[$v, $k, $string($x)]})"#,
            r#"[[1, "a"], [2, "b"]]"#,
        );
        assert_evals_to(r#"$each({"a":1, "b":2}, function(){"z"})"#, r#"["z", "z"]"#);
    }

    /// Sort is stable for equal keys (behavioral invariant #7) — on both
    /// the lifted fast path and the general comparator path.
    #[test]
    fn sort_is_stable_for_equal_keys() {
        // Same-field comparator → fast path.
        assert_evals_to(
            r#"$sort([{"k":1,"t":"a"},{"k":1,"t":"b"},{"k":0,"t":"c"}],
                       function($l,$r){$l.k > $r.k}).t"#,
            r#"["c","a","b"]"#,
        );
        // Complex rhs defeats the lift → general call path must agree.
        assert_evals_to(
            r#"$sort([{"k":1,"t":"a"},{"k":1,"t":"b"},{"k":0,"t":"c"}],
                       function($l,$r){$l.k > $r.k + 0}).t"#,
            r#"["c","a","b"]"#,
        );
    }

    #[test]
    fn single_returns_the_unique_match_or_errors() {
        assert_evals_to("$single([1,2,3], function($v){$v = 2})", "2");
        let err = eval_err("$single([1,2,3], function($v){$v > 1})");
        assert_eq!(err.code, "D3138");
        let err = eval_err("$single([1,2,3], function($v){$v > 9})");
        assert_eq!(err.code, "D3139");
    }
}
