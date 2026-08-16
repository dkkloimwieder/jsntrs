//! Boolean functions: $boolean, $not, $exists.

use crate::error::{JsonataError, JsonataResult};
use crate::value::Value;

pub fn fn_boolean(args: &[Value], focus: &Value) -> JsonataResult {
    // Note: don't enforce arity — HOF callbacks like $filter($boolean)
    // pass 3 args (value, index, array). Only use the first arg.
    let arg = if args.is_empty() { focus } else { &args[0] };
    if arg.is_undefined() {
        return Ok(Value::Undefined);
    }
    Ok(Value::Bool(arg.to_boolean()?))
}

pub fn fn_not(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$not: argument is required"));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    Ok(Value::Bool(!args[0].to_boolean()?))
}

pub fn fn_exists(args: &[Value], _focus: &Value) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new("T0410", "$exists: argument is required"));
    }
    if args.len() > 1 {
        return Err(JsonataError::new(
            "T0410",
            "$exists: takes at most 1 argument",
        ));
    }
    Ok(Value::Bool(!args[0].is_undefined()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    const U: &Value = &Value::Undefined;

    fn b(r: JsonataResult) -> bool {
        match r {
            Ok(Value::Bool(v)) => v,
            other => panic!("expected bool, got {other:?}"),
        }
    }

    /// JSONata boolean coercion truth table (behavioral invariant
    /// INV-BOOL-COERCE: "0" is truthy, "" is falsy, "false" is truthy).
    #[test]
    fn boolean_coercion_truth_table() {
        assert!(b(fn_boolean(&[Value::String("0".into())], U)));
        assert!(b(fn_boolean(&[Value::String("false".into())], U)));
        assert!(!b(fn_boolean(&[Value::String("".into())], U)));
        assert!(!b(fn_boolean(&[Value::Number(0.0)], U)));
        assert!(b(fn_boolean(&[Value::Number(-0.5)], U)));
        assert!(!b(fn_boolean(&[Value::Null], U)));
        // Arrays: empty → false, singleton → element's truth, else any(truthy).
        assert!(!b(fn_boolean(
            &[Value::Array(Rc::from(Vec::<Value>::new()))],
            U
        )));
        assert!(!b(fn_boolean(
            &[Value::Array(Rc::from(vec![Value::Number(0.0)]))],
            U
        )));
        assert!(b(fn_boolean(
            &[Value::Array(Rc::from(vec![
                Value::Number(0.0),
                Value::Number(1.0)
            ]))],
            U
        )));
        // Objects: empty → false, non-empty → true.
        assert!(!b(fn_boolean(
            &[Value::Object(Rc::new(crate::value::ObjectMap::default()))],
            U
        )));
        // Undefined propagates.
        assert!(matches!(
            fn_boolean(&[Value::Undefined], U),
            Ok(Value::Undefined)
        ));
    }

    /// jsonata-js 2.2.2-verified (2026-08-15, jsntrs-p0v.25): `$boolean` and
    /// `$not` reach their number branch through `utils.isNumeric`, which
    /// throws D1001 on an infinity rather than answering. A NaN gets a plain
    /// `false` from it and falls through to a falsy result.
    #[test]
    fn boolean_rejects_infinity_and_reads_nan_as_false() {
        let err = |r: JsonataResult| match r {
            Err(e) => e.code,
            other => panic!("expected an error, got {other:?}"),
        };
        assert_eq!(err(fn_boolean(&[Value::Number(f64::INFINITY)], U)), "D1001");
        assert_eq!(
            err(fn_boolean(&[Value::Number(f64::NEG_INFINITY)], U)),
            "D1001"
        );
        assert_eq!(err(fn_not(&[Value::Number(f64::INFINITY)], U)), "D1001");
        assert!(!b(fn_boolean(&[Value::Number(f64::NAN)], U)));
        assert!(b(fn_not(&[Value::Number(f64::NAN)], U)));
        // The reference filters the whole array, so a truthy element ahead of
        // the infinity does not short-circuit the D1001 away.
        assert_eq!(
            err(fn_boolean(
                &[Value::Array(Rc::from(vec![
                    Value::Number(1.0),
                    Value::Number(f64::INFINITY),
                ]))],
                U
            )),
            "D1001"
        );
        // An object only has its keys counted, so an infinity inside is fine.
        let mut obj = crate::value::ObjectMap::default();
        obj.insert("a".into(), Value::Number(f64::INFINITY));
        assert!(b(fn_boolean(&[Value::Object(Rc::new(obj))], U)));
    }

    #[test]
    fn not_negates_and_propagates_undefined() {
        assert!(!b(fn_not(&[Value::Bool(true)], U)));
        assert!(b(fn_not(&[Value::String("".into())], U)));
        assert!(matches!(
            fn_not(&[Value::Undefined], U),
            Ok(Value::Undefined)
        ));
        assert!(fn_not(&[], U).is_err());
    }

    /// $exists is the one place undefined and null must diverge.
    #[test]
    fn exists_distinguishes_undefined_from_null() {
        assert!(!b(fn_exists(&[Value::Undefined], U)));
        assert!(b(fn_exists(&[Value::Null], U)));
    }
}
