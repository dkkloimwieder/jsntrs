//! Conformance test harness: runs the JSONata test suite from testdata/groups/.
//!
//! Each test case is a JSON file with:
//! - `expr`: JSONata expression string
//! - `dataset` or `data`: input data (dataset name or inline JSON)
//! - `result`: expected result (if successful)
//! - `undefinedResult`: true if result should be undefined
//! - `code`: expected error code (if expression should fail)
//! - `bindings`: optional variable bindings

use std::path::{Path, PathBuf};
use std::rc::Rc;

use jsntrs::Environment;
use jsntrs::Value;
use jsntrs::eval;
use jsntrs::{Parser, process_ast};

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
}

#[derive(Debug)]
enum Expected {
    Result(Value),
    Undefined,
    Error(String), // error code
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
            // serde_json fails on lone UTF-16 surrogates (\uD800).
            // Extract test case info via regex to avoid skipping.
            return load_surrogate_test_cases(&content, path);
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

/// Fallback for test files containing lone UTF-16 surrogates that serde_json
/// can't parse. Extracts the expression and expected error code via raw string
/// matching. The JSON \uD800 escape is kept as literal characters so our
/// JSONata lexer sees `\uD800` and rejects it with D3140.
fn load_surrogate_test_cases(content: &str, _path: &Path) -> Vec<TestCase> {
    // Extract "expr": "..." — keep JSON escapes as-is (don't decode \uD800).
    let expr = extract_json_string_raw(content, "expr");
    let expr = match expr {
        Some(e) => e,
        None => return vec![],
    };

    // Extract error code from "code" or nested "error"."code".
    let code = extract_json_string_raw(content, "code");
    let expected = match code {
        Some(c) => Expected::Error(c),
        None => return vec![],
    };

    let input = if content.contains("\"dataset5\"") {
        load_dataset("dataset5")
    } else {
        Value::Undefined
    };

    vec![TestCase {
        expr,
        input,
        expected,
        bindings: Vec::new(),
        unordered: false,
    }]
}

/// Extract a JSON string value by key from raw text without full JSON parsing.
/// Returns the raw content between quotes (with JSON escapes preserved as-is).
fn extract_json_string_raw(content: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let i = content.find(&needle)?;
    let rest = &content[i + needle.len()..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    if !after.starts_with('"') {
        return None;
    }
    let s = &after[1..];
    let mut end = 0;
    let bytes = s.as_bytes();
    while end < bytes.len() {
        if bytes[end] == b'\\' {
            end += 2;
        } else if bytes[end] == b'"' {
            break;
        } else {
            end += 1;
        }
    }
    Some(s[..end].to_string())
}

fn parse_test_object(
    obj: &serde_json::Map<String, serde_json::Value>,
    dir: &Path,
    _file_path: &Path,
) -> Option<TestCase> {
    // Expression: either inline "expr" or external "expr-file".
    let expr = if let Some(e) = obj.get("expr").and_then(|v| v.as_str()) {
        e.to_string()
    } else if let Some(f) = obj.get("expr-file").and_then(|v| v.as_str()) {
        std::fs::read_to_string(dir.join(f)).ok()?
    } else {
        return None;
    };

    // Load input data.
    let input = if let Some(dataset) = obj.get("dataset").and_then(|v| v.as_str()) {
        load_dataset(dataset)
    } else if let Some(data) = obj.get("data") {
        Value::from_json(data.clone())
    } else {
        Value::Undefined
    };

    // Determine expected outcome.
    let expected = if let Some(code) = obj.get("code").and_then(|v| v.as_str()) {
        Expected::Error(code.to_string())
    } else if let Some(err_obj) = obj.get("error").and_then(|v| v.as_object()) {
        let code = err_obj
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Expected::Error(code)
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
    })
}

fn run_test_case(tc: &TestCase) -> Result<(), String> {
    // Parse.
    let parse_result = Parser::parse(&tc.expr);
    let (mut arena, root) = match parse_result {
        Ok(r) => r,
        Err(e) => {
            return match &tc.expected {
                Expected::Error(code) if e.code == *code => Ok(()),
                Expected::Error(code) => {
                    Err(format!("expected error {code}, got parse error: {e}"))
                }
                _ => Err(format!("parse error: {e}")),
            };
        }
    };

    // Process AST.
    let root = match process_ast(&mut arena, root) {
        Ok(r) => r,
        Err(e) => {
            return match &tc.expected {
                Expected::Error(code) if e.code == *code => Ok(()),
                _ => Err(format!("process error: {e}")),
            };
        }
    };

    // Set up environment with stdlib.
    let mut env = Environment::new();
    jsntrs::register_all(&mut env);
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

    match (&tc.expected, result) {
        (Expected::Error(code), Err(e)) => {
            if e.code == *code {
                Ok(())
            } else {
                Err(format!(
                    "expected error {code}, got error {}: {}",
                    e.code, e.message
                ))
            }
        }
        (Expected::Error(code), Ok(val)) => {
            Err(format!("expected error {code}, got value: {val:?}"))
        }
        (Expected::Undefined, Ok(val)) => {
            if val.is_undefined() {
                Ok(())
            } else {
                Err(format!("expected undefined, got: {val:?}"))
            }
        }
        (Expected::Result(expected), Ok(actual)) => {
            if values_match(expected, &actual, tc.unordered) {
                Ok(())
            } else {
                Err(format!(
                    "result mismatch:\n  expected: {}\n  actual:   {}",
                    serde_json::to_string(&expected.to_json()).unwrap_or_default(),
                    serde_json::to_string(&actual.to_json()).unwrap_or_default()
                ))
            }
        }
        (Expected::Undefined, Err(e)) => Err(format!("expected undefined, got error: {e}")),
        (Expected::Result(expected), Err(e)) => Err(format!(
            "expected result {}, got error: {e}",
            serde_json::to_string(&expected.to_json()).unwrap_or_default()
        )),
    }
}

/// Compare values, treating both as JSON for comparison.
fn values_match(expected: &Value, actual: &Value, unordered: bool) -> bool {
    // Compare via JSON representation for robustness.
    let ej = expected.to_json();
    let aj = actual.to_json();
    if unordered {
        json_equal_unordered(&ej, &aj)
    } else {
        json_equal(&ej, &aj)
    }
}

/// Like `json_equal`, but arrays compare as multisets at every depth —
/// the reference harness's deep-equal-in-any-order for fixtures flagged
/// `"unordered": true`.
fn json_equal_unordered(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    match (a, b) {
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) => {
            if a.len() != b.len() {
                return false;
            }
            let mut unmatched: Vec<&serde_json::Value> = b.iter().collect();
            for x in a {
                match unmatched.iter().position(|y| json_equal_unordered(x, y)) {
                    Some(i) => {
                        unmatched.swap_remove(i);
                    }
                    None => return false,
                }
            }
            true
        }
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|bv| json_equal_unordered(v, bv)))
        }
        _ => json_equal(a, b),
    }
}

