//! Array functions: $count, $append, $reverse, $shuffle, $distinct, $flatten, $zip.

use std::rc::Rc;

use crate::error::{JsonataError, JsonataResult};
use crate::value::Value;

pub fn fn_count(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$count: argument is required"));
    }
    if args.len() > 1 {
        return Err(JsonataError::new("T0410", "$count: expects 1 argument"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Number(0.0));
    }
    match &args[0] {
        Value::Array(a) => Ok(Value::Number(a.len() as f64)),
        _ => Ok(Value::Number(1.0)), // scalar counts as 1
    }
}

pub fn fn_append(args: &[Value], _focus: &Value) -> JsonataResult {
    // Go caps the result at 10M elements (guards runaway growth in loops).
    const MAX_APPEND_SIZE: usize = 10_000_000;
    if args.len() < 2 {
        return Err(JsonataError::new("T0410", "$append: requires 2 arguments"));
    }
    let a = &args[0];
    let b = &args[1];
    // If either is undefined, return the other unchanged.
    if a.is_undefined() {
        return Ok(b.clone());
    }
    if b.is_undefined() {
        return Ok(a.clone());
    }
    let len_of = |v: &Value| match v {
        Value::Array(arr) => arr.len(),
        _ => 1,
    };
    if len_of(a) + len_of(b) > MAX_APPEND_SIZE {
        return Err(JsonataError::new(
            "D3010",
            format!("$append: result array exceeds maximum size of {MAX_APPEND_SIZE} elements"),
        ));
    }
    let mut result = match a {
        Value::Array(arr) => arr.to_vec(),
        other => vec![other.clone()],
    };
    match b {
        Value::Array(arr) => result.extend(arr.iter().cloned()),
        other => result.push(other.clone()),
    }
    Ok(Value::Array(Rc::from(result)))
}

pub fn fn_reverse(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$reverse: argument is required"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let mut arr = match &args[0] {
        Value::Array(a) => a.to_vec(),
        other => vec![other.clone()],
    };
    arr.reverse();
    Ok(Value::Array(Rc::from(arr)))
}

pub fn fn_shuffle(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$shuffle: argument is required"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let mut arr = match &args[0] {
        Value::Array(a) => a.to_vec(),
        other => vec![other.clone()],
    };
    // Fisher-Yates shuffle.
    for i in (1..arr.len()).rev() {
        let j = fastrand::usize(..=i);
        arr.swap(i, j);
    }
    Ok(Value::Array(Rc::from(arr)))
}

pub fn fn_distinct(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new(
            "T0410",
            "$distinct: argument is required",
        ));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let arr = match &args[0] {
        Value::Array(a) => a,
        other => return Ok(other.clone()),
    };
    let mut result = Vec::new();
    for item in arr.iter() {
        if !result
            .iter()
            .any(|existing: &Value| existing.deep_equal(item))
        {
            result.push(item.clone());
        }
    }
    Ok(Value::Array(Rc::from(result)))
}

pub fn fn_flatten(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$flatten: argument is required"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let arr = match &args[0] {
        Value::Array(a) => a,
        other => return Ok(other.clone()),
    };
    let depth = args
        .get(1)
        .and_then(super::super::value::Value::as_f64)
        .map_or(usize::MAX, |n| n as usize);
    let result = flatten_recursive(arr, depth);
    Ok(Value::Array(Rc::from(result)))
}

/// Recursively flatten nested arrays up to the given depth — the body of
/// `$flatten`, shared with `fast_path`'s lifted `$flatten` so the two cannot
/// drift (jsntrs-6d5.2).
pub(crate) fn flatten_recursive(arr: &[Value], depth: usize) -> Vec<Value> {
    let mut result = Vec::new();
    for item in arr {
        if depth > 0
            && let Value::Array(inner) = item
        {
            result.extend(flatten_recursive(inner.as_ref(), depth - 1));
            continue;
        }
        result.push(item.clone());
    }
    result
}

