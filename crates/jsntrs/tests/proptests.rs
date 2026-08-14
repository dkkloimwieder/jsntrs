//! Property-based tests using proptest.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use compact_str::CompactString;
use proptest::prelude::*;

use jsntrs::Expression;
use jsntrs::{ObjectMap, Value};

// ── Value strategy ──────────────────────────────────────────────────────────

/// Generate arbitrary JSON-representable Values (no Sequence/Function/TailCall).
fn arb_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        // Finite f64 only — NaN/Inf serialize to null, breaking roundtrip.
        any::<f64>()
            .prop_filter("must be finite and not -0", |n| n.is_finite()
                && !(*n == 0.0 && n.is_sign_negative()))
            .prop_map(Value::Number),
        "[a-zA-Z0-9_ ]{0,50}".prop_map(|s| Value::String(CompactString::from(s))),
    ];

    leaf.prop_recursive(
        3,  // max depth
        64, // max nodes
        4,  // items per collection
        |inner| {
            prop_oneof![
                // Arrays
                prop::collection::vec(inner.clone(), 0..5).prop_map(|v| Value::Array(Rc::from(v))),
                // Objects
                prop::collection::vec(("[a-zA-Z_][a-zA-Z0-9_]{0,10}", inner), 0..5,).prop_map(
                    |pairs| {
                        let mut map = ObjectMap::default();
                        for (k, v) in pairs {
                            map.insert(CompactString::from(k), v);
                        }
                        Value::Object(Rc::from(map))
                    }
                ),
            ]
        },
    )
}

// ── Property: Value JSON roundtrip ──────────────────────────────────────────

proptest! {
    #[test]
    fn value_json_roundtrip(val in arb_value()) {
        let json_val = val.to_json();
        let json_str = serde_json::to_string(&json_val).unwrap();
        // Use serde_json for roundtrip (simd-json rejects some ryu-js formatted numbers).
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        prop_assert_eq!(json_val, parsed);
    }
}

// ── Property: deep_equal reflexivity ────────────────────────────────────────

proptest! {
    #[test]
    fn deep_equal_reflexive(val in arb_value()) {
        // deep_equal(undefined, undefined) is false by JSONata spec.
        if !val.is_undefined() {
            prop_assert!(val.deep_equal(&val), "deep_equal should be reflexive for {:?}", val);
        }
    }
}

// ── Property: deep_equal symmetry ───────────────────────────────────────────

proptest! {
    #[test]
    fn deep_equal_symmetric(a in arb_value(), b in arb_value()) {
        prop_assert_eq!(
            a.deep_equal(&b),
            b.deep_equal(&a),
            "deep_equal should be symmetric for {:?} and {:?}", a, b,
        );
    }
}

// ── Property: parse_signature never panics ──────────────────────────────────

proptest! {
    #[test]
    fn parse_signature_no_panic(s in "[ -~]{0,30}") {
        // Should return Ok or Err, never panic.
        let _ = jsntrs::parse_signature(&s);
    }
}

// ── Property: Parser::parse never panics ────────────────────────────────────

proptest! {
    #[test]
    fn parser_parse_no_panic(s in "[ -~]{0,50}") {
        let _ = jsntrs::Parser::parse(&s);
    }
}

// ── Property: to_boolean is total (never panics) ────────────────────────────

proptest! {
    #[test]
    fn to_boolean_total(val in arb_value()) {
        let _ = val.to_boolean();
    }
}

// ── Property: to_boolean idempotent via Bool ────────────────────────────────

proptest! {
    #[test]
    fn to_boolean_idempotent(val in arb_value()) {
        let b = val.to_boolean();
        prop_assert_eq!(Value::Bool(b).to_boolean(), b);
    }
}

// ── Property: stringify never panics for JSON-representable values ───────────

proptest! {
    #[test]
    fn stringify_no_panic(val in arb_value()) {
        let _ = val.stringify(false);
    }
}

// ── Property: coerce_to_array always returns an array ───────────────────────

proptest! {
    #[test]
    fn coerce_to_array_returns_array(val in arb_value()) {
        let arr = val.coerce_to_array();
        // Undefined coerces to empty array; everything else wraps or stays.
        if val.is_undefined() {
            prop_assert_eq!(arr.len(), 0);
        } else if val.is_array() {
            // Arrays stay as-is.
        } else {
            prop_assert_eq!(arr.len(), 1);
        }
    }
}

