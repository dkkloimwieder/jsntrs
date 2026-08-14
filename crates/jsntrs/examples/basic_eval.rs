//! Basic JSONata evaluation: compile expressions and evaluate against JSON.

use jsntrs::Expression;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Path access
    let expr = Expression::compile("Account.Name")?;
    let result = expr.evaluate(r#"{"Account": {"Name": "Firefly"}}"#)?;
    println!("Path access:       {}", result.stringify(false)?);

    // Nested path with array flattening
    let expr = Expression::compile("Account.Order.Product.SKU")?;
    let result = expr.evaluate(
        r#"{"Account": {"Order": [
            {"Product": [{"SKU": "A1"}, {"SKU": "A2"}]},
            {"Product": [{"SKU": "B1"}]}
        ]}}"#,
    )?;
    println!("Nested path:       {}", result.stringify(false)?);

    // Arithmetic
    let expr = Expression::compile("price * quantity")?;
    let result = expr.evaluate(r#"{"price": 9.99, "quantity": 3}"#)?;
    println!("Arithmetic:        {}", result.stringify(false)?);

    // String concatenation
    let expr = Expression::compile(r#"firstName & " " & lastName"#)?;
    let result = expr.evaluate(r#"{"firstName": "Ada", "lastName": "Lovelace"}"#)?;
    println!("Concatenation:     {}", result.stringify(false)?);

    // Conditional
    let expr = Expression::compile(r#"age >= 18 ? "adult" : "minor""#)?;
    let result = expr.evaluate(r#"{"age": 25}"#)?;
    println!("Conditional:       {}", result.stringify(false)?);

    // Literal expression (no input data needed)
    let expr = Expression::compile("2 + 3 * 4")?;
    let result = expr.evaluate("")?;
    println!("Literal math:      {}", result.stringify(false)?);

    Ok(())
}
