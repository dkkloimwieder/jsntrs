//! Error handling, cancellation, and edge cases.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jsntrs::Value;
use jsntrs::{Expression, JsonataError};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // --- Parse errors (S0xxx) ---

    println!("=== Parse Errors ===\n");

    match Expression::compile("1 +") {
        Err(e) => println!("  Incomplete expr: [{}] {}", e.code, e.message),
        Ok(_) => println!("  (unexpected success)"),
    }

    match Expression::compile("foo(bar") {
        Err(e) => println!("  Missing paren:   [{}] {}", e.code, e.message),
        Ok(_) => println!("  (unexpected success)"),
    }

    // --- Type errors (T0xxx) ---

    println!("\n=== Type Errors ===\n");

    let expr = Expression::compile(r#""hello" + 5"#)?;
    match expr.evaluate("") {
        Err(e) => println!("  String + Number: [{}] {}", e.code, e.message),
        Ok(v) => println!("  Result: {}", v.stringify(false)?),
    }

    // --- Error code matching ---

    println!("\n=== Error Code Matching ===\n");

    let handle_error = |e: &JsonataError| match &e.code[..2] {
        "S0" => println!("  Syntax error: {}", e.message),
        "T0" | "T1" | "T2" => println!("  Type error: {}", e.message),
        "D1" | "D2" | "D3" => println!("  Runtime error: {}", e.message),
        "U1" => println!("  Stack overflow: {}", e.message),
        _ => println!("  Unknown error [{}]: {}", e.code, e.message),
    };

    for bad_expr in &["1 + +", r#"$unknown()"#, r#""a" - "b""#] {
        match Expression::compile(bad_expr).and_then(|e| e.evaluate(r#"{"x": 1}"#)) {
            Err(e) => {
                print!("  {:20}", format!("{bad_expr:?}"));
                handle_error(&e);
            }
            Ok(v) => println!(
                "  {:20} => {}",
                format!("{bad_expr:?}"),
                v.stringify(false)?
            ),
        }
    }

    // --- Cancellation ---

    println!("\n=== Cancellation ===\n");

    let cancel = Arc::new(AtomicBool::new(false));

    // Pre-cancel before evaluation
    cancel.store(true, Ordering::Relaxed);
    let expr = Expression::compile("$sum($map([1,2,3,4,5], function($v){$v * $v}))")?;
    match expr.evaluate_with_cancel("{}", cancel.clone()) {
        Err(e) => println!("  Pre-cancelled: [{}] {}", e.code, e.message),
        Ok(v) => println!("  Result: {}", v.stringify(false)?),
    }

    // Cancel from another thread during evaluation
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_clone = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1));
        cancel_clone.store(true, Ordering::Relaxed);
    });

    // Long-running, not endless: uncancelled this exponential recursion stops
    // with U1001 once the lambda call depth passes DEFAULT_MAX_CALL_DEPTH
    // (100). Cancellation is what makes it stop *early*.
    let expr = Expression::compile(
        r#"( $f := function($n){$n > 0 ? $f($n - 1) + $f($n - 2) : 1}; $f(100) )"#,
    )?;
    match expr.evaluate_with_cancel("{}", cancel) {
        Err(e) => println!("  Thread-cancelled: [{}] {}", e.code, e.message),
        Ok(v) => println!("  Result: {}", v.stringify(false)?),
    }

    // --- Undefined vs Null ---

    println!("\n=== Undefined vs Null ===\n");

    let data = r#"{"a": null, "b": 42}"#;

    let cases = vec![
        ("Existing null field", "a"),
        ("Missing field", "c"),
        ("Null = Null", "a = null"),
        ("Undefined = Undefined", "c = c"),
        ("$type(null)", "$type(a)"),
        ("$type(missing)", "$type(c)"),
        ("$exists(null)", "$exists(a)"),
        ("$exists(missing)", "$exists(c)"),
    ];

    for (label, expr_str) in cases {
        let expr = Expression::compile(expr_str)?;
        let result = expr.evaluate(data)?;
        let display = if result.is_undefined() {
            "undefined".to_string()
        } else {
            result.stringify(false)?
        };
        println!("  {label:30} => {display}");
    }

    // --- deep_equal semantics ---

    println!("\n=== deep_equal ===\n");

    let pairs = vec![
        (Value::Null, Value::Null, true),
        (Value::Undefined, Value::Undefined, false), // JSONata spec!
        (Value::Number(42.0), Value::Number(42.0), true),
        (Value::Number(0.0), Value::Bool(false), false), // No coercion
    ];

    for (a, b, expected) in pairs {
        let result = jsntrs::deep_equal(&a, &b);
        let status = if result == expected { "ok" } else { "MISMATCH" };
        println!("  deep_equal({:?}, {:?}) = {result:5}  ({status})", a, b,);
    }

    // --- Type predicates ---

    println!("\n=== Type Predicates ===\n");

    let values: Vec<(&str, Value)> = vec![
        ("Undefined", Value::Undefined),
        ("Null", Value::Null),
        ("Bool", Value::Bool(true)),
        ("Number", Value::Number(42.0)),
        ("String", Value::String("hi".into())),
    ];

    for (label, v) in &values {
        println!(
            "  {label:12} is_number={:5} is_string={:5} to_boolean={:5}",
            v.is_number(),
            v.is_string(),
            // Coercion is fallible: an infinity is D1001, not a truth value.
            v.to_boolean()?,
        );
    }

    Ok(())
}
