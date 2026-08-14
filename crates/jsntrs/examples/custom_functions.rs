//! Register Rust functions callable from JSONata expressions.

use std::sync::Arc;

use jsntrs::{CustomFunc, Expression, Value, new_custom_env};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // --- One-shot custom function ---

    let double: CustomFunc = Arc::new(|args, _focus| {
        let n = args.first().and_then(Value::as_f64).unwrap_or(0.0);
        Ok(Value::Number(n * 2.0))
    });

    let expr = Expression::compile("$double(21)")?;
    let result = expr.evaluate_with_custom_funcs("", &[("double".into(), double.clone())])?;
    println!("$double(21) = {}", result.stringify(false)?);

    // --- Multiple custom functions ---

    let greet: CustomFunc = Arc::new(|args, _focus| {
        let name = args.first().and_then(Value::as_str).unwrap_or("stranger");
        Ok(Value::String(format!("Hello, {name}!").into()))
    });

    let fahrenheit: CustomFunc = Arc::new(|args, _focus| {
        let celsius = args.first().and_then(Value::as_f64).unwrap_or(0.0);
        Ok(Value::Number(celsius * 9.0 / 5.0 + 32.0))
    });

    let expr = Expression::compile(r#"$greet("Alice")"#)?;
    let result = expr.evaluate_with_custom_funcs("", &[("greet".into(), greet.clone())])?;
    println!("{}", result.stringify(false)?);

    let expr = Expression::compile("$fahrenheit(100)")?;
    let result =
        expr.evaluate_with_custom_funcs("", &[("fahrenheit".into(), fahrenheit.clone())])?;
    println!("100C = {}F", result.stringify(false)?);

    // --- Reusable environment (shared across evaluations) ---

    let env = new_custom_env(&[
        ("double".into(), double),
        ("greet".into(), greet),
        ("fahrenheit".into(), fahrenheit),
    ]);

    // All three functions available in every evaluation
    let expr1 =
        Expression::compile(r#"$greet("Bob") & " It's " & $fahrenheit(37) & "F outside.""#)?;
    let result = expr1.evaluate_with_env(&Value::Undefined, &env)?;
    println!("{}", result.stringify(false)?);

    let expr2 = Expression::compile("$double($fahrenheit(0))")?;
    let result = expr2.evaluate_with_env(&Value::Undefined, &env)?;
    println!("$double($fahrenheit(0)) = {}", result.stringify(false)?);

    // --- Custom function accessing input data ---

    let tax: CustomFunc = Arc::new(|args, _focus| {
        let amount = args.first().and_then(Value::as_f64).unwrap_or(0.0);
        let rate = args.get(1).and_then(Value::as_f64).unwrap_or(0.1);
        Ok(Value::Number(amount * (1.0 + rate)))
    });

    let env = new_custom_env(&[("tax".into(), tax)]);

    let expr = Expression::compile("$tax(price, 0.08)")?;
    let input = Value::from_json_str(r#"{"price": 100.0}"#)?;
    let result = expr.evaluate_with_env(&input, &env)?;
    println!("$tax(100, 0.08) = {}", result.stringify(false)?);

    Ok(())
}
