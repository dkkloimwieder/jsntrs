#![no_main]

//! Differential fuzzing: fast paths vs the general evaluator.
//!
//! Generates arbitrary item data, runs fast-path-recognized expressions
//! with fast paths enabled and disabled (`fast_path::testing`), and
//! asserts both produce identical results including error codes.

use arbitrary::Arbitrary;
use jsntrs::{ObjectMap, Value};
use jsntrs::Expression;
use libfuzzer_sys::fuzz_target;
use std::rc::Rc;

#[derive(Arbitrary, Debug)]
enum Scalar {
    Missing,
    Null,
    Bool(bool),
    Int(i32),
    /// Mapped onto a small set of interesting floats (incl. 0.5 steps).
    Float(i16),
    Str(String),
}

impl Scalar {
    fn to_value(&self) -> Option<Value> {
        match self {
            Scalar::Missing => None,
            Scalar::Null => Some(Value::Null),
            Scalar::Bool(b) => Some(Value::Bool(*b)),
            Scalar::Int(n) => Some(Value::Number(f64::from(*n))),
            Scalar::Float(n) => Some(Value::Number(f64::from(*n) / 2.0)),
            Scalar::Str(s) => Some(Value::String(s.as_str().into())),
        }
    }
}

#[derive(Arbitrary, Debug)]
struct Item {
    x: Scalar,
    y: Scalar,
    name: Scalar,
}

#[derive(Arbitrary, Debug)]
struct Input {
    expr_idx: u8,
    items: Vec<Item>,
}

/// Expressions recognized by at least one fast path.
const EXPRS: &[&str] = &[
    "$map(items, function($v){$v.x})",
    "$filter(items, function($v){$v.x > 2})",
    "$filter(items, function($v){$v.x != 3})",
    "$filter(items, function($v){$v.x = $v.y})",
    "$filter(items, function($v){$v.x > 1 and $v.y < 5})",
    "$sort(items, function($a, $b){$a.x > $b.x})",
    "$reduce(items, function($acc, $v){$acc + $v.x}, 0)",
    "$reduce(items, function($acc, $v){$acc + ($v.x * $v.y)}, 0)",
    "$map(items, function($v){$v.name & \"-\" & $string($v.x)})",
    "$map(items, function($v){$substring($v.name, 1, 3)})",
    "items.$round(x)",
    "items.$contains(name, \"a\")",
    "items.$formatBase(x, 16)",
    "items^(x)",
    "items.x",
    "$sum(items.x)",
    "$max(items.x)",
    "$count(items.x)",
    "$distinct(items.name)",
];

fuzz_target!(|input: Input| {
    let expr = EXPRS[input.expr_idx as usize % EXPRS.len()];
    let compiled = Expression::compile(expr).expect("fixed exprs always compile");

    let items: Vec<Value> = input
        .items
        .iter()
        .take(64)
        .map(|item| {
            let mut obj = ObjectMap::default();
            if let Some(v) = item.x.to_value() {
                obj.insert("x".into(), v);
            }
            if let Some(v) = item.y.to_value() {
                obj.insert("y".into(), v);
            }
            if let Some(v) = item.name.to_value() {
                obj.insert("name".into(), v);
            }
            Value::Object(Rc::new(obj))
        })
        .collect();
    let mut root = ObjectMap::default();
    root.insert("items".into(), Value::Array(Rc::from(items)));
    let data = Value::Object(Rc::new(root));

    jsntrs::fast_path_testing::set_fast_paths_disabled(false);
    let fast = compiled.evaluate_value(&data);
    jsntrs::fast_path_testing::set_fast_paths_disabled(true);
    let general = compiled.evaluate_value(&data);
    jsntrs::fast_path_testing::set_fast_paths_disabled(false);

    match (&fast, &general) {
        (Ok(a), Ok(b)) => {
            let matches = (a.is_undefined() && b.is_undefined()) || jsntrs::deep_equal(a, b);
            assert!(matches, "{expr}: fast {a:?} != general {b:?}");
        }
        (Err(a), Err(b)) => {
            assert_eq!(a.code, b.code, "{expr}: fast err != general err");
        }
        _ => panic!("{expr}: fast {fast:?} != general {general:?}"),
    }
});
