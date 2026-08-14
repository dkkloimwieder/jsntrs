//! JSONata standard library: 60+ built-in functions.
//!
//! Port of Go `functions/` package.

mod array;
mod boolean;
pub mod datetime;
mod eval_fn;
mod format_integer;
pub(crate) mod format_number;
mod hof;
pub mod hof_fast;
mod number_words;
mod numeric;
mod object;
mod parse_integer;
pub mod regex;
mod string_funcs;
mod types;

use std::rc::Rc;

use crate::evaluator::{BuiltinFn, Environment, FunctionValue};
use crate::value::Value;

thread_local! {
    /// Canonical `Rc` identities for the builtins that have `PreparedState`
    /// fast paths in `hof_fast`. `register_all` binds clones of these, and
    /// `analyze_mapped_call` only prepares when the name resolves to the
    /// canonical `Rc` (`Rc::ptr_eq`) — so a custom function bound over the
    /// same name always takes the generic call path. Thread-local because
    /// `Value` (and thus environments) is `!Send`; comparisons never cross
    /// threads.
    static CANONICAL_PREPARED: [(&'static str, Rc<BuiltinFn>); 4] = [
        ("contains", Rc::new(string_funcs::fn_contains)),
        ("round", Rc::new(numeric::fn_round)),
        ("formatBase", Rc::new(numeric::fn_format_base)),
        ("formatNumber", Rc::new(format_number::fn_format_number)),
    ];
}

/// Pick the type-mismatch code for a builtin parameter that can be filled
/// from the context value.
///
/// The reference drives context injection from the signature itself: a `-`
/// after a parameter sets `param.context`, and when `signature.validate`
/// finds that parameter unmatched it substitutes the context value — but
/// only after testing the context's type against the parameter's regex. A
/// context value of the wrong type is **T0411** ("context value is not a
/// compatible type with argument N"), never the **T0410** reserved for an
/// argument the caller actually wrote.
///
/// jsntrs resolves the fallback inside each builtin instead of in a
/// signature gate, so the builtins that take a focus fallback route their
/// first-argument type error through this (jsntrs-p0v.18).
pub(crate) fn context_arg_code(from_focus: bool) -> &'static str {
    if from_focus { "T0411" } else { "T0410" }
}

/// Look up the canonical builtin `Rc` for a prepared-fast-path name.
pub(crate) fn canonical_prepared(name: &str) -> Option<Rc<BuiltinFn>> {
    CANONICAL_PREPARED.with(|table| {
        table
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, f)| Rc::clone(f))
    })
}

/// Bind a prepared-fast-path builtin from the canonical table.
fn bind_canonical(env: &mut Environment, name: &str) {
    let Some(func) = canonical_prepared(name) else {
        unreachable!("{name} must be in the CANONICAL_PREPARED table")
    };
    env.bind(
        name,
        Value::Function(Box::new(FunctionValue::Builtin(func))),
    );
}

thread_local! {
    static CACHED_ROOT_ENV: std::rc::Rc<Environment> = {
        let mut env = Environment::new();
        register_all(&mut env);
        std::rc::Rc::new(env)
    };
}

/// The thread-cached stdlib root environment.
///
/// Public evaluations build a per-evaluation child of this root
/// ([`Environment::new_eval_child`](crate::evaluator::Environment)) instead
/// of re-registering ~70 builtins per call. Invariants: the root is never
/// bound into after construction, never handed out to user code, and never
/// torn down — `teardown_cycles` only touches eval-scope closure
/// environments, which are descendants of the per-eval child.
pub(crate) fn cached_root_env() -> std::rc::Rc<Environment> {
    CACHED_ROOT_ENV.with(std::rc::Rc::clone)
}

