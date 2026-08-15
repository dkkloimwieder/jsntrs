//! Differential fast-path tests.
//!
//! Every pair here is evaluated twice — fast paths enabled, then disabled
//! via `fast_path::testing` — and both results must be identical, including
//! error codes. This guards against the fast-path layer diverging from the
//! general evaluator (gnata-bec.5).
//!
//! # Tiers
//!
//! A pair only compares two *different* implementations when a lift actually
//! fires; otherwise both runs are the general evaluator and the case passes
//! vacuously. `fast_path_testing::hits()` counts the lifts taken (one per
//! lift decision, at exactly the sites the disable flag guards), so the two
//! lists below can be told apart and enforced:
//!
//! - [`CASES`] — at least one lift must fire. A case that stops lifting is
//!   reported by name, not silently downgraded to a no-op.
//! - [`GENERAL_ONLY_CASES`] — no lift may fire. Either analysis declines the
//!   shape (`$sum(prices)[]`, `$v.x[]` in a lambda body, a typed lambda) or
//!   the lifted evaluator inspects the data and hands the whole expression
//!   back (`$sum` over nested arrays, `$trim` on a number). Both are worth
//!   pinning — the general result is the answer the lift has to reproduce if
//!   it ever stops declining — but neither exercises a second implementation
//!   today.
//!
//! Membership is enforced in both directions. If a `GENERAL_ONLY_CASES` pair
//! starts lifting, that is not automatically a bug: check that fast and
//! general still agree (the same test run does), then move the pair to
//! `CASES`.
//!
//! # Lanes
//!
//! Each pair runs through `evaluate` (JSON text, so pure paths take the
//! tape lift), `evaluate_value` (pre-parsed input) and `evaluate_with_env`
//! under an empty custom environment. The last lane exists because bindings
//! change which lifts are legal: `eval_fast_with_bindings` drops the whole
//! function class, since `$sum(path)` resolves its callee by name at compile
//! time and a caller could bind over that name (jsntrs-6wr.4). The lane
//! pins that merely *carrying* an environment never changes an answer. It
//! cannot pin the override behaviour itself — an override deliberately
//! changes the result, so there is nothing to compare against the general
//! run — and that coverage lives in unit tests instead:
//! `expression.rs::custom_override_wins_over_top_level_function_fast_path`,
//! `expression.rs::binding_free_fast_paths_survive_a_custom_env` and
//! `stream.rs::stream_custom_func_overrides_builtin_fast_path`.
//!
//! # Group-by postfixes (jsntrs-6wr.9)
//!
//! The `{…}` postfix used to be silently dropped when it sat on a lifted
//! mapped call or on a lifted field access, so seven shapes were excluded
//! from both lists. `analyze_mapped_call` and `extract_param_field` now
//! decline any node carrying one; all seven live in [`GENERAL_ONLY_CASES`]
//! alongside the neighbours that already agreed, and nothing in this file
//! is excluded from both lists any more.

use std::rc::Rc;

use jsntrs::{Environment, Expression, JsonataError, Value, new_custom_env};

/// Items with clean, homogeneous fields — exercises happy paths.
const CLEAN: &str = r#"{"items": [
    {"x": 3, "y": 1.5, "name": "Alpha"},
    {"x": 1, "y": 2.5, "name": "beta"},
    {"x": 2.5, "y": 0, "name": "Gamma"}
]}"#;

/// Items with missing fields, nulls, and mixed types — exercises
/// error paths and undefined propagation.
const HOSTILE: &str = r#"{"items": [
    {"x": 3, "y": 1, "name": "Alpha"},
    {"x": "str", "name": "beta"},
    {"x": true},
    {"x": null, "name": 7},
    {"y": 2},
    {}
]}"#;

/// Numbers including zeros and extremes — division/overflow behavior.
const NUMS: &str = r#"{"items": [
    {"x": 10, "y": 2},
    {"x": 1, "y": 0},
    {"x": 0, "y": 5},
    {"x": 1e308, "y": 1e308},
    {"x": -2.5, "y": 0.5}
]}"#;

/// Signed zeros next to ordinary numbers. `$formatNumber` decides the
/// sub-picture and strips the sign in two places — the builtin and the
/// mapped-call `PreparedState` — so -0.0 pins them against each other
/// (jsntrs-p0v.26).
const SIGNED_ZEROS: &str = r#"{"items": [
    {"x": -0.0},
    {"x": 0.0},
    {"x": -1.5}
]}"#;

/// Nested document for pure-path / comparison / function fast paths.
const NESTED: &str = r#"{"a": {"b": {"c": 42, "s": "hello"}},
    "arr": [{"v": 1}, {"v": 2}, {"v": 3}],
    "mixed": [{"v": 1}, {"w": 2}, {"v": null}],
    "empty": []}"#;

/// Arrays of arrays — auto-map must recurse per level, and singleton
/// collapse interacts with the one-level flatten (gnata-dx5.4).
const NESTED_ARRAYS: &str = r#"{
    "a": [[{"b": 1}]],
    "c": [{"d": {"e": [1]}}],
    "f": [[{"g": {"h": [1, 2]}}], [{"g": {"h": 3}}]],
    "m": [[{"v": 1}, {"v": 2}], [{"v": 3}]],
    "p": [{"q": [[1, 2]]}],
    "s": [[1, 2], [3]]}"#;

/// Field present but holding an empty array — the path resolves to []
/// (defined), not undefined, which flips $exists (gnata-dx5.5).
const EMPTY_FIELDS: &str = r#"{
    "a": [{"b": []}],
    "half": [{"b": []}, {"b": 1}],
    "none": [{}],
    "deep": [[{"b": []}]]}"#;

/// Strings that parse as non-finite f64 — $number must raise D3030 on
/// every path instead of returning Infinity/NaN (gnata-dx5.9).
const NONFINITE_STRINGS: &str = r#"{
    "inf": "Infinity",
    "neginf": "-Infinity",
    "nan": "NaN",
    "num": "1e2"}"#;

/// $distinct dedup shapes: key-reordered objects are deep_equal, -0 and 0
/// compare equal, mixed scalar/object arrays defer (gnata-dx5.10).
const DISTINCT_SHAPES: &str = r#"{
    "objs": [{"x": 1, "y": 2}, {"y": 2, "x": 1}, {"x": 1, "y": 3}],
    "zeros": [-0.0, 0, 0.0],
    "scalars": [1, "1", 1, true, "a", "a", null, null, 2.5, 2.5],
    "mixed": [1, {"x": 1}, 1, {"x": 1}]}"#;

/// Shapes for `$func([path])`: the array constructor is transparent only to
/// the aggregates, whose signatures flatten the argument anyway. Everything
/// else observes the wrapper — a wrong type, a wrong string, or T0410
/// (jsntrs-6wr.1).
const ARRAY_ARG: &str = r#"{
    "n": {"v": 42},
    "s": {"v": "HELLO"},
    "ns": {"v": "7"},
    "sq": {"v": 9},
    "miss": {"other": 1},
    "obj": {"v": {"k": 1}},
    "arr": {"v": [3, 1, 2]},
    "seq": [{"v": 1}, {"v": 2}],
    "emptyf": [{"v": []}]}"#;

/// Fixture for `$func(path)[]` / `$func(path){…}`: the `[]` keep-array and
/// `{…}` group-by postfixes bind to the call node itself, and the general
/// path honours both (jsntrs-6wr.2). Also the argument fixture for the
/// collection builtins ($values/$reverse/$flatten/$shuffle): `single` and
/// `none` keep `$shuffle` comparable, since a longer array would permute
/// differently on each side.
const POSTFIX: &str = r#"{
    "prices": [10, 20, 30],
    "single": [7],
    "none": [],
    "name": "Alice",
    "obj": {"x": 1, "y": 2}}"#;

/// Fields holding arrays and singletons, for keep-array (`[]`) lambda
/// bodies: `$v.x[]` must stay wrapped, so the lifted analyzers have to
/// bail instead of returning the collapsed value (jsntrs-6wr.3).
const KEEP_ARRAY: &str = r#"{
    "items": [{"x": [3], "name": "a"},
              {"x": [1, 2], "name": "b"},
              {"x": 5, "name": "c"},
              {"name": "d"}],
    "obj": {"a": {"x": [3]}, "b": {"x": [1]}}}"#;