fn json_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    match (a, b) {
        (serde_json::Value::Null, serde_json::Value::Null) => true,
        (serde_json::Value::Bool(a), serde_json::Value::Bool(b)) => a == b,
        (serde_json::Value::Number(a), serde_json::Value::Number(b)) => {
            // Compare as f64 for numeric equality.
            let af = a.as_f64().unwrap_or(f64::NAN);
            let bf = b.as_f64().unwrap_or(f64::NAN);
            (af - bf).abs() < 1e-10 || (af.is_nan() && bf.is_nan())
        }
        (serde_json::Value::String(a), serde_json::Value::String(b)) => a == b,
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| json_equal(x, y))
        }
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|bv| json_equal(v, bv)))
        }
        _ => false,
    }
}

/// Known-failing cases exempted from the strict `failed == 0` gate,
/// as `"group/file.json"` names. Keep this list shrinking: only add an
/// entry together with a tracking issue, and remove it once fixed. A
/// listed case that PASSES also fails the suite so stale entries get
/// pruned.
const EXPECTED_FAILURES: &[&str] = &[];

#[test]
fn conformance_suite() {
    let groups_dir = testdata_dir().join("groups");
    let mut total = 0;
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut xfailed = 0;
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
                match run_test_case(tc) {
                    Ok(()) if expected_failure => {
                        passed += 1;
                        unexpected_passes.push(case_name.clone());
                    }
                    Ok(()) => passed += 1,
                    Err(_) if expected_failure => {
                        xfailed += 1;
                        eprintln!("XFAIL {case_name}");
                    }
                    Err(msg) => {
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

    if !failures.is_empty() {
        eprintln!("\n── All failures ──");
        for (name, msg) in &failures {
            eprintln!("FAIL {name}: {msg}");
        }
    }

    // Strict gate: every case passes unless listed in EXPECTED_FAILURES.
    assert_eq!(failed, 0, "{failed} conformance tests failed unexpectedly");
    assert!(
        unexpected_passes.is_empty(),
        "cases in EXPECTED_FAILURES now pass — remove them: {unexpected_passes:?}"
    );
    // Load-sanity floor: guards against the harness silently loading or
    // skipping large parts of the suite (1,733 cases at time of writing).
    assert!(
        passed >= 1700,
        "expected at least 1700 conformance tests to pass, got {passed} — \
         did the suite fail to load?"
    );
}