pub fn fn_zip(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new(
            "T0410",
            "$zip: requires at least 1 argument",
        ));
    }
    // If any argument is undefined, return empty array.
    if args.iter().any(super::super::value::Value::is_undefined) {
        return Ok(Value::Array(Rc::from(vec![])));
    }
    // Wrap non-array args as singleton arrays.
    let arrays: Vec<Vec<Value>> = args
        .iter()
        .map(|a| match a {
            Value::Array(arr) => arr.to_vec(),
            other => vec![other.clone()],
        })
        .collect();
    if arrays.is_empty() {
        return Ok(Value::Array(Rc::from(vec![])));
    }
    // Use minimum length across all arrays.
    let min_len = arrays.iter().map(std::vec::Vec::len).min().unwrap_or(0);
    let mut result = Vec::with_capacity(min_len);
    for i in 0..min_len {
        let tuple: Vec<Value> = arrays.iter().map(|a| a[i].clone()).collect();
        result.push(Value::Array(Rc::from(tuple)));
    }
    Ok(Value::Array(Rc::from(result)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const U: &Value = &Value::Undefined;

    fn n(x: f64) -> Value {
        Value::Number(x)
    }
    fn arr(items: Vec<Value>) -> Value {
        Value::Array(Rc::from(items))
    }
    fn ok(r: JsonataResult) -> Value {
        match r {
            Ok(v) => v,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }

    /// $append caps the result at 10M elements with D3010, like Go
    /// (Go-verified 2026-08-07: $append([1..5000000],[1..5000001])
    /// errors D3010).
    #[test]
    fn append_caps_result_size() {
        let a = arr(vec![n(0.0); 5_000_000]);
        let b = arr(vec![n(0.0); 5_000_001]);
        let err = fn_append(&[a.clone(), b], U).unwrap_err();
        assert_eq!(err.code, "D3010");
        // At exactly the cap it succeeds.
        let b2 = arr(vec![n(0.0); 5_000_000]);
        assert!(matches!(ok(fn_append(&[a, b2], U)), Value::Array(r) if r.len() == 10_000_000));
    }

    /// $append treats undefined as the empty sequence and wraps scalars.
    #[test]
    fn append_handles_undefined_and_scalars() {
        assert!(ok(fn_append(&[Value::Undefined, n(2.0)], U)).deep_equal(&n(2.0)));
        assert!(ok(fn_append(&[n(1.0), Value::Undefined], U)).deep_equal(&n(1.0)));
        assert!(ok(fn_append(&[n(1.0), n(2.0)], U)).deep_equal(&arr(vec![n(1.0), n(2.0)])));
        assert!(
            ok(fn_append(
                &[arr(vec![n(1.0)]), arr(vec![n(2.0), n(3.0)])],
                U
            ))
            .deep_equal(&arr(vec![n(1.0), n(2.0), n(3.0)]))
        );
    }

    /// $count: undefined → 0, scalar → 1, array → length.
    #[test]
    fn count_of_undefined_is_zero() {
        assert!(ok(fn_count(&[Value::Undefined], U)).deep_equal(&n(0.0)));
        assert!(ok(fn_count(&[n(7.0)], U)).deep_equal(&n(1.0)));
        assert!(ok(fn_count(&[arr(vec![n(1.0), n(2.0)])], U)).deep_equal(&n(2.0)));
    }

    #[test]
    fn reverse_reverses() {
        assert!(
            ok(fn_reverse(&[arr(vec![n(1.0), n(2.0), n(3.0)])], U)).deep_equal(&arr(vec![
                n(3.0),
                n(2.0),
                n(1.0)
            ]))
        );
    }

    /// $distinct keeps the first occurrence, compared by deep equality.
    #[test]
    fn distinct_dedups_by_deep_equality() {
        assert!(
            ok(fn_distinct(
                &[arr(vec![n(1.0), n(2.0), n(1.0), n(3.0), n(2.0)])],
                U
            ))
            .deep_equal(&arr(vec![n(1.0), n(2.0), n(3.0)]))
        );
        let obj = |k: f64| {
            let mut m = crate::value::ObjectMap::default();
            m.insert("a".into(), n(k));
            Value::Object(Rc::new(m))
        };
        assert!(
            ok(fn_distinct(&[arr(vec![obj(1.0), obj(1.0)])], U)).deep_equal(&arr(vec![obj(1.0)]))
        );
    }

    #[test]
    fn flatten_honors_depth() {
        let nested = arr(vec![arr(vec![n(1.0), arr(vec![n(2.0)])]), n(3.0)]);
        assert!(
            ok(fn_flatten(std::slice::from_ref(&nested), U)).deep_equal(&arr(vec![
                n(1.0),
                n(2.0),
                n(3.0)
            ]))
        );
        assert!(ok(fn_flatten(&[nested, n(1.0)], U)).deep_equal(&arr(vec![
            n(1.0),
            arr(vec![n(2.0)]),
            n(3.0)
        ])));
    }

    /// $zip truncates to the shortest input; any undefined input → [].
    #[test]
    fn zip_truncates_to_shortest() {
        assert!(
            ok(fn_zip(
                &[arr(vec![n(1.0), n(2.0)]), arr(vec![n(3.0), n(4.0), n(5.0)])],
                U
            ))
            .deep_equal(&arr(vec![
                arr(vec![n(1.0), n(3.0)]),
                arr(vec![n(2.0), n(4.0)])
            ]))
        );
        let z = ok(fn_zip(&[arr(vec![n(1.0)]), Value::Undefined], U));
        assert!(matches!(&z, Value::Array(a) if a.is_empty()));
    }
}