/// Items for lambda callees invoked as path steps and from lambda bodies:
/// a call that supplies fewer args than the lambda declares params gets the
/// context item prepended on the path-step route (jsntrs-6wr.5).
const LAMBDA_ARITY: &str = r#"{
    "items": [{"x": "p"}, {"x": "q", "y": "z"}],
    "one": {"x": "p"}}"#;

/// Items for reversed literal comparisons: `miss` has no `x` at all, so a
/// `null`/boolean literal on the left of an ordering operator raises T2010
/// on the general path while the swapped form would see undefined
/// (jsntrs-6wr.6).
const COMPARE_LITS: &str = r#"{
    "miss": [{"y": 1}, {"y": 2}],
    "items": [{"x": 3}, {"y": 1}, {"x": "s"}, {"x": null}, {"x": true}]}"#;

/// Fields feeding signed builtins in a lifted `.( … )` step (jsntrs-6wr.7).
const SIGNED: &str = r#"{
    "items": [{"x": 3, "name": "alice"}, {"x": "s"}, {"y": 1}]}"#;

/// Number literals `JSON.parse` accepts but simd-json refuses — an integer
/// past `u64` range and an exponent that overflows to `Infinity`. The tape
/// lift parses the bytes itself, so it declines the whole document and the
/// `evaluate` lane has to reach the same answer as the pre-parsed lanes
/// (jsntrs-ztg).
const NUMBER_LIMITS: &str = r#"{
    "big": 123456789012345678901,
    "inf": 1e400,
    "neginf": -1e400,
    "ok": 1.5,
    "items": [{"x": 123456789012345678901}, {"x": 1e400}, {"x": 2}]}"#;

