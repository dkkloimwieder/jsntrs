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