/// Register all built-in functions into an environment.
pub fn register_all(env: &mut Environment) {
    // ~70 builtins land below; one up-front reserve avoids rehash churn.
    env.reserve_bindings(70);
    // ── String ──────────────────────────────────────────────────────
    bind_signed_builtin(env, "string", string_funcs::fn_string, "x?b?");
    bind_builtin(env, "length", string_funcs::fn_length);
    bind_builtin(env, "substring", string_funcs::fn_substring);
    bind_builtin(env, "substringBefore", string_funcs::fn_substring_before);
    bind_builtin(env, "substringAfter", string_funcs::fn_substring_after);
    bind_signed_builtin(env, "uppercase", string_funcs::fn_uppercase, "s?");
    bind_signed_builtin(env, "lowercase", string_funcs::fn_lowercase, "s?");
    bind_builtin(env, "trim", string_funcs::fn_trim);
    bind_builtin(env, "pad", string_funcs::fn_pad);
    bind_canonical(env, "contains");
    bind_builtin(env, "split", string_funcs::fn_split);
    bind_builtin(env, "join", string_funcs::fn_join);
    bind_builtin(env, "base64encode", string_funcs::fn_base64_encode);
    bind_builtin(env, "base64decode", string_funcs::fn_base64_decode);
    bind_builtin(env, "encodeUrl", string_funcs::fn_encode_url);
    bind_builtin(
        env,
        "encodeUrlComponent",
        string_funcs::fn_encode_url_component,
    );
    bind_builtin(env, "decodeUrl", string_funcs::fn_decode_url);
    bind_builtin(
        env,
        "decodeUrlComponent",
        string_funcs::fn_decode_url_component,
    );

    // ── Numeric ─────────────────────────────────────────────────────
    bind_builtin(env, "number", numeric::fn_number);
    bind_builtin(env, "abs", numeric::fn_abs);
    bind_builtin(env, "floor", numeric::fn_floor);
    bind_builtin(env, "ceil", numeric::fn_ceil);
    bind_canonical(env, "round");
    bind_builtin(env, "power", numeric::fn_power);
    bind_builtin(env, "sqrt", numeric::fn_sqrt);
    bind_builtin(env, "random", numeric::fn_random);
    bind_signed_builtin(env, "sum", numeric::fn_sum, "a<n>");
    bind_signed_builtin(env, "max", numeric::fn_max, "a<n>");
    bind_signed_builtin(env, "min", numeric::fn_min, "a<n>");
    bind_signed_builtin(env, "average", numeric::fn_average, "a<n>");
    bind_canonical(env, "formatBase");
    bind_canonical(env, "formatNumber");
    bind_builtin(env, "formatInteger", format_integer::fn_format_integer);
    bind_builtin(env, "parseInteger", parse_integer::fn_parse_integer);

    // ── Array ───────────────────────────────────────────────────────
    bind_builtin(env, "count", array::fn_count);
    bind_builtin(env, "append", array::fn_append);
    bind_builtin(env, "reverse", array::fn_reverse);
    bind_builtin(env, "shuffle", array::fn_shuffle);
    bind_builtin(env, "distinct", array::fn_distinct);
    bind_builtin(env, "flatten", array::fn_flatten);
    bind_builtin(env, "zip", array::fn_zip);

    // ── Object ──────────────────────────────────────────────────────
    bind_builtin(env, "keys", object::fn_keys);
    bind_builtin(env, "values", object::fn_values);
    bind_builtin(env, "spread", object::fn_spread);
    bind_builtin(env, "merge", object::fn_merge);
    bind_builtin(env, "lookup", object::fn_lookup);
    bind_builtin(env, "error", object::fn_error);

    // ── Boolean ─────────────────────────────────────────────────────
    bind_signed_builtin(env, "boolean", boolean::fn_boolean, "x?");
    bind_builtin(env, "not", boolean::fn_not);
    bind_builtin(env, "exists", boolean::fn_exists);

    // ── Type / Misc ─────────────────────────────────────────────────
    bind_builtin(env, "type", types::fn_type_of);
    bind_builtin(env, "assert", types::fn_assert);

    // ── HOF (env-aware) ─────────────────────────────────────────────
    bind_env_builtin(env, "map", hof::fn_map);
    bind_env_builtin(env, "filter", hof::fn_filter);
    bind_env_builtin(env, "reduce", hof::fn_reduce);
    bind_env_builtin(env, "each", hof::fn_each);
    bind_env_builtin(env, "sift", hof::fn_sift);
    bind_env_builtin(env, "sort", hof::fn_sort);
    bind_env_builtin(env, "single", hof::fn_single);

    // ── Regex / Pattern ──────────────────────────────────────────────
    bind_env_builtin(env, "match", regex::fn_match);
    bind_env_builtin(env, "replace", regex::fn_replace);
    bind_env_builtin(env, "eval", eval_fn::fn_eval);

    // ── DateTime ────────────────────────────────────────────────────
    bind_builtin(env, "now", datetime::fn_now);
    bind_builtin(env, "millis", datetime::fn_millis);
    bind_builtin(env, "fromMillis", datetime::fn_from_millis);
    bind_builtin(env, "toMillis", datetime::fn_to_millis);
}