/// (expression, data) pairs where a lift **must** fire: every one of these
/// compares the fast implementation against the general evaluator.
const CASES: &[(&str, &str)] = &[
    // ── Pure paths over nested arrays (gnata-dx5.4) ──
    ("a.b", NESTED_ARRAYS),
    ("c.d.e", NESTED_ARRAYS),
    ("f.g.h", NESTED_ARRAYS),
    ("m.v", NESTED_ARRAYS),
    ("p.q", NESTED_ARRAYS),
    ("s", NESTED_ARRAYS),
    ("$count(a.b)", NESTED_ARRAYS),
    ("$count(m.v)", NESTED_ARRAYS),
    ("$count(p.q)", NESTED_ARRAYS),
    ("$exists(a.b)", NESTED_ARRAYS),
    // ── Field found but empty: [] vs undefined (gnata-dx5.5) ──
    ("a.b", EMPTY_FIELDS),
    ("half.b", EMPTY_FIELDS),
    ("none.b", EMPTY_FIELDS),
    ("deep.b", EMPTY_FIELDS),
    ("$exists(a.b)", EMPTY_FIELDS),
    ("$exists(half.b)", EMPTY_FIELDS),
    ("$exists(none.b)", EMPTY_FIELDS),
    ("$exists(deep.b)", EMPTY_FIELDS),
    ("$count(a.b)", EMPTY_FIELDS),
    ("$max(a.b)", EMPTY_FIELDS),
    ("$type(a.b)", EMPTY_FIELDS),
    // ── SimpleLambda::FieldAccess ──
    ("$map(items, function($v){$v.x})", CLEAN),
    ("$map(items, function($v){$v.x})", HOSTILE),
    // ── SimpleLambda::FieldPredicate (each relational op, both data sets) ──
    ("$filter(items, function($v){$v.x > 2})", CLEAN),
    ("$filter(items, function($v){$v.x > 2})", HOSTILE),
    ("$filter(items, function($v){$v.x < 2})", CLEAN),
    ("$filter(items, function($v){$v.x < 2})", HOSTILE),
    ("$filter(items, function($v){$v.x >= 2.5})", CLEAN),
    ("$filter(items, function($v){$v.x <= 2.5})", HOSTILE),
    ("$filter(items, function($v){$v.x = 3})", CLEAN),
    ("$filter(items, function($v){$v.x = 3})", HOSTILE),
    ("$filter(items, function($v){$v.x != 3})", CLEAN),
    ("$filter(items, function($v){$v.x != 3})", HOSTILE),
    ("$filter(items, function($v){$v.name = \"Alpha\"})", CLEAN),
    (
        "$filter(items, function($v){$v.name != \"Alpha\"})",
        HOSTILE,
    ),
    ("$filter(items, function($v){$v.name > \"B\"})", HOSTILE),
    // ── SimpleLambda::TwoFieldPredicate ──
    ("$filter(items, function($v){$v.x = $v.y})", CLEAN),
    ("$filter(items, function($v){$v.x = $v.y})", HOSTILE),
    ("$filter(items, function($v){$v.x > $v.y})", CLEAN),
    ("$filter(items, function($v){$v.x > $v.y})", HOSTILE),
    ("$filter(items, function($v){$v.x > $v.y})", NUMS),
    // ── SimpleLambda::CompoundPredicate (and/or, short-circuit order) ──
    ("$filter(items, function($v){$v.x > 1 and $v.x < 3})", CLEAN),
    (
        "$filter(items, function($v){$v.x > 1 and $v.x < 3})",
        HOSTILE,
    ),
    ("$filter(items, function($v){$v.x > 2 or $v.y > 2})", CLEAN),
    (
        "$filter(items, function($v){$v.x > 2 or $v.y > 2})",
        HOSTILE,
    ),
    (
        "$filter(items, function($v){$v.x > 100 and $v.name > 5})",
        CLEAN,
    ),
    // `&` is not an eval_binary_simple op, but the body is a ConcatTemplate,
    // so this one does lift (its `and`/`or`/`in`/`**` neighbours do not —
    // they sit in GENERAL_ONLY_CASES).
    ("$filter(items, function($v){$v.name & \"!\"})", CLEAN),
    // ── Reversed literal-op-field shapes (gnata-dx5.2 — non-commutative
    //    arithmetic used to lift with swapped operands; the comparisons
    //    below still lift, with the mirrored op) ──
    ("$filter(items, function($v){2 > $v.x})", CLEAN),
    ("$filter(items, function($v){2 > $v.x})", HOSTILE),
    ("$filter(items, function($v){3 = $v.x})", HOSTILE),
    (
        "$filter(items, function($v){2 > $v.x or $v.y = 2})",
        HOSTILE,
    ),
    // ── SimpleLambda::SortComparator / SortComparatorOp ──
    ("$sort(items, function($a, $b){$a.x > $b.x})", CLEAN),
    ("$sort(items, function($a, $b){$a.x > $b.x})", HOSTILE),
    ("$sort(items, function($a, $b){$a.x < $b.x})", CLEAN),
    ("$sort(items, function($a, $b){$a.x >= $b.x})", CLEAN),
    ("$sort(items, function($a, $b){$a.x <= $b.x})", NUMS),
    ("$sort(items, function($a, $b){$a.name > $b.name})", CLEAN),
    ("$sort(items, function($a, $b){$a.name > $b.name})", HOSTILE),
    // ── SimpleLambda::ReduceAccum / ReduceCompoundAccum ──
    ("$reduce(items, function($acc, $v){$acc + $v.x}, 0)", CLEAN),
    (
        "$reduce(items, function($acc, $v){$acc + $v.x}, 0)",
        HOSTILE,
    ),
    ("$reduce(items, function($acc, $v){$acc + $v.x}, 0)", NUMS),
    ("$reduce(items, function($acc, $v){$acc * $v.x}, 1)", CLEAN),
    ("$reduce(items, function($acc, $v){$acc - $v.x}, 100)", NUMS),
    // ReduceCompoundAccum. The shape had no live coverage at all until
    // jsntrs-6wr.8 added the bare `$acc op $v.f op $v.g` rows below.
    (
        "$reduce(items, function($acc, $v){$acc + $v.x * $v.y}, 0)",
        CLEAN,
    ),
    (
        "$reduce(items, function($acc, $v){$acc + $v.x * $v.y}, 0)",
        NUMS,
    ),
    (
        "$reduce(items, function($acc, $v){$acc + $v.x * $v.y}, 0)",
        HOSTILE,
    ),
    (
        "$reduce(items, function($acc, $v){$acc + $v.x / $v.y}, 0)",
        NUMS,
    ),
    // Parenthesising the inner term wraps it in a single-expression Block.
    // `*` and `/` bind tighter than `+`, so the parenthesised spelling is
    // the same tree as its bare twin above once the (empty, unobservable)
    // block frame is dropped — `unwrap_paren_block` drops it, so these lift
    // too and must give the same answer and the same error code
    // (jsntrs-5sj). 6wr.8 had pinned the first three as declines in
    // GENERAL_ONLY_CASES; they moved here. The declines that survive the
    // fix — a block that is more than its inner value — stayed there.
    (
        "$reduce(items, function($acc, $v){$acc + ($v.x * $v.y)}, 0)",
        CLEAN,
    ),
    (
        "$reduce(items, function($acc, $v){$acc + ($v.x * $v.y)}, 0)",
        NUMS,
    ),
    (
        "$reduce(items, function($acc, $v){$acc + ($v.x / $v.y)}, 0)",
        NUMS,
    ),
    (
        "$reduce(items, function($acc, $v){$acc + ($v.x * $v.y)}, 0)",
        HOSTILE,
    ),
    // Nested parens peel all the way down to the same Binary.
    (
        "$reduce(items, function($acc, $v){$acc + (($v.x * $v.y))}, 0)",
        CLEAN,
    ),
    // ── SimpleLambda::ConcatTemplate ──
    (
        "$map(items, function($v){$v.name & \"-\" & $string($v.x)})",
        CLEAN,
    ),
    (
        "$map(items, function($v){$v.name & \"-\" & $string($v.x)})",
        HOSTILE,
    ),
    (
        "$map(items, function($v){$lowercase($v.name) & $uppercase($v.name)})",
        CLEAN,
    ),
    (
        "$map(items, function($v){$substring($v.name, 1, 3) & \"!\"})",
        CLEAN,
    ),
    (
        "$map(items, function($v){$substring($v.name, 1, 3) & \"!\"})",
        HOSTILE,
    ),
    // ── Mapped-call lift (path-step form, PreparedState) ──
    ("items.$round(x)", CLEAN),
    ("items.$round(x)", HOSTILE),
    ("items.$round(x, 1)", NUMS),
    ("items.$formatNumber(x, \"#,##0.00\")", CLEAN),
    ("items.$formatNumber(x, \"#,##0.00\")", NUMS),
    ("items.$formatNumber(x, \"9,9,99.99\")", SIGNED_ZEROS),
    ("items.$formatNumber(x, \"0.00;(0.00)\")", SIGNED_ZEROS),
    ("items.$contains(name, \"a\")", CLEAN),
    ("items.$contains(name, \"a\")", HOSTILE),
    ("items.$formatBase(x, 16)", CLEAN),
    ("items.$substring(name, 0, 2)", CLEAN),
    // $pad/$split once had PreparedState twins that exec never matched
    // (deleted in gnata-dx5.12) — keep both routed via generic dispatch.
    ("items.$pad(name, 8)", CLEAN),
    ("items.$pad(name, 8, \"*\")", HOSTILE),
    ("items.$split(name, \"a\")", CLEAN),
    ("items.$split(name, \"a\", 1)", HOSTILE),
    ("items.$lowercase(name)", CLEAN),
    ("items.$lowercase(name)", HOSTILE),
    ("items.$uppercase(name)", CLEAN),
    ("items.$string(x)", HOSTILE),
    // ── ^() sort operator fast path ──
    ("items^(x)", CLEAN),
    ("items^(>x)", CLEAN),
    ("items^(x)", HOSTILE),
    ("items^(x, y)", NUMS),
    ("items^(>name)", CLEAN),
    // ── Expression-level FastPath::PurePath ──
    ("a.b.c", NESTED),
    ("a.b.s", NESTED),
    ("a.b.missing", NESTED),
    ("arr.v", NESTED),
    ("mixed.v", NESTED),
    ("empty.v", NESTED),
    ("a.b", NESTED),
    // ── Expression-level FastPath::Comparison ──
    ("a.b.c = 42", NESTED),
    ("a.b.c != 42", NESTED),
    ("a.b.c = \"42\"", NESTED),
    ("a.b.s = \"hello\"", NESTED),
    ("a.b.missing = 1", NESTED),
    // ── Expression-level FastPath::Function ──
    ("$sum(arr.v)", NESTED),
    ("$count(arr)", NESTED),
    ("$count(empty)", NESTED),
    ("$exists(a.b.c)", NESTED),
    ("$exists(a.b.missing)", NESTED),
    ("$distinct(arr.v)", NESTED),
    ("$keys(a.b)", NESTED),
    ("$sqrt(a.b.c)", NESTED),
    ("$string(a.b.c)", NESTED),
    ("$max(arr.v)", NESTED),
    ("$min(a.b.missing)", NESTED),
    ("$average(arr.v)", NESTED),
    ("$length(a.b.s)", NESTED),
    ("$abs(a.b.c)", NESTED),
    ("$ceil(a.b.c)", NESTED),
    ("$number(a.b.c)", NESTED),
    ("$boolean(a.b.missing)", NESTED),
    ("$not(a.b.missing)", NESTED),
    ("$trim(a.b.s)", NESTED),
    ("$contains(a.b.s, \"ell\")", NESTED),
    ("$lowercase(a.b.s)", NESTED),
    ("$exists(mixed.v)", NESTED),
    ("$count(mixed.v)", NESTED),
    ("$count(a.b.missing)", NESTED),
    ("$number(num)", NONFINITE_STRINGS),
    // FuncFastKind::{Values, Reverse, Shuffle, Flatten} had no live case at
    // all before jsntrs-6wr.8 — every mention of them sat on a shape that
    // declines the lift. $shuffle is randomised, so only the 0/1-element and
    // undefined inputs can be compared value-for-value.
    ("$values(a.b)", NESTED),
    ("$values(obj)", POSTFIX),
    ("$reverse(arr.v)", NESTED),
    ("$reverse(prices)", POSTFIX),
    ("$reverse(single)", POSTFIX),
    ("$reverse(none)", POSTFIX),
    ("$reverse(missing)", POSTFIX),
    ("$flatten(s)", NESTED_ARRAYS),
    ("$flatten(prices)", POSTFIX),
    ("$flatten(none)", POSTFIX),
    ("$flatten(missing)", POSTFIX),
    ("$shuffle(single)", POSTFIX),
    ("$shuffle(none)", POSTFIX),
    ("$shuffle(missing)", POSTFIX),
    ("$distinct(none)", POSTFIX),
    ("$count(none)", POSTFIX),
    // ── Shadowed callee names in lifted $map/.() dispatch (gnata-dx5.7) ──
    // Same-scope shadow: prepared-by-name exec must not engage.
    (
        "( $round := function($x){ 99 }; $map(arr, function($v){ $round($v.v) }) )",
        NESTED,
    ),
    // Shadow only in the lambda's closure, not at the $map call site.
    (
        "( $f := ( $round := function($x){ 99 }; function($v){ $round($v.v) } ); $map(arr, $f) )",
        NESTED,
    ),
    // Shadow only at the call site: the closure's builtin must win.
    (
        "( $f := function($v){ $round($v.v) }; ( $round := function($x){ 99 }; $map(arr, $f) ) )",
        NESTED,
    ),
    // Shadow visible from a .() path function step.
    ("( $round := function($x){ 99 }; arr.$round(v) )", NESTED),
    // ── Array items auto-map through lifted HOF field access (gnata-dx5.8) ──
    ("$map(m, function($v){$v.v})", NESTED_ARRAYS),
    ("$map(a, function($v){$v.b})", NESTED_ARRAYS),
    ("$filter(m, function($v){$v.v > 1})", NESTED_ARRAYS),
    ("$filter(m, function($v){$v.v = 3})", NESTED_ARRAYS),
    ("$map(m, function($v){\"id-\" & $v.v})", NESTED_ARRAYS),
    ("$map(m, function($v){$round($v.v)})", NESTED_ARRAYS),
    ("$sort(m, function($a,$b){$a.v > $b.v})", NESTED_ARRAYS),
    ("$map(deep, function($v){$v.b})", EMPTY_FIELDS),
    // ── $distinct must dedupe like deep_equal (gnata-dx5.10) ──
    ("$distinct(zeros)", DISTINCT_SHAPES),
    ("$distinct(scalars)", DISTINCT_SHAPES),
    // ── $func([path]) for the aggregates, which may drop the array
    //    constructor because their signatures flatten it anyway
    //    (jsntrs-6wr.1). Every kind × every argument shape that lifts. ──
    ("$count([n.v])", ARRAY_ARG),
    ("$count([miss.v])", ARRAY_ARG),
    ("$count([seq.v])", ARRAY_ARG),
    ("$count([emptyf.v])", ARRAY_ARG),
    ("$count([arr.v])", ARRAY_ARG),
    ("$count([obj.v])", ARRAY_ARG),
    ("$sum([n.v])", ARRAY_ARG),
    ("$sum([arr.v])", ARRAY_ARG),
    ("$sum([seq.v])", ARRAY_ARG),
    ("$sum([sq.v])", ARRAY_ARG),
    ("$max([n.v])", ARRAY_ARG),
    ("$max([arr.v])", ARRAY_ARG),
    ("$max([seq.v])", ARRAY_ARG),
    ("$max([miss.v])", ARRAY_ARG),
    ("$max([emptyf.v])", ARRAY_ARG),
    ("$min([n.v])", ARRAY_ARG),
    ("$min([arr.v])", ARRAY_ARG),
    ("$min([seq.v])", ARRAY_ARG),
    ("$min([miss.v])", ARRAY_ARG),
    ("$min([emptyf.v])", ARRAY_ARG),
    ("$average([n.v])", ARRAY_ARG),
    ("$average([arr.v])", ARRAY_ARG),
    ("$average([seq.v])", ARRAY_ARG),
    ("$average([miss.v])", ARRAY_ARG),
    ("$average([emptyf.v])", ARRAY_ARG),
    // The un-postfixed control for the `$func(path)[]` block below.
    ("$sum(prices)", POSTFIX),
    // ── Postfixes that do NOT sit on the lifted node: the `[]`/`{…}` binds
    //    to the whole path or to the HOF call, so the inner lift is still
    //    legal and must still fire (jsntrs-6wr.2, jsntrs-e8l) ──
    ("items.$string(x){'k': $}", CLEAN),
    ("items.($string(x)){'k': $}", CLEAN),
    ("items.($string(x))[]", CLEAN),
    ("items^(x)[]", CLEAN),
    ("items^(x){'k': $}", CLEAN),
    ("$sort(items, function($a,$b){$a.x > $b.x})[]", CLEAN),
    ("$sort(items, function($a,$b){$a.x > $b.x}){'k': $}", CLEAN),
    ("$map(items, function($v){$v.x}){'k': $}", CLEAN),
    // ── Keep-array `[]` inside lifted lambda bodies (jsntrs-6wr.3): the
    //    shapes WITHOUT `[]` must still lift, or the guard over-bailed and
    //    quietly disabled the fast paths it was meant to narrow. ──
    ("$map(items, function($v){$v.x})", KEEP_ARRAY),
    ("$filter(items, function($v){$v.x > 2})", CLEAN),
    ("$sort(items, function($a,$b){$a.x > $b.x})", CLEAN),
    ("$reduce(items, function($p,$c){$p + $c.x}, 0)", CLEAN),
    ("$map(items, function($v){\"id-\" & $v.name})", CLEAN),
    ("$map(items, function($v){$string($v.x)})", KEEP_ARRAY),
    ("items.$string(x)", KEEP_ARRAY),
    // ── Under-supplied lambda callees in lifted calls (jsntrs-6wr.5): the
    //    guard must not over-bail — fully supplied calls still lift. ──
    (
        r#"( $f := function($a){ $a & "!" }; items.$f(x) )"#,
        LAMBDA_ARITY,
    ),
    (
        "( $f := function($a, $b){ $b }; items.$f(x, y) )",
        LAMBDA_ARITY,
    ),
    (
        "( $f := function($a){ $a }; $map(items, function($v){ $f($v.x) }) )",
        LAMBDA_ARITY,
    ),
    // ── Reversed literal operands (jsntrs-6wr.6). Equality is symmetric and
    //    numbers/strings pass `Value::compare`'s left-operand type check, so
    //    these reversed shapes keep lifting (the orderings led by `null`/a
    //    boolean must not — see GENERAL_ONLY_CASES). ──
    ("$sort(items, function($a,$b){$a.x > $b.x})", COMPARE_LITS),
    ("$filter(miss, function($v){null = $v.x})", COMPARE_LITS),
    ("$filter(miss, function($v){null != $v.x})", COMPARE_LITS),
    ("$filter(miss, function($v){false = $v.x})", COMPARE_LITS),
    ("$filter(miss, function($v){true != $v.x})", COMPARE_LITS),
    ("$filter(items, function($v){null = $v.x})", COMPARE_LITS),
    ("$filter(items, function($v){true = $v.x})", COMPARE_LITS),
    ("$filter(items, function($v){false != $v.x})", COMPARE_LITS),
    ("$filter(miss, function($v){2 > $v.x})", COMPARE_LITS),
    ("$filter(items, function($v){2 > $v.x})", COMPARE_LITS),
    ("$filter(items, function($v){\"s\" <= $v.x})", COMPARE_LITS),
    (
        "$filter(items, function($v){2 > $v.x or $v.y = 1})",
        COMPARE_LITS,
    ),
    // ── Signed builtins reached through a lifted call (jsntrs-6wr.7) ──
    // A block-wrapped step and a lambda body both evaluate through
    // eval_function on the general path, which validates and coerces
    // SignedBuiltin arguments; the lift dispatched to the raw fn and
    // silently accepted arity/type errors.
    ("items.($uppercase(name, 1))", SIGNED),
    ("items.($lowercase(name, 1))", SIGNED),
    ("items.($sum(x, 1))", SIGNED),
    ("items.($max(x, 2))", SIGNED),
    ("items.($string(x, 1))", SIGNED),
    ("items.($boolean(x, 1))", SIGNED),
    ("items.($uppercase(x))", SIGNED),
    ("items.($sum(name))", SIGNED),
    ("items.($sum(x))", SIGNED),
    ("items.($string(x))", SIGNED),
    ("items.($average(x))", SIGNED),
    ("items.($min(x))", SIGNED),
    ("items.($boolean(x))", SIGNED),
    ("items.($uppercase(name))", SIGNED),
    ("$map(items, function($v){$uppercase($v.name, 1)})", SIGNED),
    ("$map(items, function($v){$sum($v.x, 1)})", SIGNED),
    ("$map(items, function($v){$uppercase($v.x)})", SIGNED),
    // A bare function path step is the other route: its general path is
    // eval_path_function_step, which now runs the same validation
    // (jsntrs-p0v.7). Before that fix both sides agreed on the *unvalidated*
    // answer, so these pairs passed while pinning the bug.
    ("items.$uppercase(name, 1)", SIGNED),
    ("items.$sum(x, 1)", SIGNED),
    ("items.$string(x, 1)", SIGNED),
    ("items.$boolean(x, 1)", SIGNED),
    ("items.$sum(x)", SIGNED),
    ("items.$uppercase(name)", SIGNED),
    ("items.$string(x)", SIGNED),
    // ── Bare-step signature validation (jsntrs-p0v.7) — track:
    //    step-validate-group-lift ──
    ("items.$lowercase(name, 1)", SIGNED),
    ("items.$max(x, 2)", SIGNED),
    ("items.$uppercase(x)", SIGNED),
    ("items.$sum(name)", SIGNED),
    ("items.$average(x)", SIGNED),
    ("items.$min(x)", SIGNED),
    ("items.$boolean(x)", SIGNED),
    ("items.$uppercase(missing)", SIGNED),
    ("items.$sum(missing)", SIGNED),
    ("items.$string(missing)", SIGNED),
    // ── `[]` on a call, where the lift is still legal (jsntrs-e8l) ──
    ("items.$string(x)[]", KEEP_ARRAY),
    ("items.$count(x)[]", KEEP_ARRAY),
    ("items.($string(x)[])", KEEP_ARRAY),
    ("$map(items, function($v){$v.x})[]", KEEP_ARRAY),
    ("$filter(items, function($v){$v.x > 2})[]", CLEAN),
    ("$map(items, function($v){$string($v.x)[]})", KEEP_ARRAY),
    ("$map(items, function($v){$round($v.x)[]})", CLEAN),
    ("items ~> $filter(function($v){$v.x > 2})[]", CLEAN),
    (
        "($f := function($v){$v}; $map(items, function($v){$f($v.x)[]}))",
        KEEP_ARRAY,
    ),
    // ── sequence-leak track (jsntrs-p0v.6): a lifted HOF still returns an
    //    uncollapsed sequence, and every consumer position around it now
    //    collapses that sequence. Fast and general must agree on both. ──
    ("$count($map(items, function($v){$v.x}))", CLEAN),
    ("$count($map(items, function($v){$v.x}))", HOSTILE),
    ("$reverse($map(items, function($v){$v.x}))", CLEAN),
    ("$distinct($map(items, function($v){$v.x}))", CLEAN),
    ("$type($map(items, function($v){$v.x}))", CLEAN),
    ("$type($map(items, function($v){$v.x}))", KEEP_ARRAY),
    ("$string($map(items, function($v){$v.x}))", CLEAN),
    ("$sum($map(items, function($v){$v.y}))", CLEAN),
    ("$map(items, function($v){$v.x}) = 3", CLEAN),
    ("$map(items, function($v){$v.x}) ~> $count()", CLEAN),
    ("$map(items, function($v){$v.x})[0]", CLEAN),
    ("$map(items, function($v){$v.x})[$ > 1]", CLEAN),
    ("[$map(items, function($v){$v.x})]", CLEAN),
    ("{'k': $map(items, function($v){$v.x})}", CLEAN),
    ("$count($filter(items, function($v){$v.x > 2}))", CLEAN),
    ("$type($filter(items, function($v){$v.x > 99}))", CLEAN),
    ("$map(items, function($v){$v.x})^($)", CLEAN),
    // ── Track lone-name-path ──────────────────────────────────────────
    // `[]` on a parenthesised path step is now hoisted to the path
    // (jsntrs-ews). The flag lives on the enclosing path, so the mapped
    // call inside the block still lifts and both routes must agree on the
    // singleton wrap.
    ("items.($string(x))[]", KEEP_ARRAY),
    // ── json-number-limits track (jsntrs-ztg): documents carrying number
    //    literals simd-json rejects. The tape lift hands the document back
    //    rather than failing the evaluation; the Value lanes still lift, so
    //    all three must land on the same Infinity / nearest-f64 answer. ──
    ("big", NUMBER_LIMITS),
    ("inf", NUMBER_LIMITS),
    ("items.x", NUMBER_LIMITS),
    ("big = 123456789012345680000", NUMBER_LIMITS),
    ("$count(items.x)", NUMBER_LIMITS),
    // ── Track toplevel-decorations (jsntrs-p0v.20) ────────────────────
    // `a^(b)[]` is a one-step path over the sort, and `eval_sort` still
    // takes the simple-field comparison lift for name-only terms, so these
    // do exercise two implementations of the comparison.
    ("items^(x)[]", CLEAN),
    ("items^(name)[]", KEEP_ARRAY),
    ("items^(x)[].name", CLEAN),
    // ── Track numeric-datetime-edges (jsntrs-p0v.5): the prepared
    //    $round precision and $formatBase radix are narrowed at analysis
    //    time, so the narrowing has to match the builtin's. The precision
    //    used to go f64 → i64 → i32 (the second step wraps: 1e300 became
    //    -1 and rounded to tens) and the radix used to truncate (15.5 was
    //    base 15, not 16). ──
    //    (An absurd *negative* precision underflows to NaN on both routes,
    //    which this harness's deep-equal cannot compare; that pair lives in
    //    `hof_fast::tests::round_fast_path_saturates_extreme_precision_like_the_builtin`.)
    ("items.$round(x, 1e300)", NUMS),
    ("items.$round(x, 1e10)", NUMS),
    ("items.$formatBase(x, 15.5)", CLEAN),
    ("items.$formatBase(x, 2.5)", NUMS),
    // ── Track block-step-sequence (jsntrs-p0v.19) ─────────────────────
    // The path's keep-singleton is a lazy flag now, so the wrap happens
    // wherever the value is collapsed rather than inside the step. These
    // are the shapes where a lift still fires *under* that flag — the
    // simple-field sort and the mapped call in a block step — and both
    // routes have to land on the same singleton.
    ("a^(b)[]", EMPTY_FIELDS),
    ("half^(b)[]", EMPTY_FIELDS),
    ("items.$string(x)[]", KEEP_ARRAY),
];

