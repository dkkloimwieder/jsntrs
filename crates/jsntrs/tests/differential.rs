//! Differential fast-path tests.
//!
//! Every expression here is recognized by at least one fast path
//! (`hof_fast` lambda shapes, mapped-call lifts, the `^()` sort fast
//! path, or `fast_path` expression classification). Each case is
//! evaluated twice — fast paths enabled, then disabled via
//! `fast_path::testing` — and both results must be identical,
//! including error codes. This guards against the fast-path layer
//! diverging from the general evaluator (gnata-bec.5).

use jsntrs::Value;
use jsntrs::{Expression, JsonataError};

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

/// (expression, data) pairs. Every pair runs fast vs general.
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
    ("$sum(m.v)", NESTED_ARRAYS),
    ("$max(f.g.h)", NESTED_ARRAYS),
    ("$min(m.v)", NESTED_ARRAYS),
    ("$average(m.v)", NESTED_ARRAYS),
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
    ("$sum(a.b)", EMPTY_FIELDS),
    ("$max(a.b)", EMPTY_FIELDS),
    ("$string(a.b)", EMPTY_FIELDS),
    ("$boolean(a.b)", EMPTY_FIELDS),
    ("$type(a.b)", EMPTY_FIELDS),
    ("a.b = 1", EMPTY_FIELDS),
    ("a.b != 1", EMPTY_FIELDS),
    // ── SimpleLambda::FieldAccess ──
    ("$map(items, function($v){$v.x})", CLEAN),
    ("$map(items, function($v){$v.x})", HOSTILE),
    ("$each({\"a\": 1, \"b\": 2}, function($v){$v})", CLEAN),
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
    ("$sift(items[0], function($v){$v > 1})", CLEAN),
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
    // ── Ops eval_binary_simple does NOT implement: must not be lifted
    //    (gnata-dx5.1 — they used to lift and evaluate to undefined) ──
    ("$filter(items, function($v){$v.x and $v.y})", CLEAN),
    ("$filter(items, function($v){$v.x and $v.y})", HOSTILE),
    ("$filter(items, function($v){$v.x or $v.y})", HOSTILE),
    ("$filter(items, function($v){$v.x in [1, 3]})", CLEAN),
    ("$filter(items, function($v){$v.name & \"!\"})", CLEAN),
    ("$map(items, function($v){$v.x ** 2})", NUMS),
    // ── Reversed literal-op-field shapes (gnata-dx5.2 — non-commutative
    //    arithmetic used to lift with swapped operands) ──
    ("$filter(items, function($v){2 > $v.x})", CLEAN),
    ("$filter(items, function($v){2 > $v.x})", HOSTILE),
    ("$filter(items, function($v){3 = $v.x})", HOSTILE),
    ("$filter(items, function($v){10 - $v.x})", NUMS),
    ("$filter(items, function($v){10 % $v.x})", NUMS),
    ("$filter(items, function($v){1 < $v.x and 10 - $v.y})", NUMS),
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
    ("arr.v = 2", NESTED),
    // ── Expression-level FastPath::Function ──
    ("$sum(arr.v)", NESTED),
    ("$sum(mixed.v)", NESTED),
    ("$count(arr)", NESTED),
    ("$count(empty)", NESTED),
    ("$exists(a.b.c)", NESTED),
    ("$exists(a.b.missing)", NESTED),
    ("$distinct(arr.v)", NESTED),
    ("$keys(a.b)", NESTED),
    ("$sqrt(a.b.c)", NESTED),
    ("$string(a.b.c)", NESTED),
    ("$sum(a.b.missing)", NESTED),
    ("$max(mixed.v)", NESTED),
    ("$min(mixed.v)", NESTED),
    ("$max(arr.v)", NESTED),
    ("$min(a.b.missing)", NESTED),
    ("$average(arr.v)", NESTED),
    ("$average(mixed.v)", NESTED),
    ("$length(a.b.s)", NESTED),
    ("$length(arr)", NESTED),
    ("$length(arr.v)", NESTED),
    ("$abs(a.b.c)", NESTED),
    ("$floor(a.b.s)", NESTED),
    ("$ceil(a.b.c)", NESTED),
    ("$number(a.b.s)", NESTED),
    ("$number(a.b.c)", NESTED),
    ("$boolean(a.b.missing)", NESTED),
    ("$boolean(empty)", NESTED),
    ("$not(a.b.missing)", NESTED),
    ("$trim(a.b.s)", NESTED),
    ("$trim(a.b.c)", NESTED),
    ("$contains(a.b.s, \"ell\")", NESTED),
    ("$contains(a.b.c, \"4\")", NESTED),
    ("$lowercase(a.b.s)", NESTED),
    ("$uppercase(a.b.c)", NESTED),
    ("$exists(mixed.v)", NESTED),
    ("$count(mixed.v)", NESTED),
    ("$count(a.b.missing)", NESTED),
    // ── $number on non-finite strings: D3030, not Infinity (gnata-dx5.9) ──
    ("$number(inf)", NONFINITE_STRINGS),
    ("$number(neginf)", NONFINITE_STRINGS),
    ("$number(nan)", NONFINITE_STRINGS),
    ("$number(num)", NONFINITE_STRINGS),
    // ── Typed lambdas must defer to the general call path (gnata-dx5.6) ──
    ("$map(arr, function($v)<s>{$v.v})", NESTED),
    ("$map(arr, function($v)<o>{$v.v})", NESTED),
    ("$filter(arr, function($v)<n:b>{$v.v > 1})", NESTED),
    ("$filter(arr, function($v)<o:b>{$v.v > 1})", NESTED),
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
    ("$distinct(objs)", DISTINCT_SHAPES),
    ("$distinct(zeros)", DISTINCT_SHAPES),
    ("$distinct(scalars)", DISTINCT_SHAPES),
    ("$distinct(mixed)", DISTINCT_SHAPES),
    // ── $func([path]): only the aggregates may drop the array
    //    constructor; every other kind must keep it (jsntrs-6wr.1) ──
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
    // Aggregates keep the lift — these must stay identical too.
    ("$count([n.v])", ARRAY_ARG),
    ("$count([miss.v])", ARRAY_ARG),
    ("$count([seq.v])", ARRAY_ARG),
    ("$count([emptyf.v])", ARRAY_ARG),
    ("$count([arr.v])", ARRAY_ARG),
    ("$sum([n.v])", ARRAY_ARG),
    ("$sum([arr.v])", ARRAY_ARG),
    ("$sum([seq.v])", ARRAY_ARG),
    ("$sum([miss.v])", ARRAY_ARG),
    ("$sum([emptyf.v])", ARRAY_ARG),
    ("$max([arr.v])", ARRAY_ARG),
    ("$max([miss.v])", ARRAY_ARG),
    ("$max([emptyf.v])", ARRAY_ARG),
    ("$min([arr.v])", ARRAY_ARG),
    ("$min([miss.v])", ARRAY_ARG),
    ("$average([seq.v])", ARRAY_ARG),
    ("$average([miss.v])", ARRAY_ARG),
    ("$average([emptyf.v])", ARRAY_ARG),
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

