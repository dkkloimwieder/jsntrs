#![no_main]

//! Fuzz target: verify write_json produces identical output to serde_json.
//!
//! Generates arbitrary Value trees and asserts both serialization paths match.
//! Any divergence (escaping, number formatting, structure) is a bug.

use std::rc::Rc;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use jsntrs::{ObjectMap, Value};

/// Fuzzer-friendly Value generator. Limits depth and size to keep
/// iterations fast while still covering all Value variants.
#[derive(Debug, Arbitrary)]
enum FuzzValue {
    Null,
    Undefined,
    BoolTrue,
    BoolFalse,
    Zero,
    One,
    NegOne,
    Pi,
    Large(f64),
    EmptyStr,
    Str(String),
    EmptyArray,
    SmallArray(Vec<FuzzLeaf>),
    EmptyObject,
    SmallObject(Vec<(String, FuzzLeaf)>),
    Nested(Box<FuzzNested>),
}

/// Leaf values — no recursion, keeps the tree bounded.
#[derive(Debug, Arbitrary)]
enum FuzzLeaf {
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
}

/// One level of nesting.
#[derive(Debug, Arbitrary)]
struct FuzzNested {
    key: String,
    inner: FuzzLeaf,
    extra: Vec<FuzzLeaf>,
}

impl FuzzLeaf {
    fn to_value(&self) -> Value {
        match self {
            FuzzLeaf::Null => Value::Null,
            FuzzLeaf::Bool(b) => Value::Bool(*b),
            FuzzLeaf::Number(n) => Value::Number(*n),
            FuzzLeaf::Str(s) => Value::String(s.as_str().into()),
        }
    }
}

impl FuzzValue {
    fn to_value(&self) -> Value {
        match self {
            FuzzValue::Null => Value::Null,
            FuzzValue::Undefined => Value::Undefined,
            FuzzValue::BoolTrue => Value::Bool(true),
            FuzzValue::BoolFalse => Value::Bool(false),
            FuzzValue::Zero => Value::Number(0.0),
            FuzzValue::One => Value::Number(1.0),
            FuzzValue::NegOne => Value::Number(-1.0),
            FuzzValue::Pi => Value::Number(std::f64::consts::PI),
            FuzzValue::Large(n) => Value::Number(*n),
            FuzzValue::EmptyStr => Value::String("".into()),
            FuzzValue::Str(s) => Value::String(s.as_str().into()),
            FuzzValue::EmptyArray => Value::Array(Rc::from(vec![])),
            FuzzValue::SmallArray(items) => {
                let vals: Vec<Value> = items.iter().map(FuzzLeaf::to_value).collect();
                Value::Array(Rc::from(vals))
            }
            FuzzValue::EmptyObject => Value::Object(Rc::new(ObjectMap::default())),
            FuzzValue::SmallObject(pairs) => {
                let mut map = ObjectMap::default();
                for (k, v) in pairs {
                    map.insert(k.as_str().into(), v.to_value());
                }
                Value::Object(Rc::new(map))
            }
            FuzzValue::Nested(n) => {
                let mut map = ObjectMap::default();
                map.insert(n.key.as_str().into(), n.inner.to_value());
                let arr: Vec<Value> = n.extra.iter().map(FuzzLeaf::to_value).collect();
                map.insert("arr".into(), Value::Array(Rc::from(arr)));
                Value::Object(Rc::new(map))
            }
        }
    }
}

fuzz_target!(|input: FuzzValue| {
    let val = input.to_value();

    // Path 1: write_json (direct-to-bytes)
    let direct = val.to_json_string();

    // Path 2: to_json() + serde_json (reference)
    let json_val = val.to_json();
    let reference = serde_json::to_string(&json_val).unwrap();

    assert_eq!(
        direct, reference,
        "write_json diverged from serde_json for {val:?}"
    );
});
