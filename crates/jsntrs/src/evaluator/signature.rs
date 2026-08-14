//! Function signature parsing and argument validation.
//!
//! Port of Go `internal/parser/signature.go` and `internal/evaluator/signature.go`.

// Reachable only through #[doc(hidden)] re-exports for in-repo tooling;
// not part of the documented public API.
#![expect(missing_docs)]
use std::rc::Rc;

use crate::error::JsonataError;
use crate::value::Value;

/// Parsed parameter spec from a function signature.
#[derive(Debug, Clone)]
pub struct ParamSpec {
    pub types: Vec<u8>,   // accepted base types: b n s l a o f j x
    pub content_type: u8, // for 'a': element type constraint; 0 = any
    pub optional: bool,   // ? — param may be omitted
    pub variadic: bool,   // + — repeats; must be the last spec
}

/// Parse a raw function signature string (content between outer < > brackets).
///
/// # Errors
/// Returns `S0401` or `S0402` for invalid signature syntax.
pub fn parse_signature(raw: &str) -> Result<Vec<ParamSpec>, JsonataError> {
    let s = strip_return_type(raw);
    let bytes = s.as_bytes();
    let mut specs = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        let mut spec = ParamSpec {
            types: Vec::new(),
            content_type: 0,
            optional: false,
            variadic: false,
        };

        if bytes[i] == b'(' {
            i += 1;
            let (types, next) = parse_union(bytes, i)?;
            spec.types = types;
            i = next;
        } else {
            if !is_valid_sig_type(bytes[i]) {
                return Err(sig_error(
                    "S0402",
                    &format!("unknown type specifier {:?} in signature", bytes[i] as char),
                ));
            }
            spec.types = vec![bytes[i]];
            i += 1;
        }

        if i < bytes.len() && bytes[i] == b'<' {
            let (ct, next) = parse_content_type(bytes, i, &spec.types)?;
            spec.content_type = ct;
            i = next;
        }

        while i < bytes.len() && matches!(bytes[i], b'?' | b'+' | b'-') {
            match bytes[i] {
                b'?' => spec.optional = true,
                b'+' => spec.variadic = true,
                _ => {}
            }
            i += 1;
        }

        specs.push(spec);
    }
    Ok(specs)
}

/// Process call arguments: nil propagation, singleton coercion, type validation.
/// Returns (coerced_args, return_undefined); `coerced_args` is `None` when the
/// original arguments pass through unchanged, so the common no-coercion call
/// avoids cloning every argument.
///
/// # Errors
/// Returns `T0410` or `T0412` for argument type mismatches.
pub fn process_call_args(
    specs: &[ParamSpec],
    args: &[Value],
) -> Result<(Option<Vec<Value>>, bool), JsonataError> {
    // Fast path: nothing to collapse and nothing to coerce — validate the
    // originals in place and hand them back without cloning.
    if !args.iter().any(|v| matches!(v, Value::Sequence(_))) {
        let mut needs_coercion = false;
        for (i, spec) in specs.iter().enumerate() {
            if spec.variadic || i >= args.len() {
                break;
            }
            // Nil propagation: undefined arg for a typed param → return undefined.
            if args[i].is_undefined() && !arg_matches_types(&Value::Undefined, &spec.types) {
                return Ok((None, true));
            }
            if needs_singleton_coercion(spec, &args[i]) {
                needs_coercion = true;
                break;
            }
        }
        if !needs_coercion {
            validate_call_args(specs, args)?;
            return Ok((None, false));
        }
    }

    let mut coerced: Vec<Value> = args.to_vec();

    // Collapse any Sequence values before processing.
    for v in &mut coerced {
        if matches!(v, Value::Sequence(_)) {
            let owned = std::mem::replace(v, Value::Undefined);
            if let Value::Sequence(seq) = owned {
                *v = seq.into_value();
            }
        }
    }

    for (i, spec) in specs.iter().enumerate() {
        if spec.variadic {
            break;
        }
        if i >= coerced.len() {
            break;
        }

        // Nil propagation: undefined arg for a typed param → return undefined.
        if coerced[i].is_undefined() && !arg_matches_types(&Value::Undefined, &spec.types) {
            return Ok((None, true));
        }

        // Singleton coercion: 'a' param with a non-array value → [arg].
        if needs_singleton_coercion(spec, &coerced[i]) {
            let v = std::mem::replace(&mut coerced[i], Value::Undefined);
            coerced[i] = Value::Array(Rc::from(vec![v]));
        }
    }

    validate_call_args(specs, &coerced)?;
    Ok((Some(coerced), false))
}

/// `'a'` param with a non-array value that satisfies the content type → `[arg]`.
fn needs_singleton_coercion(spec: &ParamSpec, v: &Value) -> bool {
    spec.types.contains(&b'a')
        && !v.is_undefined()
        && !arg_matches_types(v, b"a")
        && (spec.content_type == 0 || arg_matches_types(v, &[spec.content_type]))
}

