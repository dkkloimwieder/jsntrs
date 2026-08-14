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

use crate::evaluator::{BuiltinFn, EnvAwareBuiltinFn, Environment, FunctionValue};
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

    /// Canonical `Rc` identities for the plain builtins whose result stands in
    /// for a JSONata *sequence*. The reference implementation builds these
    /// results with `createSequence()`, so the `[]` (keep-array) postfix
    /// applies to them; jsntrs collapses them to a bare value before
    /// returning, which loses that distinction, so the call site recovers it
    /// from the identity of the function it called (see
    /// [`returns_sequence`]). Thread-local for the same reason as
    /// `CANONICAL_PREPARED`.
    static CANONICAL_SEQUENCE: [(&'static str, Rc<BuiltinFn>); 2] = [
        ("keys", Rc::new(object::fn_keys)),
        ("lookup", Rc::new(object::fn_lookup)),
    ];

    /// Canonical `Rc` identities for the env-aware builtins whose result
    /// stands in for a JSONata sequence. Companion to `CANONICAL_SEQUENCE`.
    static CANONICAL_SEQUENCE_ENV: [(&'static str, Rc<EnvAwareBuiltinFn>); 2] = [
        ("filter", Rc::new(hof::fn_filter)),
        ("match", Rc::new(regex::fn_match)),
    ];
}

/// Does this function value's result stand in for a JSONata *sequence* that
/// jsntrs has already collapsed?
///
/// `$map`/`$each`/`$spread` keep an internal [`crate::value::Sequence`], so
/// they need no entry here; `$filter`, `$keys`, `$lookup` and `$match` return
/// a collapsed value, and only their identity distinguishes
/// `$filter([1,2,3], fn)[]` → `[2]` from `$sum([1,2])[]` → `3`.
///
/// Matching on `Rc` identity (not on the name at the call site) keeps
/// `$f := $filter; $f(a, fn)[]` working and keeps a user function bound over
/// a stdlib name out of it.
pub(crate) fn returns_sequence(func: &FunctionValue) -> bool {
    match func {
        FunctionValue::Builtin(rc) => {
            CANONICAL_SEQUENCE.with(|table| table.iter().any(|(_, f)| Rc::ptr_eq(rc, f)))
        }
        FunctionValue::EnvAwareBuiltin(rc) => {
            CANONICAL_SEQUENCE_ENV.with(|table| table.iter().any(|(_, f)| Rc::ptr_eq(rc, f)))
        }
        _ => false,
    }
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

/// Bind a sequence-returning builtin from the canonical table, so that
/// [`returns_sequence`] recognises the bound value by `Rc` identity.
fn bind_canonical_sequence(env: &mut Environment, name: &str) {
    let func = CANONICAL_SEQUENCE.with(|table| {
        let Some((_, f)) = table.iter().find(|(n, _)| *n == name) else {
            unreachable!("{name} must be in the CANONICAL_SEQUENCE table")
        };
        Rc::clone(f)
    });
    env.bind(
        name,
        Value::Function(Box::new(FunctionValue::Builtin(func))),
    );
}

/// Env-aware companion to [`bind_canonical_sequence`].
fn bind_canonical_sequence_env(env: &mut Environment, name: &str) {
    let func = CANONICAL_SEQUENCE_ENV.with(|table| {
        let Some((_, f)) = table.iter().find(|(n, _)| *n == name) else {
            unreachable!("{name} must be in the CANONICAL_SEQUENCE_ENV table")
        };
        Rc::clone(f)
    });
    env.bind(
        name,
        Value::Function(Box::new(FunctionValue::EnvAwareBuiltin(func))),
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
    bind_canonical_sequence(env, "keys");
    bind_builtin(env, "values", object::fn_values);
    bind_builtin(env, "spread", object::fn_spread);
    bind_builtin(env, "merge", object::fn_merge);
    bind_canonical_sequence(env, "lookup");
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
    bind_canonical_sequence_env(env, "filter");
    bind_env_builtin(env, "reduce", hof::fn_reduce);
    bind_env_builtin(env, "each", hof::fn_each);
    bind_env_builtin(env, "sift", hof::fn_sift);
    bind_env_builtin(env, "sort", hof::fn_sort);
    bind_env_builtin(env, "single", hof::fn_single);

    // ── Regex / Pattern ──────────────────────────────────────────────
    bind_canonical_sequence_env(env, "match");
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

    fn registered(name: &str) -> Value {
        let mut env = Environment::new();
        register_all(&mut env);
        env.lookup(name).unwrap()
    }

    /// The `[]` postfix on `$filter(…)`/`$keys(…)`/`$lookup(…)`/`$match(…)`
    /// depends on these staying bound from the canonical tables — a plain
    /// `bind_builtin` would allocate a fresh `Rc` and silently stop matching
    /// (jsntrs-e8l).
    #[test]
    fn sequence_builtins_bind_from_the_canonical_tables() {
        for name in ["filter", "keys", "lookup", "match"] {
            let Value::Function(f) = registered(name) else {
                panic!("${name} is not bound to a function")
            };
            assert!(returns_sequence(&f), "${name} lost its canonical identity");
        }
    }

    /// Everything else must not claim sequence-ness, or `$sum(x)[]` starts
    /// wrapping its scalar again.
    #[test]
    fn scalar_builtins_are_not_sequence_returning() {
        for name in [
            "sum", "round", "string", "count", "reduce", "single", "sift", "merge", "map", "each",
            "spread",
        ] {
            let Value::Function(f) = registered(name) else {
                panic!("${name} is not bound to a function")
            };
            assert!(!returns_sequence(&f), "${name} claims to return a sequence");
        }
    }
}
