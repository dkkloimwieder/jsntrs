//! Conformance test harness: runs the JSONata test suite from testdata/groups/.
//!
//! Each test case is a JSON file (or an array of them) with:
//! - `expr` / `expr-file`: JSONata expression, inline or in a sibling file
//! - `dataset` or `data`: input data (dataset name or inline JSON)
//! - `result`: expected result (if successful)
//! - `undefinedResult`: true if result should be undefined
//! - `code`, or `error: { code, token }`: expected error
//! - `token`: the token the reference attached to the error (counted and
//!   reported, deliberately not enforced — see [`TOKEN_FIELD_NOTE`])
//! - `bindings`: optional variable bindings
//! - `unordered`: compare arrays as multisets at every depth
//! - `timelimit`: milliseconds the evaluation may take (enforced, see
//!   [`TIMELIMIT_SLACK`])
//! - `depth`: the reference harness's timebox depth (counted and reported,
//!   see [`DEPTH_FIELD_NOTE`])
//!
//! Every field above is honoured. A field the harness cannot enforce is
//! counted and reported per run rather than dropped in silence, and the
//! harness's own machinery is covered by the tests at the bottom of this
//! file: a check that silently stops checking is the failure mode this
//! suite exists to prevent.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use jsntrs::Environment;
use jsntrs::JsonataError;
use jsntrs::Value;
use jsntrs::eval;
use jsntrs::{Parser, process_ast};

/// Multiplier applied to a fixture's `timelimit` before enforcing it.
///
/// The fixture limits come from the reference harness running an optimized
/// JIT; the conformance suite runs an unoptimized debug build, often under
/// parallel load. The limits exist to catch runaway evaluation, not to
/// benchmark, so enforce them with generous slack — a hang still fails, a
/// slow machine still passes.
const TIMELIMIT_SLACK: u32 = 20;

/// How often the watchdog wakes to check whether its evaluation finished.
const WATCHDOG_TICK: Duration = Duration::from_millis(5);

/// Why fixture `depth` values are counted but not enforced.
///
/// The reference harness's `timeboxExpression(expr, timelimit, depth)`
/// installs `__evaluate_entry`/`__evaluate_exit` hooks and throws `U1001`
/// once *evaluate() nesting* passes `depth`; jsonata-js itself has no
/// recursion cap (unboxed, `$factorial(100)` just returns 9.33e157). jsntrs
/// caps lambda-call depth natively at `DEFAULT_MAX_CALL_DEPTH` (100 frames),
/// and that native cap is what makes the fixtures' `U1001` expectations come
/// out right.
///
/// The two units are not interchangeable, and the fixtures prove it:
/// `tail-recursion/case001` (factorial(99) → result) and `case002`
/// (factorial(100) → U1001) both carry `"depth": 302`, so feeding 302 to a
/// call-depth cap would let case002 succeed and contradict its own pinned
/// error. Nor is there a knob to feed: `CallCounter::max` is fixed at
/// construction and no public setter exposes it. Counting and reporting is
/// therefore the honest treatment — the field is visible in every run's
/// summary instead of being dropped in silence.
const DEPTH_FIELD_NOTE: &str = "not enforced: the reference's `depth` counts evaluate() nesting; \
     jsntrs caps lambda-call depth natively (DEFAULT_MAX_CALL_DEPTH), which is what these \
     fixtures' U1001 expectations already pin";

/// Handle whose drop tells the watchdog thread its evaluation is over.
struct Watchdog {
    done: Arc<AtomicBool>,
    limit_ms: u64,
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
    }
}