// ── Property: as_f64 returns Some only for Number and numeric strings ───────

proptest! {
    #[test]
    fn as_f64_for_numbers(n in any::<f64>().prop_filter("finite", |n| n.is_finite())) {
        let val = Value::Number(n);
        prop_assert_eq!(val.as_f64(), Some(n));
    }
}

// ── Property: type predicate exclusivity ────────────────────────────────────

proptest! {
    #[test]
    fn type_predicates_exclusive(val in arb_value()) {
        // Exactly one of the base type predicates should be true.
        let checks = [
            val.is_undefined(),
            val.is_null(),
            val.is_bool(),
            val.is_number(),
            val.is_string(),
            val.is_array(),
            val.is_object(),
        ];
        let true_count = checks.iter().filter(|&&x| x).count();
        prop_assert_eq!(true_count, 1, "Expected exactly 1 type predicate true for {:?}, got {}", val, true_count);
    }
}

// ── Property: Expression::compile + evaluate never panics ───────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn compile_eval_no_panic(expr in "[ -~]{0,40}") {
        if let Ok(compiled) = Expression::compile(&expr) {
            let _ = compiled.evaluate_value(&Value::Undefined);
            let _ = compiled.evaluate_value(&Value::Null);
            let _ = compiled.evaluate_value(&Value::Number(42.0));
            let _ = compiled.evaluate_value(&Value::String("hello".into()));
        }
    }
}

// ── Property: Expression::compile + evaluate with cancel never panics ───────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn compile_eval_with_cancel_no_panic(expr in "[ -~]{0,30}") {
        if let Ok(compiled) = Expression::compile(&expr) {
            let cancel = Arc::new(AtomicBool::new(false));
            let _ = compiled.evaluate_with_cancel("null", cancel);
        }
    }
}

// ── Property: compare_order is antisymmetric ────────────────────────────────

proptest! {
    #[test]
    fn compare_order_antisymmetric(a in arb_value(), b in arb_value()) {
        if let (Ok(ab), Ok(ba)) = (a.compare_order(&b), b.compare_order(&a)) {
            prop_assert_eq!(ab, -ba, "compare_order should be antisymmetric for {:?} and {:?}", a, b);
        }
    }
}

// ── Property: deep_equal consistent with to_json equality ───────────────────

proptest! {
    #[test]
    fn deep_equal_consistent_with_json(a in arb_value(), b in arb_value()) {
        // If two values produce the same JSON, they should be deep_equal
        // (except Undefined, which maps to null in JSON).
        if !a.is_undefined() && !b.is_undefined() && a.to_json() == b.to_json() {
            prop_assert!(a.deep_equal(&b), "same JSON but not deep_equal: {:?} vs {:?}", a, b);
        }
    }
}

// ── Property: Lexer never panics ────────────────────────────────────────────

proptest! {
    #[test]
    fn lexer_no_panic(s in "[ -~]{0,60}") {
        let mut lexer = jsntrs::Lexer::new(&s);
        let mut infix = false;
        for _ in 0..200 {
            match lexer.next(infix) {
                Ok(tok) => {
                    if tok.typ == jsntrs::TokenType::EOF {
                        break;
                    }
                    // Toggle infix based on token type.
                    infix = matches!(
                        tok.typ,
                        jsntrs::TokenType::Name
                            | jsntrs::TokenType::Number
                            | jsntrs::TokenType::String
                            | jsntrs::TokenType::Value
                            | jsntrs::TokenType::Variable
                            | jsntrs::TokenType::RBracket
                            | jsntrs::TokenType::RBrace
                            | jsntrs::TokenType::RParen
                    );
                }
                Err(_) => break,
            }
        }
    }
}

// ── Property: Number formatting produces parseable output ────────────────────

proptest! {
    #[test]
    fn number_format_parseable(n in any::<f64>().prop_filter("finite", |n| n.is_finite())) {
        let formatted = jsntrs::format_float(n);
        // format_float (ryu-js) must always produce valid numeric output.
        let parsed: Result<f64, _> = formatted.parse();
        prop_assert!(parsed.is_ok(), "format_float produced unparseable output '{}' for {}", formatted, n);
    }
}
