//! Pass Rust values into JSONata expressions as variables.

use jsntrs::Expression;
use jsntrs::Value;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Simple variable binding
    let expr = Expression::compile("$x + $y")?;
    let result = expr.evaluate_with_vars(
        "",
        &[
            ("x".into(), Value::Number(10.0)),
            ("y".into(), Value::Number(32.0)),
        ],
    )?;
    println!("$x + $y = {}", result.stringify(false)?);

    // Mix variables with input data
    let expr = Expression::compile("$multiplier * price")?;
    let result = expr.evaluate_with_vars(
        r#"{"price": 25.0}"#,
        &[("multiplier".into(), Value::Number(1.1))],
    )?;
    println!("With data: {}", result.stringify(false)?);

    // String variables
    let expr = Expression::compile(r#"$greeting & ", " & name & "!""#)?;
    let result = expr.evaluate_with_vars(
        r#"{"name": "World"}"#,
        &[("greeting".into(), Value::String("Hello".into()))],
    )?;
    println!("Strings:   {}", result.stringify(false)?);

    // Boolean variable
    let expr = Expression::compile(r#"$verbose ? details : summary"#)?;
    let result = expr.evaluate_with_vars(
        r#"{"details": "Full report with all fields", "summary": "OK"}"#,
        &[("verbose".into(), Value::Bool(false))],
    )?;
    println!("Boolean:   {}", result.stringify(false)?);

    // Array variable
    let items: Vec<Value> = vec![
        Value::Number(10.0),
        Value::Number(20.0),
        Value::Number(30.0),
    ];
    let expr = Expression::compile("$sum($items)")?;
    let result = expr.evaluate_with_vars(
        "",
        &[("items".into(), Value::Array(std::rc::Rc::from(items)))],
    )?;
    println!("Array sum: {}", result.stringify(false)?);

    Ok(())
}