/// (expression, data) pairs where **no** lift may fire.
///
/// Two reasons land a pair here, and the comments say which:
///
/// - *Declined*: analysis refuses the shape (`$sum(prices)[]`, `$v.x[]` in a
///   lambda body, a typed lambda, an under-supplied lambda callee). These
///   are the regression pins for the epic's fixes — the general result is
///   the answer, and a future lift has to reproduce it.
/// - *Deferred*: the shape classifies, then the lifted evaluator looks at
///   the data and hands the whole expression back (`$sum` over nested
///   arrays, `$trim` on a number, `$number` on `"Infinity"`). The bail
///   decision is the thing under test.
///
/// Neither kind runs two implementations, so they are kept out of `CASES`
/// rather than passing there vacuously.
const GENERAL_ONLY_CASES: &[(&str, &str)] = &[
    // ── Deferred: aggregates bail on nested arrays, whose per-level
    //    collapse a fold cannot reproduce (gnata-dx5.4) ──
    ("$sum(m.v)", NESTED_ARRAYS),
    ("$max(f.g.h)", NESTED_ARRAYS),
    ("$min(m.v)", NESTED_ARRAYS),
    ("$average(m.v)", NESTED_ARRAYS),
    // ── Deferred: a field found but empty resolves to `[]`, which is an
    //    array — every scalar-shaped kind and the comparison class bail
    //    (gnata-dx5.5) ──
    ("$sum(a.b)", EMPTY_FIELDS),
    ("$string(a.b)", EMPTY_FIELDS),
    ("$boolean(a.b)", EMPTY_FIELDS),
    ("a.b = 1", EMPTY_FIELDS),
    ("a.b != 1", EMPTY_FIELDS),
    // ── Declined: bodies with no SimpleLambda shape ──
    // A bare `$v` body is not a field access, and $each/$sift have no
    // lifted shape of their own.
    ("$each({\"a\": 1, \"b\": 2}, function($v){$v})", CLEAN),
    ("$sift(items[0], function($v){$v > 1})", CLEAN),
    ("$each(obj, function($v){$v.x})", KEEP_ARRAY),
    // ── Declined: ops eval_binary_simple does NOT implement
    //    (gnata-dx5.1 — they used to lift and evaluate to undefined) ──
    ("$filter(items, function($v){$v.x and $v.y})", CLEAN),
    ("$filter(items, function($v){$v.x and $v.y})", HOSTILE),
    ("$filter(items, function($v){$v.x or $v.y})", HOSTILE),
    ("$filter(items, function($v){$v.x in [1, 3]})", CLEAN),
    ("$map(items, function($v){$v.x ** 2})", NUMS),
    // ── Declined: reversed non-commutative arithmetic (gnata-dx5.2) ──
    ("$filter(items, function($v){10 - $v.x})", NUMS),
    ("$filter(items, function($v){10 % $v.x})", NUMS),
    ("$filter(items, function($v){1 < $v.x and 10 - $v.y})", NUMS),
    // ── Declined: ReduceCompoundAccum only recognises `$acc op $v.f op
    //    $v.g`; leading with the fields is a different tree. ──
    (
        "$reduce(items, function($acc, $v){$acc * $v.x + $v.y}, 1)",
        NUMS,
    ),
    // ── Declined: the block around a compound reduce's inner term is only
    //    pure punctuation when it holds exactly one binding-free expression
    //    and wears no postfix, so `unwrap_paren_block` keeps it here and
    //    the general evaluator answers — as it must if the guard is ever
    //    loosened (jsntrs-5sj). `[]` is the postfix `process_ast` hoists
    //    onto an enclosing path; `;` makes the block several expressions;
    //    `:=` is a binding the unwrap would leak into the lambda frame.
    //    `(( … )[])` is the row that separates a per-level guard from a
    //    top-level-only one: wrapping the `[]` block in a clean pair of
    //    parentheses must not make it liftable. The spellings that lift
    //    after the fix are in CASES, next to their bare twins. ──
    (
        "$reduce(items, function($acc, $v){$acc + ($v.x * $v.y)[]}, 0)",
        CLEAN,
    ),
    (
        "$reduce(items, function($acc, $v){$acc + ($v.x * $v.y)[]}, 0)",
        HOSTILE,
    ),
    (
        "$reduce(items, function($acc, $v){$acc + (($v.x * $v.y)[])}, 0)",
        CLEAN,
    ),
    (
        "$reduce(items, function($acc, $v){$acc + ($v.x; $v.x * $v.y)}, 0)",
        CLEAN,
    ),
    (
        "$reduce(items, function($acc, $v){$acc + ($z := $v.x * $v.y)}, 0)",
        CLEAN,
    ),
    // ── Deferred: comparison LHS auto-maps to an array ──
    ("arr.v = 2", NESTED),
    // ── Deferred: the argument's runtime type is not the one the lifted
    //    builtin implements, so apply_func hands back (T0410/D3030 and the
    //    coercions all belong to the general path) ──
    ("$sum(mixed.v)", NESTED),
    ("$sum(a.b.missing)", NESTED),
    ("$max(mixed.v)", NESTED),
    ("$min(mixed.v)", NESTED),
    ("$average(mixed.v)", NESTED),
    ("$length(arr)", NESTED),
    ("$length(arr.v)", NESTED),
    ("$floor(a.b.s)", NESTED),
    ("$number(a.b.s)", NESTED),
    ("$boolean(empty)", NESTED),
    ("$trim(a.b.c)", NESTED),
    ("$contains(a.b.c, \"4\")", NESTED),
    ("$uppercase(a.b.c)", NESTED),
    ("$keys(none)", POSTFIX),
    ("$values(none)", POSTFIX),
    ("$values(empty)", NESTED),
    ("$reverse(name)", POSTFIX),
    ("$flatten(name)", POSTFIX),
    ("$shuffle(name)", POSTFIX),
    // ── Deferred: $number on non-finite strings must raise D3030, not
    //    return Infinity (gnata-dx5.9) ──
    ("$number(inf)", NONFINITE_STRINGS),
    ("$number(neginf)", NONFINITE_STRINGS),
    ("$number(nan)", NONFINITE_STRINGS),
    // ── Declined: typed lambdas need the general call path for signature
    //    validation and coercion (gnata-dx5.6) ──
    ("$map(arr, function($v)<s>{$v.v})", NESTED),
    ("$map(arr, function($v)<o>{$v.v})", NESTED),
    ("$filter(arr, function($v)<n:b>{$v.v > 1})", NESTED),
    ("$filter(arr, function($v)<o:b>{$v.v > 1})", NESTED),
    // ── Deferred: $distinct dedupes with deep_equal, which a scalar key
    //    cannot reproduce for objects (gnata-dx5.10) ──
    ("$distinct(objs)", DISTINCT_SHAPES),
    ("$distinct(mixed)", DISTINCT_SHAPES),
    // ── Declined: `$func([path])` keeps the array constructor for every
    //    kind but the aggregates (jsntrs-6wr.1) ──
    ("$string([n.v])", ARRAY_ARG),
    ("$string([miss.v])", ARRAY_ARG),
    ("$string([s.v])", ARRAY_ARG),
    ("$type([n.v])", ARRAY_ARG),
    ("$type([miss.v])", ARRAY_ARG),
    ("$boolean([n.v])", ARRAY_ARG),
    ("$boolean([miss.v])", ARRAY_ARG),
    ("$not([n.v])", ARRAY_ARG),
    ("$exists([miss.v])", ARRAY_ARG),
    ("$exists([n.v])", ARRAY_ARG),
    ("$exists([emptyf.v])", ARRAY_ARG),
    ("$number([ns.v])", ARRAY_ARG),
    ("$sqrt([sq.v])", ARRAY_ARG),
    ("$abs([n.v])", ARRAY_ARG),
    ("$floor([n.v])", ARRAY_ARG),
    ("$ceil([n.v])", ARRAY_ARG),
    ("$lowercase([s.v])", ARRAY_ARG),
    ("$uppercase([s.v])", ARRAY_ARG),
    ("$trim([s.v])", ARRAY_ARG),
    ("$length([s.v])", ARRAY_ARG),
    ("$contains([s.v], \"ELL\")", ARRAY_ARG),
    ("$keys([obj.v])", ARRAY_ARG),
    ("$values([obj.v])", ARRAY_ARG),
    ("$reverse([arr.v])", ARRAY_ARG),
    ("$distinct([arr.v])", ARRAY_ARG),
    ("$flatten([arr.v])", ARRAY_ARG),
    ("$shuffle([n.v])", ARRAY_ARG),
    // Aggregates lift the constructor away, then defer on the data: an
    // empty leaf list is `[]` for `[path]` but undefined for `path`, and
    // only the general path can tell them apart.
    ("$sum([miss.v])", ARRAY_ARG),
    ("$sum([emptyf.v])", ARRAY_ARG),
    // ── Declined: `$func(path)[]` / `$func(path){…}` — the postfix binds to
    //    the call node and the lift has nowhere to put it (jsntrs-6wr.2) ──
    ("$sum(prices)[]", POSTFIX),
    ("$sum(prices){'k': $}", POSTFIX),
    ("$sum(single)[]", POSTFIX),
    ("$sum(none)[]", POSTFIX),
    ("$sum(none){'k': $}", POSTFIX),
    ("$count(prices)[]", POSTFIX),
    ("$count(prices){'n': $}", POSTFIX),
    ("$max(prices)[]", POSTFIX),
    ("$min(prices)[]", POSTFIX),
    ("$min(prices){'m': $}", POSTFIX),
    ("$average(prices)[]", POSTFIX),
    ("$average(prices){'a': $}", POSTFIX),
    ("$exists(prices)[]", POSTFIX),
    ("$exists(missing)[]", POSTFIX),
    ("$exists(prices){'e': $}", POSTFIX),
    ("$string(prices)[]", POSTFIX),
    ("$type(prices)[]", POSTFIX),
    ("$type(name){'t': $}", POSTFIX),
    ("$boolean(prices)[]", POSTFIX),
    ("$not(prices)[]", POSTFIX),
    ("$number(name)[]", POSTFIX),
    ("$sqrt(single)[]", POSTFIX),
    ("$abs(single)[]", POSTFIX),
    ("$floor(single)[]", POSTFIX),
    ("$ceil(single)[]", POSTFIX),
    ("$keys(obj)[]", POSTFIX),
    ("$keys(obj){'k': $}", POSTFIX),
    ("$keys(obj)[0]", POSTFIX),
    ("$values(obj)[]", POSTFIX),
    ("$values(obj){'v': $}", POSTFIX),
    ("$distinct(prices)[]", POSTFIX),
    ("$reverse(prices)[]", POSTFIX),
    ("$reverse(prices){'r': $}", POSTFIX),
    ("$flatten(prices)[]", POSTFIX),
    ("$flatten(prices){'f': $}", POSTFIX),
    ("$shuffle(single)[]", POSTFIX),
    ("$shuffle(single){'s': $}", POSTFIX),
    ("$length(name)[]", POSTFIX),
    ("$trim(name)[]", POSTFIX),
    ("$uppercase(name)[]", POSTFIX),
    ("$lowercase(name){'l': $}", POSTFIX),
    ("$contains(name, \"lic\")[]", POSTFIX),
    ("$contains(name, \"lic\"){'c': $}", POSTFIX),
    // ── Declined: keep-array `[]` inside lambda bodies (jsntrs-6wr.3).
    //    `[]` on any step makes the whole path keep singletons as arrays; no
    //    lifted shape can express that, so analysis must decline. ──
    // FieldAccess, on array-valued and scalar fields alike.
    ("$map(items, function($v){$v.x[]})", KEEP_ARRAY),
    ("$map(items, function($v){$v.x[]})", CLEAN),
    // `[]` on the parameter step propagates to the enclosing path too.
    ("$map(items, function($v){$v[].x})", KEEP_ARRAY),
    ("$map(items, function($v){$v[].name})", CLEAN),
    // FieldPredicate, both operand orders.
    ("$filter(items, function($v){$v.x[] > 2})", CLEAN),
    ("$filter(items, function($v){2 < $v.x[]})", CLEAN),
    // TwoFieldPredicate: `[]` on one side only still changes the compare.
    ("$filter(items, function($v){$v.x[] = $v.y})", NUMS),
    // CompoundPredicate clauses.
    (
        "$filter(items, function($v){$v.x[] > 2 and $v.y < 2})",
        CLEAN,
    ),
    (
        "$filter(items, function($v){$v.x > 2 or $v.y[] < 2})",
        CLEAN,
    ),
    // SortComparator / ReduceAccum / ReduceCompoundAccum.
    ("$sort(items, function($a,$b){$a.x[] > $b.x[]})", CLEAN),
    ("$reduce(items, function($p,$c){$p + $c.x[]}, 0)", CLEAN),
    (
        "$reduce(items, function($p,$c){$p + $c.x[] * $c.y}, 0)",
        CLEAN,
    ),
    // ConcatTemplate pieces: bare field and stringifying wrappers.
    ("$map(items, function($v){\"id-\" & $v.name[]})", CLEAN),
    ("$map(items, function($v){$v.name[] & \"!\"})", CLEAN),
    (
        "$map(items, function($v){\"n:\" & $uppercase($v.name[])})",
        CLEAN,
    ),
    (
        "$map(items, function($v){\"x:\" & $string($v.x[])})",
        KEEP_ARRAY,
    ),
    // Mapped-call lift, as a lambda arg and as a `.()` path step.
    ("$map(items, function($v){$string($v.x[])})", KEEP_ARRAY),
    ("$map(items, function($v){$round($v.x[])})", CLEAN),
    ("items.$string(x[])", KEEP_ARRAY),
    ("items.$count(x[])", KEEP_ARRAY),
    ("items.($string(x[]))", KEEP_ARRAY),
    // $each has no lifted shape; pin it so a future lift keeps the wrap.
    ("$each(obj, function($v){$v.x[]})", KEEP_ARRAY),
    // ── Declined: under-supplied lambda callees (jsntrs-6wr.5). As a path
    //    step the general path prepends the context item; inside a lambda
    //    body it pads with undefined. One lifted arg template cannot be
    //    both, so the lift declines on either route. ──
    (
        r#"( $f := function($a, $b){ $a.x & "/" & $b }; items.$f(y) )"#,
        LAMBDA_ARITY,
    ),
    (
        r#"( $f := function($a, $b){ $a.x & "/" & $b }; one.$f(y) )"#,
        LAMBDA_ARITY,
    ),
    (
        r#"( $f := function($a, $b, $c){ $a.x & $b & $c }; items.$f(y) )"#,
        LAMBDA_ARITY,
    ),
    (
        "( $f := function($a, $b){ $b }; items.$f(x) )",
        LAMBDA_ARITY,
    ),
    (
        "( $f := function($a, $b){ $a.x = $b }; items.$f(x) )",
        LAMBDA_ARITY,
    ),
    (
        "( $f := function($a, $b){ $b }; $map(items, function($v){ $f($v.x) }) )",
        LAMBDA_ARITY,
    ),
    (
        "( $f := function($a, $b){ $a }; $map(items, function($v){ $f($v.x) }) )",
        LAMBDA_ARITY,
    ),
    // ── Declined: reversed literal operands `Value::compare` cannot mirror
    //    (jsntrs-6wr.6). It checks the LEFT operand's type before undefined
    //    propagation, so `null > $v.x` raises T2010 on a missing field while
    //    the swapped `$v.x < null` propagates undefined. Every ordering
    //    operator led by `null`/`true`/`false`, in both filter clauses. ──
    ("$filter(miss, function($v){null > $v.x})", COMPARE_LITS),
    ("$filter(miss, function($v){null < $v.x})", COMPARE_LITS),
    ("$filter(miss, function($v){null >= $v.x})", COMPARE_LITS),
    ("$filter(miss, function($v){null <= $v.x})", COMPARE_LITS),
    ("$filter(miss, function($v){false > $v.x})", COMPARE_LITS),
    ("$filter(miss, function($v){false < $v.x})", COMPARE_LITS),
    ("$filter(miss, function($v){true <= $v.x})", COMPARE_LITS),
    ("$filter(miss, function($v){true >= $v.x})", COMPARE_LITS),
    ("$filter(items, function($v){null > $v.x})", COMPARE_LITS),
    ("$filter(items, function($v){null >= $v.x})", COMPARE_LITS),
    ("$filter(items, function($v){true < $v.x})", COMPARE_LITS),
    ("$map(items, function($v){null > $v.x})", COMPARE_LITS),
    ("$sort(items, function($a,$b){null > $a.x})", COMPARE_LITS),
    (
        "$reduce(miss, function($a,$v){null > $v.x}, 0)",
        COMPARE_LITS,
    ),
    // Compound predicates reverse literals the same way, in either clause.
    (
        "$filter(miss, function($v){null > $v.x and $v.y = 1})",
        COMPARE_LITS,
    ),
    (
        "$filter(miss, function($v){$v.y = 1 and false >= $v.x})",
        COMPARE_LITS,
    ),
    (
        "$filter(miss, function($v){$v.y = 1 or true < $v.x})",
        COMPARE_LITS,
    ),
    // ── Declined: `[]` on a bare call (jsntrs-e8l). The postfix is honoured
    //    only when the result stands in for a sequence, so a call carrying
    //    it must not be lifted. ──
    ("$count(items)[]", KEEP_ARRAY),
    ("$string(items[0].x)[]", KEEP_ARRAY),
    ("$sum(items[0].x)[]", KEEP_ARRAY),
    ("$each(obj, function($v){$string($v.x)[]})", KEEP_ARRAY),
    ("items ~> $count()[]", KEEP_ARRAY),
    ("($f := function($v){$v}; $f(items)[])", KEEP_ARRAY),
    // ── Declined: `{…}` on a block or on a predicate body — the neighbours
    //    of the former jsntrs-6wr.9 hole. These always agreed. ──
    ("$map(items, function($v){($string($v.x)){'k': $}})", CLEAN),
    ("$filter(items, function($v){$string($v.x){'k': $}})", CLEAN),
    (
        "$each(obj, function($v){$string($v.x){'k': $}})",
        KEEP_ARRAY,
    ),
    // ── sequence-leak track (jsntrs-p0v.6): shapes the analyzers decline,
    //    pinned because they are exactly where a builtin callback's
    //    uncollapsed sequence is embedded, or where a lambda body hands one
    //    back through the tail position. ──
    ("$map(items, $keys)", CLEAN),
    ("$each(obj, $keys)", KEEP_ARRAY),
    ("$map(items, function($v){$keys($v)})", CLEAN),
    ("$each(obj, function($v){$keys($v)})", KEEP_ARRAY),
    ("$count($each(obj, function($v){$keys($v)}))", KEEP_ARRAY),
    ("$keys(obj)[]", POSTFIX),
    ("$count($keys(obj))", POSTFIX),
    ("($f := function(){$keys(obj)}; $f()[])", POSTFIX),
    ("($f := function(){$keys(obj)}; $count($f()))", POSTFIX),
    (
        "$map(prices, function($v){$map([$v], function($w){$w})})",
        POSTFIX,
    ),
    (
        "$map(single, function($v){$map([$v], function($w){$w})})",
        POSTFIX,
    ),
    (
        "$reduce(prices, function($a,$b){$map([$a+$b], function($w){$w})})",
        POSTFIX,
    ),
    (
        "$each(obj, function($v,$k){$each({'z': $v}, function($w,$j){$k & $j})})",
        POSTFIX,
    ),
    // ── Declined: `{…}` on a lifted mapped call or a lifted field access
    //    (jsntrs-6wr.9) — track: step-validate-group-lift. These are the
    //    seven shapes the module header used to exclude from both lists:
    //    the lift dropped the postfix and answered the ungrouped value.
    //    `analyze_mapped_call` and `extract_param_field` now decline, so
    //    each one runs the general evaluator on both sides. The SIGNED
    //    fixture's third item has no `x`, which is what makes the general
    //    path emit the empty object. ──
    ("$map(items, function($v){$string($v.x){'k': $}})", SIGNED),
    ("$map(items, function($v){$round($v.x){'k': $}})", CLEAN),
    ("$map(items, function($v){$v.x{'k': $}})", SIGNED),
    (
        "$map(items, function($v){$string($v.x){'k': $v.name}})",
        SIGNED,
    ),
    (
        "($f := function($v){$v}; $map(items, function($v){$f($v.x){'k': $}}))",
        SIGNED,
    ),
    ("items.($string(x){'k': $})", SIGNED),
    ("items.($round(x){'k': $})", CLEAN),
    // The same postfix reached through the other lifted shapes: a predicate
    // operand, a homogeneous fixture with no missing field, and a `.()`
    // field step.
    ("$filter(items, function($v){$v.x{'k': $} = 3})", SIGNED),
    ("$map(items, function($v){$v.x{'k': $}})", CLEAN),
    ("items.(x{'k': $})", SIGNED),
    // ── Track lone-name-path ──────────────────────────────────────────
    // Declined: a decorated lone `Name` is now a single-step path whose
    // `keep_singleton_array` is set (jsntrs-ews), and `[]` on a
    // parenthesised path step hoists the same flag. `collect_pure_path`
    // refuses a path carrying it, so none of these lift — the general
    // result is the answer a future lift would have to reproduce.
    ("items[0].x[]", KEEP_ARRAY),
    ("obj.a.x[]", KEEP_ARRAY),
    ("$string(items[2].x[])", KEEP_ARRAY),
    ("$count(items[3].x[])", KEEP_ARRAY),
    ("items.(x[])", KEEP_ARRAY),
    ("items.(name)[]", KEEP_ARRAY),
    ("($f := function(){ name[] }; items.$f())", KEEP_ARRAY),
    ("($sum(prices))[]", POSTFIX),
    ("obj.($sum(x))[]", POSTFIX),
    // Declined: a lone `@$v`/`#$i` name is a one-step tuple path
    // (jsntrs-p0v.8). `collect_pure_path` refuses a step carrying a focus
    // or index binding, so the tuple stream is always the general path's.
    ("items@$v{$v.name: $v.x}", KEEP_ARRAY),
    ("items#$i{$string($i): name}", KEEP_ARRAY),
    ("items@$v", KEEP_ARRAY),
    ("items#$i", KEEP_ARRAY),
    ("items@$v.$v.name", KEEP_ARRAY),
    ("items#$i.($i & name)", KEEP_ARRAY),
    ("items@$v[]", KEEP_ARRAY),
    // ── [group-key-parent] Declined: a `{…}` group-by postfix keeps the whole
    //    expression off every lift, so the `%`-in-a-pair rule (undefined, not
    //    S0217 — jsntrs-p0v.9) is general-evaluator-only. ──
    ("items{%.name: $string(x)}", CLEAN),
    ("items{$string(x): %.name}", CLEAN),
    ("items.name{%: $}", CLEAN),
    // ── Track toplevel-decorations ────────────────────────────────────
    // Declined: `*[]` is now a single-step path carrying
    // `keep_singleton_array` and `**[]` keeps its raw `Descendant`
    // (jsntrs-p0v.20); `collect_pure_path` takes neither a decorated path
    // nor a wildcard/descendant step, so the general result is the answer.
    ("*[]", POSTFIX),
    ("*[]", KEEP_ARRAY),
    ("obj.*[]", POSTFIX),
    ("(*[])", POSTFIX),
    ("$count(*[])", POSTFIX),
    ("*[].x", KEEP_ARRAY),
    ("**[]", POSTFIX),
    ("obj.**[]", POSTFIX),
    ("$count(**[])", POSTFIX),
    // Declined: a bare `**` now runs the same root-inclusive descendant
    // walk as a `**` path step (jsntrs-p0v.22); no lift covers either.
    ("**", POSTFIX),
    ("**", KEEP_ARRAY),
    ("**.x", KEEP_ARRAY),
    ("$count(**)", POSTFIX),
    ("[**]", POSTFIX),
    ("** ~> $count", POSTFIX),
    ("obj.**", POSTFIX),
    // ── Track block-step-sequence (jsntrs-p0v.19) ─────────────────────
    // Declined: a `[]` anywhere in the shape sets `keep_singleton_array`
    // on the path (or reaches the value through a block step that carries
    // it), and `collect_pure_path` refuses such a path. The flag is now a
    // lazy `Sequence` marker the enclosing path drops while re-sequencing,
    // so these pin the general answer a future lift would have to
    // reproduce — including the `[]` that survives (`obj.a.(x[])[]`) and
    // the one that does not (`obj.a.(x[])`).
    ("obj.a.(x[])", KEEP_ARRAY),
    ("obj.a.(x[])[]", KEEP_ARRAY),
    ("obj.(a.x[])", KEEP_ARRAY),
    ("obj.(a.x[][0])", KEEP_ARRAY),
    ("obj.($keys(a)[])", KEEP_ARRAY),
    ("obj.a.(x[]^($))", KEEP_ARRAY),
    ("$string(obj.b.(x[]))", KEEP_ARRAY),
    ("items[2].(x[])", KEEP_ARRAY),
    ("items[2].($string(x[]))", KEEP_ARRAY),
    ("obj.(x[])", POSTFIX),
    ("obj.($keys($)[])", POSTFIX),
    ("obj.($sum(x)[])", POSTFIX),
    ("items[0].(x[])", SIGNED),
];