/// Validate arguments against parameter specs.
fn validate_call_args(specs: &[ParamSpec], args: &[Value]) -> Result<(), JsonataError> {
    let mut si = 0;
    let mut ai = 0;

    while si < specs.len() {
        let spec = &specs[si];

        if spec.variadic {
            while ai < args.len() {
                validate_one_arg(spec, &args[ai], ai + 1)?;
                ai += 1;
            }
            si += 1;
            continue;
        }

        if ai >= args.len() {
            if !spec.optional {
                return Err(JsonataError::new(
                    "T0410",
                    format!(
                        "argument {} does not match function signature: too few arguments",
                        ai + 1
                    ),
                ));
            }
            si += 1;
            continue;
        }

        validate_one_arg(spec, &args[ai], ai + 1)?;
        ai += 1;
        si += 1;
    }

    if ai < args.len() {
        return Err(JsonataError::new(
            "T0410",
            format!(
                "argument {} does not match function signature: too many arguments",
                ai + 1
            ),
        ));
    }

    Ok(())
}

/// Validate a single argument against one parameter spec.
fn validate_one_arg(spec: &ParamSpec, arg: &Value, pos: usize) -> Result<(), JsonataError> {
    if !arg_matches_types(arg, &spec.types) {
        if spec.content_type != 0 {
            return Err(JsonataError::new(
                "T0412",
                format!(
                    "argument {} must be an array of {}",
                    pos, spec.content_type as char
                ),
            ));
        }
        return Err(JsonataError::new(
            "T0410",
            format!("argument {pos} does not match function signature"),
        ));
    }

    // Content-type validation for array elements.
    if spec.content_type != 0
        && let Value::Array(arr) = arg
    {
        let ct = [spec.content_type];
        for elem in arr.iter() {
            if !arg_matches_types(elem, &ct) {
                return Err(JsonataError::new(
                    "T0412",
                    format!(
                        "argument {} must be an array of {}",
                        pos, spec.content_type as char
                    ),
                ));
            }
        }
    }

    Ok(())
}

/// Check if a value matches at least one of the type chars.
fn arg_matches_types(arg: &Value, types: &[u8]) -> bool {
    types.iter().any(|&t| type_matches(arg, t))
}

/// Check if a value matches a single type character.
fn type_matches(arg: &Value, t: u8) -> bool {
    match t {
        b'x' => true,               // anything including functions
        b'j' => !arg.is_function(), // any JSON value (not function)
        b'n' => arg.is_number(),
        b's' => arg.is_string(),
        b'b' => arg.is_bool(),
        b'l' => arg.is_undefined() || arg.is_null(),
        b'a' => arg.is_array(),
        b'o' => arg.is_object(),
        b'f' => arg.is_function(),
        _ => false,
    }
}

// ── Signature string parsing ────────────────────────────────────────

fn parse_union(bytes: &[u8], start: usize) -> Result<(Vec<u8>, usize), JsonataError> {
    let mut types = Vec::new();
    let mut i = start;
    while i < bytes.len() && bytes[i] != b')' {
        if bytes[i] == b'<' {
            return Err(sig_error(
                "S0402",
                "content-type specifier '<' is not allowed inside a union type group",
            ));
        }
        if !is_valid_sig_type(bytes[i]) {
            return Err(sig_error(
                "S0402",
                &format!("unknown type specifier {:?} in signature", bytes[i] as char),
            ));
        }
        types.push(bytes[i]);
        i += 1;
    }
    if i >= bytes.len() {
        return Err(sig_error("S0402", "unclosed union type group in signature"));
    }
    if types.is_empty() {
        return Err(sig_error("S0402", "empty union type group in signature"));
    }
    Ok((types, i + 1)) // +1 to consume ')'
}

fn parse_content_type(
    bytes: &[u8],
    start: usize,
    types: &[u8],
) -> Result<(u8, usize), JsonataError> {
    for &t in types {
        if t != b'a' && t != b'f' {
            return Err(sig_error(
                "S0401",
                &format!(
                    "content-type specifier '<' is not valid for type {:?}",
                    t as char
                ),
            ));
        }
    }
    let mut i = start + 1; // consume '<'
    let mut content_type = 0u8;

    if types.contains(&b'a') && i < bytes.len() && is_valid_sig_type(bytes[i]) {
        content_type = bytes[i];
        i += 1;
        if i < bytes.len() && bytes[i] == b'<' {
            i = skip_brackets(bytes, i);
        }
    } else {
        i = skip_brackets_keep_close(bytes, i);
    }

    if i >= bytes.len() || bytes[i] != b'>' {
        return Err(sig_error("S0402", "unclosed content-type specifier"));
    }
    Ok((content_type, i + 1))
}