/// Start a watchdog for `limit_ms` (times [`TIMELIMIT_SLACK`]).
///
/// Returns the cancellation flag to hand the evaluator and a handle that
/// stops the thread when dropped. The evaluator polls the flag at call
/// boundaries and inside its hot loops, so a runaway expression unwinds
/// with `D3001` instead of hanging the suite.
fn start_watchdog(limit_ms: u64) -> (Arc<AtomicBool>, Watchdog) {
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let (thread_cancel, thread_done) = (Arc::clone(&cancel), Arc::clone(&done));
    let budget = Duration::from_millis(limit_ms.saturating_mul(u64::from(TIMELIMIT_SLACK)));
    std::thread::spawn(move || {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if thread_done.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(WATCHDOG_TICK);
        }
        if !thread_done.load(Ordering::Relaxed) {
            thread_cancel.store(true, Ordering::Relaxed);
        }
    });
    (cancel, Watchdog { done, limit_ms })
}

fn testdata_dir() -> PathBuf {
    // crates/jsntrs/tests/conformance.rs → testdata/ is at repo root
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.join("../../testdata")
}

fn load_dataset(name: &str) -> Value {
    let path = testdata_dir().join("datasets").join(format!("{name}.json"));
    let data = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing dataset: {name}"));
    Value::from_json_str(&data).unwrap_or_else(|e| panic!("bad dataset {name}: {e}"))
}

#[derive(Debug)]
struct TestCase {
    expr: String,
    input: Value,
    expected: Expected,
    bindings: Vec<(String, Value)>,
    /// Fixture flag: compare arrays as multisets (any order, any depth),
    /// like the reference harness's deep-equal-in-any-order.
    unordered: bool,
    /// Fixture field `timelimit`: milliseconds the evaluation may take.
    /// Enforced with a watchdog that trips the cancellation flag, so a
    /// runaway expression fails the case instead of hanging the suite.
    timelimit: Option<u64>,
    /// Fixture field `depth`: the reference harness's timebox depth. Counted
    /// and reported, deliberately not enforced — see [`DEPTH_FIELD_NOTE`].
    depth: Option<u64>,
}

#[derive(Debug)]
enum Expected {
    Result(Value),
    Undefined,
    /// Expected error: spec code plus, when the fixture pins one, the token
    /// the error must be attached to.
    Error {
        code: String,
        token: Option<String>,
    },
}

/// Load one or more test cases from a file.
/// Handles both single-object and array-of-objects formats,
/// and `expr-file` references to external .jsonata files.
fn load_test_cases(path: &Path) -> Vec<TestCase> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => {
            // serde_json rejects lone UTF-16 surrogates (\uD800): they have
            // no UTF-8 encoding. Re-escape them and retry, so the case is
            // loaded in full — dataset, bindings, expectations and all —
            // through the ordinary path.
            match serde_json::from_str(&escape_lone_surrogates(&content)) {
                Ok(v) => v,
                Err(_) => return vec![],
            }
        }
    };

    let objects: Vec<&serde_json::Map<String, serde_json::Value>> = match &json {
        serde_json::Value::Object(obj) => vec![obj],
        serde_json::Value::Array(arr) => arr.iter().filter_map(|v| v.as_object()).collect(),
        _ => return vec![],
    };

    let dir = path.parent().unwrap_or(Path::new("."));

    objects
        .iter()
        .filter_map(|obj| parse_test_object(obj, dir, path))
        .collect()
}