type EvalResult = Result<Value, JsonataError>;

fn describe(r: &EvalResult) -> String {
    match r {
        Ok(v) => format!("Ok({v:?})"),
        Err(e) => format!("Err({})", e.code),
    }
}

/// Compare fast vs general results: equal values or equal error codes.
fn diverged(fast: &EvalResult, general: &EvalResult) -> bool {
    match (fast, general) {
        (Ok(a), Ok(b)) => !(a.is_undefined() && b.is_undefined()) && !jsntrs::deep_equal(a, b),
        (Err(a), Err(b)) => a.code != b.code,
        _ => true,
    }
}

/// One line naming the fixture, for the tier-membership reports.
fn fixture_tag(data: &str) -> String {
    let flat = data.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(56).collect()
}

/// Run one pair through every lane, fast paths on and then off, appending
/// any divergence to `mismatches`.
///
/// Returns the lifts the binding-free lanes took. The env lane runs after
/// the tally is read: it drops the function class by design, so counting it
/// would blur what the tiers mean.
fn run_case(
    expr: &str,
    data: &str,
    env: &Rc<Environment>,
    mismatches: &mut Vec<String>,
) -> Option<u64> {
    let compiled = match Expression::compile(expr) {
        Ok(c) => c,
        Err(e) => {
            mismatches.push(format!("COMPILE FAIL {expr}: {e}"));
            return None;
        }
    };
    // A fixture that does not parse used to sink to Undefined and compare
    // two empty runs; say so instead.
    let input = Value::from_json_str(data)
        .unwrap_or_else(|e| panic!("fixture for {expr:?} is not valid JSON: {e}"));

    jsntrs::fast_path_testing::set_fast_paths_disabled(false);
    jsntrs::fast_path_testing::reset_hits();
    let fast_str = compiled.evaluate(data);
    let fast_val = compiled.evaluate_value(&input);
    let hits = jsntrs::fast_path_testing::hits();
    let with_env = compiled.evaluate_with_env(&input, env);

    jsntrs::fast_path_testing::set_fast_paths_disabled(true);
    let general = compiled.evaluate_value(&input);
    jsntrs::fast_path_testing::set_fast_paths_disabled(false);

    for (lane, result) in [
        ("evaluate", &fast_str),
        ("evaluate_value", &fast_val),
        ("evaluate_with_env", &with_env),
    ] {
        if diverged(result, &general) {
            mismatches.push(format!(
                "{expr}\n  data:    {data}\n  fast:    {} ({lane})\n  general: {}",
                describe(result),
                describe(&general)
            ));
        }
    }

    Some(hits)
}

