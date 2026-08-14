//! Object functions: $keys, $values, $spread, $merge, $lookup, $error.

use std::rc::Rc;

use crate::error::{JsonataError, JsonataResult};
use crate::value::Value;

pub fn fn_keys(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$keys: argument is required"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    match &args[0] {
        Value::Object(obj) => {
            if obj.is_empty() {
                return Ok(Value::Undefined);
            }
            let keys: Vec<Value> = obj
                .keys()
                .map(|k| Value::String(k.as_str().into()))
                .collect();
            if keys.len() == 1 {
                return Ok(keys.into_iter().next().unwrap_or(Value::Undefined));
            }
            Ok(Value::Array(Rc::from(keys)))
        }
        Value::Array(arr) => {
            // Collect all keys from array of objects.
            let mut all_keys = Vec::new();
            for item in arr.iter() {
                if let Value::Object(obj) = item {
                    for k in obj.keys() {
                        if !all_keys
                            .iter()
                            .any(|existing: &Value| matches!(existing, Value::String(s) if s.as_str() == k.as_str()))
                        {
                            all_keys.push(Value::String(compact_str::CompactString::from(k.as_str())));
                        }
                    }
                }
            }
            if all_keys.is_empty() {
                return Ok(Value::Undefined);
            }
            if all_keys.len() == 1 {
                return Ok(all_keys.into_iter().next().unwrap_or(Value::Undefined));
            }
            Ok(Value::Array(Rc::from(all_keys)))
        }
        _ => Ok(Value::Undefined),
    }
}

pub fn fn_values(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$values: argument is required"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    match &args[0] {
        Value::Object(obj) => {
            let vals: Vec<Value> = obj.values().cloned().collect();
            Ok(Value::Array(Rc::from(vals)))
        }
        Value::Array(arr) => {
            // Collect values across object elements; non-objects are
            // skipped. Nothing collected → undefined (Go: nil result).
            let mut vals = Vec::new();
            for item in arr.iter() {
                if let Value::Object(obj) = item {
                    vals.extend(obj.values().cloned());
                }
            }
            if vals.is_empty() {
                return Ok(Value::Undefined);
            }
            Ok(Value::Array(Rc::from(vals)))
        }
        _ => Ok(Value::Undefined),
    }
}

pub fn fn_spread(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$spread: argument is required"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let spread_one = |obj: &crate::value::ObjectMap| -> Vec<Value> {
        obj.iter()
            .map(|(k, v)| {
                let mut m = crate::value::ObjectMap::default();
                m.insert(k.clone(), v.clone());
                Value::Object(Rc::new(m))
            })
            .collect()
    };
    match &args[0] {
        Value::Object(obj) => {
            let items = spread_one(obj);
            Ok(Value::Sequence(Box::new(
                crate::value::Sequence::with_items(items),
            )))
        }
        Value::Array(arr) => {
            let mut result = Vec::new();
            for item in arr.iter() {
                if let Value::Object(obj) = item {
                    result.extend(spread_one(obj));
                } else {
                    // Non-object elements pass through unchanged (Go/spec).
                    result.push(item.clone());
                }
            }
            Ok(Value::Array(Rc::from(result)))
        }
        _ => Ok(args[0].clone()),
    }
}

pub fn fn_merge(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$merge: argument is required"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let mut merged = crate::value::ObjectMap::default();
    let merge_obj = |merged: &mut crate::value::ObjectMap, obj: &crate::value::ObjectMap| {
        for (k, v) in obj {
            merged.insert(k.clone(), v.clone());
        }
    };
    match &args[0] {
        Value::Array(arr) => {
            for item in arr.iter() {
                let Value::Object(obj) = item else {
                    return Err(JsonataError::new(
                        "T0412",
                        "$merge: array elements must be objects",
                    ));
                };
                merge_obj(&mut merged, obj);
            }
        }
        Value::Object(obj) => merge_obj(&mut merged, obj),
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$merge: argument must be an array of objects",
            ));
        }
    }
    Ok(Value::Object(Rc::new(merged)))
}