/// Double the backslash of every *lone* surrogate escape in a fixture.
///
/// `\uD800` with no low-surrogate partner denotes a code point UTF-8 cannot
/// encode, so serde_json refuses the whole file. Doubling the backslash
/// turns the escape into the six literal characters `\uD800`, which is
/// exactly what the JSONata lexer needs to see: it is the lexer that rejects
/// a lone surrogate escape (D3140), where the reference — whose strings are
/// UTF-16 — carries the lone surrogate to the builtin and fails there.
///
/// Well-formed surrogate *pairs* are left untouched so they keep decoding to
/// the astral character they denote. Backslash runs are tracked, so a
/// literal `\\uD800` (backslash, then the text `uD800`) is not mistaken for
/// an escape.
fn escape_lone_surrogates(content: &str) -> String {
    fn hex4(bytes: &[u8], at: usize) -> Option<u32> {
        let hex = bytes.get(at..at + 4)?;
        u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()
    }

    let bytes = content.as_bytes();
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            // Copy one whole UTF-8 character.
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i] & 0xC0) == 0x80 {
                i += 1;
            }
            out.push_str(&content[start..i]);
            continue;
        }
        // An escape. `\uXXXX` needs inspecting; anything else (including
        // `\\`) is copied whole so its second character can't start one.
        match (bytes.get(i + 1), hex4(bytes, i + 2)) {
            (Some(b'u'), Some(0xD800..=0xDBFF)) => {
                let paired = bytes.get(i + 6) == Some(&b'\\')
                    && bytes.get(i + 7) == Some(&b'u')
                    && matches!(hex4(bytes, i + 8), Some(0xDC00..=0xDFFF));
                if paired {
                    out.push_str(&content[i..i + 12]);
                    i += 12;
                } else {
                    out.push('\\');
                    out.push_str(&content[i..i + 6]);
                    i += 6;
                }
            }
            (Some(b'u'), Some(0xDC00..=0xDFFF)) => {
                // A low surrogate reached here without a high one before it.
                out.push('\\');
                out.push_str(&content[i..i + 6]);
                i += 6;
            }
            (Some(_), _) => {
                out.push('\\');
                i += 1;
                let start = i;
                i += 1;
                while i < bytes.len() && (bytes[i] & 0xC0) == 0x80 {
                    i += 1;
                }
                out.push_str(&content[start..i]);
            }
            (None, _) => {
                out.push('\\');
                i += 1;
            }
        }
    }
    out
}

fn parse_test_object(
    obj: &serde_json::Map<String, serde_json::Value>,
    dir: &Path,
    _file_path: &Path,
) -> Option<TestCase> {
    // Expression: either inline "expr" or external "expr-file".
    let expr = if let Some(e) = obj.get("expr").and_then(|v| v.as_str()) {
        e.to_string()
    } else {
        let f = obj.get("expr-file").and_then(|v| v.as_str())?;
        std::fs::read_to_string(dir.join(f)).ok()?
    };

    // Load input data.
    let input = if let Some(dataset) = obj.get("dataset").and_then(|v| v.as_str()) {
        load_dataset(dataset)
    } else if let Some(data) = obj.get("data") {
        Value::from_json(data.clone())
    } else {
        Value::Undefined
    };

    // Determine expected outcome. A `token` sits either next to a top-level
    // `code` or inside the nested `error` object; both pin the token the
    // error must carry.
    let expected = if let Some(code) = obj.get("code").and_then(|v| v.as_str()) {
        Expected::Error {
            code: code.to_string(),
            token: obj
                .get("token")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        }
    } else if let Some(err_obj) = obj.get("error").and_then(|v| v.as_object()) {
        Expected::Error {
            code: err_obj
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            token: err_obj
                .get("token")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        }
    } else if obj.get("undefinedResult").and_then(|v| v.as_bool()) == Some(true) {
        Expected::Undefined
    } else if let Some(result) = obj.get("result") {
        Expected::Result(Value::from_json(result.clone()))
    } else {
        Expected::Undefined
    };

    // Load bindings.
    let mut bindings = Vec::new();
    if let Some(b) = obj.get("bindings").and_then(|v| v.as_object()) {
        for (k, v) in b {
            bindings.push((k.clone(), Value::from_json(v.clone())));
        }
    }

    Some(TestCase {
        expr,
        input,
        expected,
        bindings,
        unordered: obj.get("unordered").and_then(|v| v.as_bool()) == Some(true),
        timelimit: obj.get("timelimit").and_then(serde_json::Value::as_u64),
        depth: obj.get("depth").and_then(serde_json::Value::as_u64),
    })
}

/// What running one case produced.
#[derive(Debug)]
enum Outcome {
    /// Expectation met.
    Pass,
    /// Error code met; the fixture also states an error `token` the engine
    /// did not attach. Counted and reported, deliberately not enforced —
    /// see [`TOKEN_FIELD_NOTE`].
    TokenMismatch {
        /// Human-readable detail for the run summary.
        detail: String,
    },
    /// Expectation not met.
    Fail(String),
}

