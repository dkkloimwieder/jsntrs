#![no_main]

use jsntrs::{ObjectMap, Value};
use jsntrs::Expression;
use libfuzzer_sys::fuzz_target;
use std::rc::Rc;

/// Inputs spanning every Value shape: scalars, strings, arrays, nested objects.
fn build_datasets() -> Vec<Value> {
    let mut item1 = ObjectMap::default();
    item1.insert("x".into(), Value::Number(1.0));
    item1.insert("name".into(), Value::String("alpha".into()));
    let mut item2 = ObjectMap::default();
    item2.insert("x".into(), Value::Number(2.5));
    item2.insert("name".into(), Value::String("beta".into()));

    let mut inner = ObjectMap::default();
    inner.insert(
        "b".into(),
        Value::Array(Rc::from(vec![
            Value::Number(1.0),
            Value::Number(2.0),
            Value::Number(3.0),
        ])),
    );
    let mut root = ObjectMap::default();
    root.insert("a".into(), Value::Object(Rc::new(inner)));
    root.insert("name".into(), Value::String("root".into()));
    root.insert("flag".into(), Value::Bool(true));
    root.insert("nothing".into(), Value::Null);
    root.insert(
        "items".into(),
        Value::Array(Rc::from(vec![
            Value::Object(Rc::new(item1)),
            Value::Object(Rc::new(item2)),
        ])),
    );

    vec![
        Value::Undefined,
        Value::Null,
        Value::Bool(false),
        Value::Number(42.0),
        Value::Number(-0.5),
        Value::String("hello world".into()),
        Value::String(String::new().into()),
        Value::Array(Rc::from(Vec::new())),
        Value::Array(Rc::from(vec![
            Value::Number(3.0),
            Value::String("mixed".into()),
            Value::Null,
            Value::Bool(true),
        ])),
        Value::Object(Rc::new(ObjectMap::default())),
        Value::Object(Rc::new(root)),
    ]
}

thread_local! {
    static DATASETS: Vec<Value> = build_datasets();
}

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Compile + evaluate should never panic on any input.
        if let Ok(expr) = Expression::compile(s) {
            DATASETS.with(|datasets| {
                for dataset in datasets {
                    let _ = expr.evaluate_value(dataset);
                }
            });
        }
    }
});
