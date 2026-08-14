//! String functions and regex pattern matching.

use jsntrs::Expression;

fn eval(label: &str, expr_str: &str) -> Result<(), Box<dyn std::error::Error>> {
    let data = r#"{"text": "  Hello, World!  ", "email": "user@example.com", "csv": "a,b,c,d"}"#;
    let expr = Expression::compile(expr_str)?;
    let result = expr.evaluate(data)?;
    println!("{label:30} => {}", result.stringify(false)?);
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Case Conversion ===");
    eval("$uppercase", "$uppercase(text)")?;
    eval("$lowercase", "$lowercase(text)")?;

    println!("\n=== Trimming & Padding ===");
    eval("$trim", "$trim(text)")?;
    eval("$pad (right)", r#"$pad("hi", 10)"#)?;
    eval("$pad (left)", r#"$pad("hi", -10)"#)?;
    eval("$pad (custom char)", r#"$pad("42", -6, "0")"#)?;

    println!("\n=== Substring ===");
    eval("$substring(0, 5)", r#"$substring("Hello, World!", 0, 5)"#)?;
    eval("$substringBefore", r#"$substringBefore(email, "@")"#)?;
    eval("$substringAfter", r#"$substringAfter(email, "@")"#)?;

    println!("\n=== Split & Join ===");
    eval("$split", r#"$split(csv, ",")"#)?;
    eval("$join", r#"$join($split(csv, ","), " | ")"#)?;
    eval("$contains", r#"$contains(email, "example")"#)?;

    println!("\n=== Regex ===");
    eval(
        "$match",
        r#"$match("2024-03-15", /(\d{4})-(\d{2})-(\d{2})/)"#,
    )?;
    eval(
        "$replace (literal)",
        r#"$replace("Hello World", "World", "JSONata")"#,
    )?;
    eval(
        "$replace (regex)",
        r#"$replace("foo bar baz", /[aeiou]/, "*")"#,
    )?;

    println!("\n=== Encoding ===");
    eval("$base64encode", r#"$base64encode("Hello, World!")"#)?;
    eval("$base64decode", r#"$base64decode("SGVsbG8sIFdvcmxkIQ==")"#)?;
    eval(
        "$encodeUrlComponent",
        r#"$encodeUrlComponent("hello world&foo=bar")"#,
    )?;

    println!("\n=== Chained Transforms ===");
    eval(
        "Pipeline",
        r#"email ~> $substringBefore("@") ~> $uppercase"#,
    )?;
    eval("Split & count", r#"$count($split(csv, ","))"#)?;

    Ok(())
}