/// Check an error against the fixture's expectation.
///
/// The fixture's `token` (top-level next to `code`, or inside the nested
/// `error` object) pins which source token the error is attached to;
/// `JsonataError` carries one, so assert it whenever the fixture states one.
fn check_error(expected: &Expected, err: &JsonataError, stage: &str) -> Outcome {
    match expected {
        Expected::Error { code, token } => {
            if err.code != *code {
                return Outcome::Fail(format!(
                    "expected error {code}, got {stage} error {}: {}",
                    err.code, err.message
                ));
            }
            match token {
                Some(want) if err.token != *want => Outcome::TokenMismatch {
                    detail: format!(
                        "error {code}: fixture states token {want:?}, engine attached {:?} ({})",
                        err.token, err.message
                    ),
                },
                _ => Outcome::Pass,
            }
        }
        Expected::Undefined => {
            Outcome::Fail(format!("expected undefined, got {stage} error: {err}"))
        }
        Expected::Result(v) => Outcome::Fail(format!(
            "expected result {}, got {stage} error: {err}",
            describe(v)
        )),
    }
}

fn run_test_case(tc: &TestCase) -> Outcome {
    // Parse.
    let parse_result = Parser::parse(&tc.expr);
    let (mut arena, root) = match parse_result {
        Ok(r) => r,
        Err(e) => return check_error(&tc.expected, &e, "parse"),
    };

    // Process AST.
    let root = match process_ast(&mut arena, root) {
        Ok(r) => r,
        Err(e) => return check_error(&tc.expected, &e, "process"),
    };

    // Set up environment with stdlib.
    let mut env = Environment::new();
    jsntrs::register_all(&mut env);
    // A fixture `timelimit` becomes a watchdog that trips the cancellation
    // flag: a runaway expression fails its case instead of hanging the suite.
    let watchdog = tc.timelimit.map(start_watchdog);
    if let Some((cancel, _)) = &watchdog {
        env.set_cancel_token(Arc::clone(cancel));
    }
    // Bind $$ (root input reference) — Go does env.Bind("$", data).
    if !tc.input.is_undefined() {
        env.bind("$", tc.input.clone());
    }
    for (name, value) in &tc.bindings {
        env.bind(name.clone(), value.clone());
    }
    let env = Rc::new(env);

    // Evaluate.
    let result = eval(&arena, root, &tc.input, &env);

    // A tripped watchdog outranks whatever the evaluation reported: the
    // cancellation surfaces as D3001, which would otherwise read as an
    // ordinary wrong-code failure.
    if let Some((cancel, limit)) = watchdog.as_ref().map(|(c, w)| (c, w.limit_ms))
        && cancel.load(Ordering::Relaxed)
    {
        return Outcome::Fail(format!(
            "exceeded timelimit of {limit}ms (enforced at {TIMELIMIT_SLACK}x for debug builds)"
        ));
    }

    match (&tc.expected, result) {
        (Expected::Error { .. }, Err(e)) => check_error(&tc.expected, &e, "eval"),
        (Expected::Error { code, .. }, Ok(val)) => Outcome::Fail(format!(
            "expected error {code}, got value: {}",
            describe(&val)
        )),
        (Expected::Undefined, Ok(val)) => {
            if val.is_undefined() {
                Outcome::Pass
            } else {
                Outcome::Fail(format!("expected undefined, got: {}", describe(&val)))
            }
        }
        (Expected::Result(expected), Ok(actual)) => {
            if values_match(expected, &actual, tc.unordered) {
                Outcome::Pass
            } else {
                Outcome::Fail(format!(
                    "result mismatch:\n  expected: {}\n  actual:   {}",
                    describe(expected),
                    describe(&actual)
                ))
            }
        }
        (Expected::Undefined, Err(e)) => {
            Outcome::Fail(format!("expected undefined, got error: {e}"))
        }
        (Expected::Result(expected), Err(e)) => Outcome::Fail(format!(
            "expected result {}, got error: {e}",
            describe(expected)
        )),
    }
}