fn skip_brackets(bytes: &[u8], start: usize) -> usize {
    let mut depth = 1;
    let mut i = start + 1;
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'<' => depth += 1,
            b'>' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    i
}

fn skip_brackets_keep_close(bytes: &[u8], start: usize) -> usize {
    let mut depth = 1;
    let mut i = start;
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'<' => depth += 1,
            b'>' => depth -= 1,
            _ => {}
        }
        if depth > 0 {
            i += 1;
        }
    }
    i
}

fn strip_return_type(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut paren_depth = 0i32;
    let mut angle_depth = 0i32;
    let mut last_colon = None;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => paren_depth += 1,
            b')' => paren_depth -= 1,
            b'<' => angle_depth += 1,
            b'>' => angle_depth -= 1,
            b':' if paren_depth == 0 && angle_depth == 0 => last_colon = Some(i),
            _ => {}
        }
    }
    match last_colon {
        Some(i) => &s[..i],
        None => s,
    }
}

fn is_valid_sig_type(c: u8) -> bool {
    matches!(
        c,
        b'b' | b'n' | b's' | b'l' | b'a' | b'o' | b'f' | b'j' | b'x'
    )
}

fn sig_error(code: &'static str, msg: &str) -> JsonataError {
    JsonataError::new(code, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_types() {
        let specs = parse_signature("sns").unwrap();
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0].types, vec![b's']);
        assert_eq!(specs[1].types, vec![b'n']);
        assert_eq!(specs[2].types, vec![b's']);
    }

    #[test]
    fn parse_optional_and_variadic() {
        let specs = parse_signature("s?n+").unwrap();
        assert_eq!(specs.len(), 2);
        assert!(specs[0].optional);
        assert!(!specs[0].variadic);
        assert!(specs[1].variadic);
    }

    #[test]
    fn parse_union_type() {
        let specs = parse_signature("(sn)").unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].types, vec![b's', b'n']);
    }

    #[test]
    fn parse_array_content_type() {
        let specs = parse_signature("a<n>").unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].types, vec![b'a']);
        assert_eq!(specs[0].content_type, b'n');
    }

    #[test]
    fn parse_with_return_type() {
        let specs = parse_signature("sn:s").unwrap();
        assert_eq!(specs.len(), 2); // return type stripped
    }

    #[test]
    fn type_checking() {
        assert!(type_matches(&Value::Number(1.0), b'n'));
        assert!(type_matches(&Value::String("hi".into()), b's'));
        assert!(type_matches(&Value::Bool(true), b'b'));
        assert!(type_matches(&Value::Undefined, b'l'));
        assert!(type_matches(&Value::Null, b'l'));
        assert!(type_matches(&Value::Array(Rc::from(vec![])), b'a'));
        assert!(type_matches(
            &Value::Object(Rc::new(Default::default())),
            b'o'
        ));
        assert!(type_matches(&Value::Number(1.0), b'x'));
        assert!(type_matches(&Value::Number(1.0), b'j'));
    }

    #[test]
    fn nil_propagation() {
        let specs = parse_signature("sn").unwrap();
        let (_, return_undef) =
            process_call_args(&specs, &[Value::Undefined, Value::Number(1.0)]).unwrap();
        assert!(return_undef);
    }

    #[test]
    fn singleton_coercion() {
        let specs = parse_signature("a<n>").unwrap();
        let (coerced, _) = process_call_args(&specs, &[Value::Number(42.0)]).unwrap();
        let coerced = coerced.expect("coercion fired, args must be rebuilt");
        assert_eq!(
            coerced[0],
            Value::Array(Rc::from(vec![Value::Number(42.0)]))
        );
    }

    /// Args that already match pass through without a rebuilt Vec.
    #[test]
    fn matching_args_are_not_cloned() {
        let specs = parse_signature("sn").unwrap();
        let (coerced, return_undef) =
            process_call_args(&specs, &[Value::String("hi".into()), Value::Number(1.0)]).unwrap();
        assert!(coerced.is_none(), "no coercion → originals pass through");
        assert!(!return_undef);
        // An already-array arg for an 'a' param also stays borrowed.
        let specs = parse_signature("a<n>").unwrap();
        let arr = Value::Array(Rc::from(vec![Value::Number(1.0)]));
        let (coerced, _) = process_call_args(&specs, &[arr]).unwrap();
        assert!(coerced.is_none());
    }

    #[test]
    fn too_few_args() {
        let specs = parse_signature("sn").unwrap();
        let err = process_call_args(&specs, &[Value::String("hi".into())]).unwrap_err();
        assert_eq!(err.code, "T0410");
    }

    #[test]
    fn too_many_args() {
        let specs = parse_signature("s").unwrap();
        let err = process_call_args(
            &specs,
            &[Value::String("a".into()), Value::String("b".into())],
        )
        .unwrap_err();
        assert_eq!(err.code, "T0410");
    }
}