fn bind_builtin(
    env: &mut Environment,
    name: &str,
    f: fn(&[Value], &Value) -> crate::error::JsonataResult,
) {
    let func: Rc<BuiltinFn> = Rc::new(f);
    env.bind(
        name,
        Value::Function(Box::new(FunctionValue::Builtin(func))),
    );
}

fn bind_signed_builtin(
    env: &mut Environment,
    name: &str,
    f: fn(&[Value], &Value) -> crate::error::JsonataResult,
    signature: &str,
) {
    let func: Rc<BuiltinFn> = Rc::new(f);
    // Registration signatures are static strings; a parse failure is a
    // programming bug, not a runtime condition.
    let specs = match crate::evaluator::parse_signature(signature) {
        Ok(specs) => specs,
        Err(e) => unreachable!("builtin ${name} has an invalid signature {signature:?}: {e}"),
    };
    env.bind(
        name,
        Value::Function(Box::new(FunctionValue::SignedBuiltin {
            func,
            signature: specs.into(),
        })),
    );
}

fn bind_env_builtin(
    env: &mut Environment,
    name: &str,
    f: fn(
        &[Value],
        &Value,
        &Rc<Environment>,
        &crate::parser::AstArena,
    ) -> crate::error::JsonataResult,
) {
    let func: Rc<crate::evaluator::EnvAwareBuiltinFn> = Rc::new(f);
    env.bind(
        name,
        Value::Function(Box::new(FunctionValue::EnvAwareBuiltin(func))),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Evaluate `src` without the API-boundary collapse, so an internal
    /// `Value::Sequence` result is still visible.
    fn eval_raw(src: &str) -> Value {
        let (mut arena, root) = crate::parser::Parser::parse(src).expect("parse failed");
        let root = crate::parser::process_ast(&mut arena, root).expect("process failed");
        let mut env = Environment::new();
        register_all(&mut env);
        let env = Rc::new(env);
        crate::evaluator::eval_no_stack_check(&arena, root, &Value::Undefined, &env)
            .expect("eval failed")
    }

    /// The `[]` postfix on `$filter(…)`/`$keys(…)`/`$lookup(…)`/`$match(…)`
    /// needs those builtins to hand back an uncollapsed sequence; the
    /// canonical-identity tables that used to stand in for that are gone
    /// (jsntrs-e8l, jsntrs-p0v.6).
    #[test]
    fn sequence_builtins_return_an_uncollapsed_sequence() {
        for expr in [
            r#"$keys({"a": 1})"#,
            r#"$lookup([{"a": 1}], "a")"#,
            "$filter([1, 2], function($v) { $v > 1 })",
            r#"$match("abc", /b/)"#,
            "$map([1], function($v) { $v })",
            r#"$each({"a": 1}, function($v) { $v })"#,
            r#"$spread({"a": 1})"#,
        ] {
            let value = eval_raw(expr);
            assert!(
                value.is_sequence(),
                "{expr} returned {value:?}, not a sequence"
            );
        }
    }

    /// Everything else must not claim sequence-ness, or `$sum(x)[]` starts
    /// wrapping its scalar again.
    #[test]
    fn scalar_builtins_do_not_return_a_sequence() {
        for expr in [
            "$sum([1, 2])",
            "$round(1.5)",
            r#"$string("x")"#,
            "$count([1, 2])",
            "$reduce([1, 2], function($a, $b) { $a + $b })",
            "$single([1], function($v) { $v = 1 })",
            r#"$sift({"a": 1}, function($v) { $v > 0 })"#,
            r#"$merge([{"a": 1}])"#,
            "$distinct([1, 1])",
        ] {
            let value = eval_raw(expr);
            assert!(
                !value.is_sequence(),
                "{expr} returned a sequence: {value:?}"
            );
        }
    }
}