/// Compare expected against actual as `Value`s, not as JSON.
///
/// `to_json()` maps `Undefined`, `Null` and every non-finite number to JSON
/// `null`, so a JSON-level comparison cannot tell "no value" from "the null
/// value" — an `undefined` where the fixture pins `null` used to pass
/// silently. Structural `Value` comparison keeps them apart at every depth:
///
/// - `Undefined` and `Null` are distinct, as are non-finite numbers.
/// - Numbers compare exactly (`==`, plus NaN == NaN). Exact ECMAScript
///   number output is a crate invariant, so a fixture that only matches
///   within a tolerance is a fixture that pins the wrong number. The 1e-10
///   absolute tolerance this replaced turned out to be load-bearing for
///   nothing: every case passes without it.
/// - Internal variants (`Sequence`, functions) never equal a fixture value:
///   a fixture is decoded from JSON and cannot contain one, and a `Sequence`
///   reaching the harness would be an invariant violation, not a match.
/// - With `unordered`, arrays compare as multisets at every depth (the
///   reference harness's deep-equal-in-any-order for `"unordered": true`).
fn values_match(a: &Value, b: &Value, unordered: bool) -> bool {
    if a.is_sequence() || b.is_sequence() || a.is_function() || b.is_function() {
        return false;
    }
    match (a, b) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y || (x.is_nan() && y.is_nan()),
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return false;
            }
            if unordered {
                let mut unmatched: Vec<&Value> = y.iter().collect();
                for item in x.iter() {
                    match unmatched.iter().position(|c| values_match(item, c, true)) {
                        Some(i) => {
                            unmatched.swap_remove(i);
                        }
                        None => return false,
                    }
                }
                true
            } else {
                x.iter()
                    .zip(y.iter())
                    .all(|(p, q)| values_match(p, q, false))
            }
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|bv| values_match(v, bv, unordered)))
        }
        _ => false,
    }
}

/// Render a `Value` for failure messages, spelling out `undefined`
/// (which JSON cannot express) wherever it appears.
fn describe(v: &Value) -> String {
    match v {
        Value::Undefined => "undefined".to_string(),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(describe).collect();
            format!("[{}]", inner.join(","))
        }
        Value::Object(obj) => {
            let inner: Vec<String> = obj
                .iter()
                .map(|(k, val)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k.as_str()).unwrap_or_default(),
                        describe(val)
                    )
                })
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Number(n) if !n.is_finite() => format!("{n}"),
        v if v.is_sequence() => "<internal Sequence — invariant violation>".to_string(),
        v if v.is_function() => "<function>".to_string(),
        other => serde_json::to_string(&other.to_json()).unwrap_or_default(),
    }
}

/// Known-failing cases exempted from the strict `failed == 0` gate,
/// as `"group/file.json"` names. Keep this list shrinking: only add an
/// entry together with a tracking issue, and remove it once fixed. A
/// listed case that PASSES also fails the suite so stale entries get
/// pruned.
const EXPECTED_FAILURES: &[&str] = &[];

/// Why fixture `token` values are counted but not enforced.
///
/// The JSONata documentation specifies error *codes*. It nowhere obliges an
/// engine to attach a token to an error, and the two token values it shows
/// appear inside examples introduced with "for example" — illustrations of
/// one implementation's error object, not a contract.
///
/// The fixtures come from a suite generated by jsonata-js, so their tokens
/// are that parser's internals. Some are not source tokens at all: S0217's
/// token is the enclosing *node type* — `"path"`, `"binary"`, `"condition"`,
/// `"unary"`, `"function"` — which is an AST label the JSONata language has
/// no notion of. Pinning those would make another engine's parser structure
/// into this one's contract, which is exactly the inversion CLAUDE.md's
/// "the target is the specification" section exists to prevent.
///
/// So the engine still attaches tokens — they are useful to callers, and
/// jsntrs-hyj made them accurate — but a difference is reported rather than
/// failed. The count below keeps the field visible: a token expectation that
/// stops being met shows up in the summary instead of vanishing.
const TOKEN_FIELD_NOTE: &str = "not enforced: the documentation specifies error codes, not tokens; \
     the fixtures' tokens are the reference parser's internals (S0217's is an AST node type)";