pub fn fn_lookup(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.len() < 2 {
        return Err(JsonataError::new("T0410", "$lookup: requires 2 arguments"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let key: &str = match &args[1] {
        Value::String(s) => s,
        _ => return Err(JsonataError::new("T0410", "$lookup: key must be a string")),
    };
    match &args[0] {
        Value::Object(obj) => Ok(obj.get(key).cloned().unwrap_or(Value::Undefined)),
        Value::Array(arr) => {
            // Lookup across array of objects.
            let mut result = Vec::new();
            for item in arr.iter() {
                if let Value::Object(obj) = item
                    && let Some(v) = obj.get(key)
                {
                    result.push(v.clone());
                }
            }
            match result.len() {
                0 => Ok(Value::Undefined),
                1 => Ok(result.into_iter().next().unwrap_or(Value::Undefined)),
                _ => Ok(Value::Array(Rc::from(result))),
            }
        }
        _ => Ok(Value::Undefined),
    }
}

pub fn fn_error(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() || args[0].is_undefined() {
        return Err(JsonataError::new("D3137", "$error() function evaluated"));
    }
    match &args[0] {
        Value::String(s) => Err(JsonataError::new("D3137", s.to_string())),
        _ => Err(JsonataError::new(
            "T0410",
            "$error: argument must be a string",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const U: &Value = &Value::Undefined;

    fn obj(pairs: &[(&str, f64)]) -> Value {
        let mut m = crate::value::ObjectMap::default();
        for (k, v) in pairs {
            m.insert((*k).into(), Value::Number(*v));
        }
        Value::Object(Rc::new(m))
    }
    fn ok(r: JsonataResult) -> Value {
        match r {
            Ok(v) => v,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    fn strings(v: &Value) -> Vec<String> {
        match v {
            Value::Array(items) => items
                .iter()
                .map(|x| match x {
                    Value::String(s) => s.to_string(),
                    other => panic!("expected string element, got {other:?}"),
                })
                .collect(),
            Value::String(s) => vec![s.to_string()],
            other => panic!("expected array or string, got {other:?}"),
        }
    }

    /// $keys preserves insertion order (behavioral invariant #8) and
    /// unions keys across an array of objects.
    #[test]
    fn keys_preserve_insertion_order_and_union_arrays() {
        let keys = ok(fn_keys(&[obj(&[("z", 1.0), ("a", 2.0), ("m", 3.0)])], U));
        assert_eq!(strings(&keys), ["z", "a", "m"]);
        // Singleton key collapses to a plain string.
        assert_eq!(strings(&ok(fn_keys(&[obj(&[("only", 1.0)])], U))), ["only"]);
        // Empty object → undefined.
        assert!(matches!(fn_keys(&[obj(&[])], U), Ok(Value::Undefined)));
        // Array of objects: first-seen order, no duplicates.
        let arr = Value::Array(Rc::from(vec![
            obj(&[("b", 1.0), ("a", 2.0)]),
            obj(&[("a", 9.0), ("c", 3.0)]),
        ]));
        assert_eq!(strings(&ok(fn_keys(&[arr], U))), ["b", "a", "c"]);
    }

    #[test]
    fn values_returns_object_values() {
        let vals = ok(fn_values(&[obj(&[("a", 1.0), ("b", 2.0)])], U));
        assert!(vals.deep_equal(&Value::Array(Rc::from(vec![
            Value::Number(1.0),
            Value::Number(2.0)
        ]))));
    }

    /// $merge: later objects win; key order is first-seen.
    #[test]
    fn merge_is_last_writer_wins_in_first_seen_order() {
        let merged = ok(fn_merge(
            &[Value::Array(Rc::from(vec![
                obj(&[("a", 1.0), ("b", 2.0)]),
                obj(&[("b", 9.0), ("c", 3.0)]),
            ]))],
            U,
        ));
        assert!(merged.deep_equal(&obj(&[("a", 1.0), ("b", 9.0), ("c", 3.0)])));
        assert_eq!(strings(&ok(fn_keys(&[merged], U))), ["a", "b", "c"]);
    }

    /// $lookup maps across an array of objects and skips misses.
    #[test]
    fn lookup_maps_across_arrays() {
        let key = Value::String("a".into());
        assert!(
            ok(fn_lookup(&[obj(&[("a", 1.0)]), key.clone()], U)).deep_equal(&Value::Number(1.0))
        );
        assert!(matches!(
            fn_lookup(&[obj(&[("b", 1.0)]), key.clone()], U),
            Ok(Value::Undefined)
        ));
        let arr = Value::Array(Rc::from(vec![
            obj(&[("a", 1.0)]),
            obj(&[("x", 0.0)]),
            obj(&[("a", 2.0)]),
        ]));
        assert!(
            ok(fn_lookup(&[arr, key], U)).deep_equal(&Value::Array(Rc::from(vec![
                Value::Number(1.0),
                Value::Number(2.0)
            ])))
        );
    }

    /// $spread splits an object into single-pair objects.
    #[test]
    fn spread_splits_into_single_pair_objects() {
        let spread = match fn_spread(&[obj(&[("a", 1.0), ("b", 2.0)])], U) {
            Ok(Value::Sequence(seq)) => seq.into_value(),
            other => panic!("expected sequence, got {other:?}"),
        };
        assert!(spread.deep_equal(&Value::Array(Rc::from(vec![
            obj(&[("a", 1.0)]),
            obj(&[("b", 2.0)])
        ]))));
    }

    /// Go-verified (2026-08-07): non-object array elements pass through.
    #[test]
    fn spread_passes_non_objects_through() {
        let arr = Value::Array(Rc::from(vec![
            obj(&[("a", 1.0), ("b", 2.0)]),
            Value::Number(3.0),
            Value::String("x".into()),
        ]));
        assert!(
            ok(fn_spread(&[arr], U)).deep_equal(&Value::Array(Rc::from(vec![
                obj(&[("a", 1.0)]),
                obj(&[("b", 2.0)]),
                Value::Number(3.0),
                Value::String("x".into()),
            ])))
        );
    }

    /// Go-verified (2026-08-07): a non-object element raises T0412.
    #[test]
    fn merge_rejects_non_object_elements() {
        let arr = Value::Array(Rc::from(vec![obj(&[("a", 1.0)]), Value::Number(2.0)]));
        let err = match fn_merge(&[arr], U) {
            Err(e) => e,
            other => panic!("expected T0412, got {other:?}"),
        };
        assert_eq!(err.code, "T0412");
    }

    /// Go-verified (2026-08-07): $values on arrays collects across object
    /// elements, skips non-objects, and is undefined when nothing collects.
    #[test]
    fn values_collects_across_array_elements() {
        let arr = Value::Array(Rc::from(vec![
            obj(&[("a", 1.0)]),
            obj(&[("b", 2.0), ("c", 3.0)]),
            Value::Number(5.0),
        ]));
        assert!(
            ok(fn_values(&[arr], U)).deep_equal(&Value::Array(Rc::from(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
            ])))
        );
        let no_objects = Value::Array(Rc::from(vec![Value::Number(1.0), Value::Number(2.0)]));
        assert!(matches!(fn_values(&[no_objects], U), Ok(Value::Undefined)));
        let empty_objects = Value::Array(Rc::from(vec![obj(&[])]));
        assert!(matches!(
            fn_values(&[empty_objects], U),
            Ok(Value::Undefined)
        ));
    }

    #[test]
    fn error_raises_with_message() {
        let err = match fn_error(&[Value::String("boom".into())], U) {
            Err(e) => e,
            other => panic!("expected error, got {other:?}"),
        };
        assert_eq!(err.code, "D3137");
        assert_eq!(err.message, "boom");
    }
}
