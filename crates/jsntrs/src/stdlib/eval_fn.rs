//! $eval function: dynamically evaluate JSONata expression strings.

use std::rc::Rc;

use crate::error::{JsonataError, JsonataResult};
use crate::evaluator::Environment;
use crate::parser::{AstArena, Parser, process_ast};
use crate::value::Value;

const MAX_EVAL_DEPTH: u32 = 5;

pub fn fn_eval(
    args: &[Value],
    focus: &Value,
    env: &Rc<Environment>,
    _arena: &AstArena,
) -> JsonataResult {
    if args.is_empty() {
        return Err(JsonataError::new(
            "D3006",
            "$eval: requires at least 1 argument",
        ));
    }
    if args[0].is_undefined() {
        return Ok(Value::Undefined);
    }
    let expr: &str = match &args[0] {
        Value::String(s) => s,
        _ => {
            return Err(JsonataError::new(
                "T0410",
                "$eval: argument must be a string",
            ));
        }
    };

    env.incr_eval_depth(MAX_EVAL_DEPTH)?;

    let result = (|| -> JsonataResult {
        let (mut arena, root) = Parser::parse(expr)
            .map_err(|e| JsonataError::new("D3120", format!("$eval: invalid expression: {e}")))?;
        let root = process_ast(&mut arena, root)
            .map_err(|e| JsonataError::new("D3120", format!("$eval: invalid expression: {e}")))?;

        let ctx = if args.len() >= 2 && !args[1].is_undefined() {
            &args[1]
        } else {
            focus
        };

        // Only a child scope — the parent chain already provides the stdlib,
        // and re-registering here would shadow user overrides (Go creates a
        // bare child environment too).
        let child_env = Rc::new(Environment::new_child(Rc::clone(env)));
        crate::evaluator::eval(&arena, root, ctx, &child_env).map_err(|e| {
            JsonataError::new(
                "D3121",
                format!("unable to evaluate expression: {}", e.message),
            )
        })
    })();

    env.decr_eval_depth();
    result
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::expression::{CustomFunc, Expression};
    use crate::value::Value;

    // Go ground truth (2026-08-07): $eval("$string()") on input 5 => "5",
    // identical to top-level $string(). The old stdlib re-registration bound
    // a stricter "x-" signature locally and broke context substitution.
    #[test]
    fn eval_sees_same_stdlib_as_top_level() {
        let top = Expression::compile("$string()").unwrap();
        let nested = Expression::compile(r#"$eval("$string()")"#).unwrap();
        assert_eq!(top.evaluate("5").unwrap(), Value::String("5".into()));
        assert_eq!(nested.evaluate("5").unwrap(), Value::String("5".into()));
    }

    #[test]
    fn eval_sees_user_override_of_stdlib_name() {
        let shout: CustomFunc =
            Arc::new(|_args: &[Value], _focus: &Value| Ok(Value::String("overridden".into())));
        let expr = Expression::compile(r#"$eval("$uppercase('a')")"#).unwrap();
        let result = expr
            .evaluate_with_custom_funcs("{}", &[("uppercase".into(), shout)])
            .unwrap();
        assert_eq!(result, Value::String("overridden".into()));
    }
}