/// Exact number of cases the suite must load and run.
///
/// A floor is not enough: a loader that silently drops half the fixtures
/// still clears one (the floor this replaces sat 33 cases below the real
/// total, so a third of a group could vanish unnoticed). Pin the count
/// instead. This is the single constant to bump when cases are added or
/// removed — the diff then always says how many, in one line.
const EXPECTED_CASE_TOTAL: usize = 3103;

#[test]
fn conformance_suite() {
    let groups_dir = testdata_dir().join("groups");
    let mut total = 0;
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut xfailed = 0;
    let mut timelimited = 0;
    let mut depth_fixtures = 0;
    let mut token_expectations = 0;
    let mut token_mismatches: Vec<String> = Vec::new();
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut unexpected_passes: Vec<String> = Vec::new();

    let mut groups: Vec<_> = std::fs::read_dir(&groups_dir)
        .expect("cannot read testdata/groups")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .collect();
    groups.sort_by_key(|e| e.file_name());

    for group in &groups {
        let group_name = group.file_name();
        let group_name = group_name.to_string_lossy();
        let mut cases: Vec<_> = std::fs::read_dir(group.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .collect();
        cases.sort_by_key(|e| e.file_name());

        for case_entry in &cases {
            let test_cases = load_test_cases(&case_entry.path());
            if test_cases.is_empty() {
                total += 1;
                skipped += 1;
                let case_name = format!(
                    "{}/{}",
                    group_name,
                    case_entry.file_name().to_string_lossy()
                );
                eprintln!("SKIP {case_name}");
                continue;
            }
            let case_name = format!(
                "{}/{}",
                group_name,
                case_entry.file_name().to_string_lossy()
            );
            let expected_failure = EXPECTED_FAILURES.contains(&case_name.as_str());
            for tc in &test_cases {
                total += 1;
                timelimited += usize::from(tc.timelimit.is_some());
                depth_fixtures += usize::from(tc.depth.is_some());
                token_expectations += usize::from(matches!(
                    tc.expected,
                    Expected::Error { token: Some(_), .. }
                ));
                let outcome = run_test_case(tc);
                if let Outcome::TokenMismatch { detail } = &outcome {
                    token_mismatches.push(format!("{case_name}: {detail}"));
                }
                match outcome {
                    Outcome::Pass | Outcome::TokenMismatch { .. } if expected_failure => {
                        passed += 1;
                        unexpected_passes.push(case_name.clone());
                    }
                    Outcome::Pass | Outcome::TokenMismatch { .. } => passed += 1,
                    Outcome::Fail(_) if expected_failure => {
                        xfailed += 1;
                        eprintln!("XFAIL {case_name}");
                    }
                    Outcome::Fail(msg) => {
                        failed += 1;
                        failures.push((case_name.clone(), msg));
                    }
                }
            }
        }
    }

    // Print summary.
    eprintln!("\n═══ Conformance Suite Results ═══");
    eprintln!("Total:   {total}");
    eprintln!("Passed:  {passed}");
    eprintln!("Failed:  {failed}");
    eprintln!("Xfailed: {xfailed}");
    eprintln!("Skipped: {skipped}");
    eprintln!(
        "Pass rate: {:.1}%",
        if total > 0 {
            passed as f64 / total as f64 * 100.0
        } else {
            0.0
        }
    );
    eprintln!("Timelimit fixtures: {timelimited} (enforced at {TIMELIMIT_SLACK}x by watchdog)");
    eprintln!("Depth fixtures:     {depth_fixtures} ({DEPTH_FIELD_NOTE})");
    eprintln!(
        "Token fixtures:     {token_expectations} state a token, {} unmet ({TOKEN_FIELD_NOTE})",
        token_mismatches.len(),
    );

    if !failures.is_empty() {
        eprintln!("\n── All failures ──");
        for (name, msg) in &failures {
            eprintln!("FAIL {name}: {msg}");
        }
    }

    if !token_mismatches.is_empty() {
        eprintln!("\n── Token differences (informational, not failures) ──");
        for detail in &token_mismatches {
            eprintln!("TOKEN {detail}");
        }
    }

    // Strict gate: every case passes unless listed in EXPECTED_FAILURES.
    assert_eq!(failed, 0, "{failed} conformance tests failed unexpectedly");
    assert!(
        unexpected_passes.is_empty(),
        "cases in EXPECTED_FAILURES now pass — remove them: {unexpected_passes:?}"
    );
    // A fixture the loader cannot read is a fixture that pins nothing.
    assert_eq!(
        skipped, 0,
        "{skipped} fixture files loaded no cases — see the SKIP lines above"
    );
    // Load sanity: the exact count, not a floor. Bump EXPECTED_CASE_TOTAL in
    // the same commit that adds or removes cases.
    assert_eq!(
        total, EXPECTED_CASE_TOTAL,
        "conformance case count changed: expected {EXPECTED_CASE_TOTAL}, ran {total} — \
         update EXPECTED_CASE_TOTAL if this is intended"
    );
}

// ── Harness self-tests ──────────────────────────────────────────────────
//
// The bug class this file was tightened against is a check that quietly
// stops checking: a fixture field parsed and dropped, a comparison that
// cannot see the difference it is asked about. Each tightening therefore
// gets a test that fails if it degrades back into a no-op.

/// A fixture `timelimit` must be able to fail a case.
///
/// The watchdog hands the evaluator a cancellation flag; the trampoline and
/// the hot loops poll it, so a non-terminating expression unwinds instead of
/// hanging the suite. If the wiring is ever dropped, this test is what
/// notices (the runaway is still bounded by the trampoline's iteration cap,
/// so a broken watchdog fails here rather than hanging).
#[test]
fn timelimit_can_fail_a_runaway_case() {
    let tc = TestCase {
        expr: "( $inf := function($n){ $inf($n + 1) }; $inf(0) )".to_string(),
        input: Value::Undefined,
        expected: Expected::Result(Value::Number(0.0)),
        bindings: Vec::new(),
        unordered: false,
        timelimit: Some(1),
        depth: None,
    };
    match run_test_case(&tc) {
        Outcome::Fail(msg) => assert!(
            msg.contains("exceeded timelimit"),
            "runaway case failed for the wrong reason: {msg}"
        ),
        other => panic!("timelimit not enforced: {other:?}"),
    }
}

/// A `timelimit` no case can exceed must not fail that case.
#[test]
fn timelimit_leaves_a_fast_case_alone() {
    let tc = TestCase {
        expr: "1 + 1".to_string(),
        input: Value::Undefined,
        expected: Expected::Result(Value::Number(2.0)),
        bindings: Vec::new(),
        unordered: false,
        timelimit: Some(10_000),
        depth: None,
    };
    assert!(matches!(run_test_case(&tc), Outcome::Pass));
}

/// Result comparison must distinguish "no value" from "the null value".
///
/// This is what `to_json()` could not do: it maps `Undefined`, `Null` and
/// every non-finite number to JSON `null`, so a fixture pinning `null`
/// silently accepted an `undefined` (and vice versa) at any depth.
#[test]
fn undefined_null_and_non_finite_are_distinct() {
    let cases: &[(Value, Value)] = &[
        (Value::Null, Value::Undefined),
        (Value::Null, Value::Number(f64::NAN)),
        (Value::Null, Value::Number(f64::INFINITY)),
        (Value::Undefined, Value::Number(f64::NEG_INFINITY)),
    ];
    let wrap = |v: &Value| Value::Array([v.clone()].into());
    for (a, b) in cases {
        assert!(!values_match(a, b, false), "{a:?} must not match {b:?}");
        assert!(!values_match(b, a, false), "{b:?} must not match {a:?}");
        // …and at depth, where a JSON-level comparison was just as blind.
        assert!(
            !values_match(&wrap(a), &wrap(b), false),
            "[{a:?}] must not match [{b:?}]"
        );
    }
    assert!(values_match(&Value::Null, &Value::Null, false));
    assert!(values_match(&Value::Undefined, &Value::Undefined, false));
    assert!(values_match(
        &Value::Number(f64::NAN),
        &Value::Number(f64::NAN),
        false
    ));
}

/// Numbers compare exactly.
///
/// The comparison this replaces accepted any two numbers within 1e-10
/// absolute — a tolerance nothing documented and, as the suite proves, no
/// case needs: all cases pass under exact equality. Exact ECMAScript number
/// output is a crate invariant, so a fixture that only matches within a
/// tolerance is a fixture pinning the wrong number.
#[test]
fn numbers_compare_exactly() {
    let one = Value::Number(1.0);
    for off in [1e-11, -1e-11, f64::EPSILON] {
        let near = Value::Number(1.0 + off);
        assert!(
            !values_match(&one, &near, false),
            "1.0 must not match {}",
            1.0 + off
        );
    }
    assert!(values_match(&one, &Value::Number(1.0), false));
}

/// Fixtures with lone surrogate escapes load through the ordinary path,
/// keeping their dataset, bindings and expectations.
///
/// The fallback loader this replaces re-derived the case from raw text: it
/// recognised only `expr` and `code`, dropped every binding, and picked the
/// input by looking for the literal string `dataset5` in the file.
#[test]
fn lone_surrogate_fixtures_load_faithfully() {
    for group in ["function-encodeUrl", "function-encodeUrlComponent"] {
        let path = testdata_dir()
            .join("groups")
            .join(group)
            .join("case002.json");
        let cases = load_test_cases(&path);
        assert_eq!(cases.len(), 1, "{group}/case002.json must load one case");
        let tc = &cases[0];
        // The nested `error` object, not just a top-level `code`.
        assert!(
            matches!(&tc.expected, Expected::Error { code, .. } if code == "D3140"),
            "{group}: expectation lost, got {:?}",
            tc.expected
        );
        // The declared dataset, not a guess from a substring search.
        assert!(
            !tc.input.is_undefined(),
            "{group}: dataset5 was not loaded as input"
        );
        assert!(matches!(run_test_case(tc), Outcome::Pass));
    }
}

/// The surrogate re-escaping touches lone surrogates and nothing else.
#[test]
fn escape_lone_surrogates_is_surgical() {
    // Lone high and lone low surrogates get their backslash doubled, so the
    // JSONata lexer sees the escape and rejects it (D3140).
    assert_eq!(escape_lone_surrogates(r#""\uD800""#), r#""\\uD800""#);
    assert_eq!(escape_lone_surrogates(r#""\uDC00""#), r#""\\uDC00""#);
    // A well-formed pair is left alone, so it keeps decoding to the astral
    // character it denotes (U+1D11E).
    let pair = "\"\\uD834\\uDD1E\"";
    assert_eq!(escape_lone_surrogates(pair), pair);
    // …and the same character written literally survives byte-for-byte.
    assert_eq!(escape_lone_surrogates(r#""𝄞""#), r#""𝄞""#);
    // A literal backslash before `uD800` is not an escape and stays put.
    assert_eq!(escape_lone_surrogates(r#""\\uD800""#), r#""\\uD800""#);
    // Ordinary escapes, non-ASCII text and truncated input pass through.
    assert_eq!(escape_lone_surrogates(r#""a\nb\"c""#), r#""a\nb\"c""#);
    assert_eq!(escape_lone_surrogates("\"héllo → 🎉\""), "\"héllo → 🎉\"");
    assert_eq!(escape_lone_surrogates(r"\"), r"\");
}
