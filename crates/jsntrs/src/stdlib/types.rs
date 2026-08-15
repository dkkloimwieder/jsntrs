//! Type and misc functions: $type, $assert.

use crate::error::{JsonataError, JsonataResult};
use crate::value::Value;

pub fn fn_type_of(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$type: argument is required"));
    }
    let type_name = match &args[0] {
        Value::Undefined => return Ok(Value::Undefined),
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        // The reference asks `utils.isNumeric` whether this is a number, and
        // that helper throws D1001 on an infinity instead of answering
        // (jsntrs-p0v.25). A NaN gets a plain `false` — no throw — so the
        // reference's `type()` walks past its string, boolean, array and
        // function branches and lands on the "object" default. Odd, but it
        // is the observable answer, and Infinity is the case that matters:
        // `1/0` and a JSON `1e400` literal both reach here.
        Value::Number(n) if n.is_infinite() => {
            return Err(JsonataError::with_code("D1001").with_value(crate::value::format_float(*n)));
        }
        Value::Number(n) if n.is_nan() => "object",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Function(_) => "function",
        Value::Sequence(_) | Value::TailCall(_) => "undefined",
    };
    Ok(Value::String(type_name.into()))
}

pub fn fn_assert(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$assert: argument is required"));
    }
    if args.len() > 2 {
        return Err(JsonataError::new(
            "T0410",
            "$assert: takes at most 2 arguments",
        ));
    }
    // First argument must be a boolean.
    match &args[0] {
        Value::Bool(b) => {
            if !b {
                let msg = args
                    .get(1)
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "assertion failed".into());
                return Err(JsonataError::new("D3141", msg));
            }
            Ok(Value::Undefined)
        }
        _ => Err(JsonataError::new(
            "T0410",
            "$assert: first argument must be a boolean",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    const U: &Value = &Value::Undefined;

    fn type_name(v: Value) -> String {
        match fn_type_of(&[v], U) {
            Ok(Value::String(s)) => s.to_string(),
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn type_of_names_every_value_kind() {
        assert_eq!(type_name(Value::Null), "null");
        assert_eq!(type_name(Value::Bool(true)), "boolean");
        assert_eq!(type_name(Value::Number(1.0)), "number");
        assert_eq!(type_name(Value::String("x".into())), "string");
        assert_eq!(
            type_name(Value::Array(Rc::from(Vec::<Value>::new()))),
            "array"
        );
        assert_eq!(
            type_name(Value::Object(Rc::new(crate::value::ObjectMap::default()))),
            "object"
        );
        assert!(matches!(
            fn_type_of(&[Value::Undefined], U),
            Ok(Value::Undefined)
        ));
    }

    /// jsonata-js 2.2.2-verified (2026-08-15, jsntrs-p0v.25): `type()` asks
    /// `utils.isNumeric`, which throws D1001 on an infinity. NaN gets a plain
    /// `false`, so the reference falls past every branch to its "object"
    /// default — surprising, but that is the observable answer.
    #[test]
    fn type_of_rejects_infinity_and_calls_nan_an_object() {
        let err = match fn_type_of(&[Value::Number(f64::INFINITY)], U) {
            Err(e) => e,
            other => panic!("expected an error, got {other:?}"),
        };
        assert_eq!(err.code, "D1001");
        assert_eq!(
            match fn_type_of(&[Value::Number(f64::NEG_INFINITY)], U) {
                Err(e) => e.code,
                other => panic!("expected an error, got {other:?}"),
            },
            "D1001"
        );
        assert_eq!(type_name(Value::Number(f64::NAN)), "object");
        // An infinity nested in a container is not the container's type.
        assert_eq!(
            type_name(Value::Array(Rc::from(vec![Value::Number(f64::INFINITY)]))),
            "array"
        );
    }

    #[test]
    fn assert_fails_with_custom_message() {
        assert!(matches!(
            fn_assert(&[Value::Bool(true)], U),
            Ok(Value::Undefined)
        ));
        let err = match fn_assert(&[Value::Bool(false), Value::String("boom".into())], U) {
            Err(e) => e,
            other => panic!("expected error, got {other:?}"),
        };
        assert_eq!(err.code, "D3141");
        assert_eq!(err.message, "boom");
        let err = match fn_assert(&[Value::Number(1.0)], U) {
            Err(e) => e,
            other => panic!("expected error, got {other:?}"),
        };
        assert_eq!(err.code, "T0410");
    }
}