#[test]
fn fast_paths_match_general_evaluator() {
    let mut mismatches: Vec<String> = Vec::new();

    for &(expr, data) in CASES {
        let compiled = match Expression::compile(expr) {
            Ok(c) => c,
            Err(e) => {
                mismatches.push(format!("COMPILE FAIL {expr}: {e}"));
                continue;
            }
        };

        jsntrs::fast_path_testing::set_fast_paths_disabled(false);
        let fast_str = compiled.evaluate(data);
        let input = Value::from_json_str(data).expect("test data is valid JSON");
        let fast_val = compiled.evaluate_value(&input);

        jsntrs::fast_path_testing::set_fast_paths_disabled(true);
        let general = compiled.evaluate_value(&input);
        jsntrs::fast_path_testing::set_fast_paths_disabled(false);

        if diverged(&fast_str, &general) {
            mismatches.push(format!(
                "{expr}\n  data:    {data}\n  fast:    {} (evaluate)\n  general: {}",
                describe(&fast_str),
                describe(&general)
            ));
        }
        if diverged(&fast_val, &general) {
            mismatches.push(format!(
                "{expr}\n  data:    {data}\n  fast:    {} (evaluate_value)\n  general: {}",
                describe(&fast_val),
                describe(&general)
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "{} fast-path divergence(s):\n\n{}",
        mismatches.len(),
        mismatches.join("\n\n")
    );
}