fn assert_no_mismatches(mismatches: &[String]) {
    assert!(
        mismatches.is_empty(),
        "{} fast-path divergence(s):\n\n{}",
        mismatches.len(),
        mismatches.join("\n\n")
    );
}

#[test]
fn fast_paths_match_general_evaluator() {
    let env = new_custom_env(&[]);
    let mut mismatches: Vec<String> = Vec::new();
    let mut vacuous: Vec<String> = Vec::new();

    for &(expr, data) in CASES {
        if run_case(expr, data, &env, &mut mismatches) == Some(0) {
            vacuous.push(format!("{expr}\n    fixture: {}", fixture_tag(data)));
        }
    }

    assert_no_mismatches(&mismatches);
    assert!(
        vacuous.is_empty(),
        "{} CASES entr{} took no fast path — both runs were the general \
         evaluator, so the comparison proved nothing. Either fix the fixture \
         so the lift fires again, or move the entry to GENERAL_ONLY_CASES \
         with a note saying which lift now declines it:\n\n{}",
        vacuous.len(),
        if vacuous.len() == 1 { "y" } else { "ies" },
        vacuous.join("\n\n")
    );
}

#[test]
fn general_only_cases_match_and_take_no_lift() {
    let env = new_custom_env(&[]);
    let mut mismatches: Vec<String> = Vec::new();
    let mut lifted: Vec<String> = Vec::new();

    for &(expr, data) in GENERAL_ONLY_CASES {
        match run_case(expr, data, &env, &mut mismatches) {
            Some(0) | None => {}
            Some(hits) => lifted.push(format!(
                "{expr}  ({hits} lift(s))\n    fixture: {}",
                fixture_tag(data)
            )),
        }
    }

    assert_no_mismatches(&mismatches);
    assert!(
        lifted.is_empty(),
        "{} GENERAL_ONLY_CASES entr{} now take a fast path. That is not \
         necessarily a bug — this run also checked the results still agree — \
         but the entry belongs in CASES now, where the lift is required to \
         keep firing:\n\n{}",
        lifted.len(),
        if lifted.len() == 1 { "y" } else { "ies" },
        lifted.join("\n\n")
    );
}
