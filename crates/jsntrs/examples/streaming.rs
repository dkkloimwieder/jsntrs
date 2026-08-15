//! StreamEvaluator: process multiple expressions against a stream of JSON events.

use std::sync::Arc;

use jsntrs::Value;
use jsntrs::{CustomFunc, Expression, StreamEvaluator};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Simulated event stream
    let events = vec![
        r#"{"type": "login",    "user": "alice", "ts": 1000}"#,
        r#"{"type": "purchase", "user": "bob",   "ts": 2000, "amount": 49.99}"#,
        r#"{"type": "login",    "user": "carol", "ts": 3000}"#,
        r#"{"type": "purchase", "user": "alice", "ts": 4000, "amount": 129.50}"#,
        r#"{"type": "logout",   "user": "bob",   "ts": 5000}"#,
    ];

    // --- Build evaluator with multiple expressions ---

    let mut se = StreamEvaluator::new(vec![]);
    let user_idx = se.compile("user")?;
    let type_idx = se.compile("type")?;
    let is_purchase = se.compile(r#"type = "purchase""#)?;
    let amount_idx = se.compile("amount")?;

    println!("=== Process events with {} expressions ===\n", se.len());

    for event_json in &events {
        let input = Value::from_json_str(event_json)?;
        let results = se.eval_many(&input, &[user_idx, type_idx, is_purchase, amount_idx])?;

        let user = results[0]
            .as_ref()
            .map_or("?", |v| v.as_str().unwrap_or("?"));
        let typ = results[1]
            .as_ref()
            .map_or("?", |v| v.as_str().unwrap_or("?"));
        // `to_boolean` is fallible (D1001 on an infinity); nothing in this
        // feed carries one, so treat a failure as "not a purchase".
        let is_buy = results[2]
            .as_ref()
            .is_some_and(|v| v.to_boolean().unwrap_or(false));
        let amount = results[3].as_ref().and_then(Value::as_f64);

        print!("  {user:8} {typ:10}");
        if is_buy {
            print!(" ${:.2}", amount.unwrap_or(0.0));
        }
        println!();
    }

    // --- Hot-swap an expression ---

    println!("\n=== Hot-swap: replace 'user' with 'user & \" (\" & type & \")\"' ===\n");
    let new_expr = Expression::compile(r#"user & " (" & type & ")""#)?;
    se.replace(user_idx, new_expr)?;

    let input = Value::from_json_str(events[1])?;
    let results = se.eval_many(&input, &[user_idx])?;
    println!(
        "  Swapped result: {}",
        results[0].as_ref().unwrap().stringify(false)?
    );

    // --- Remove and add ---

    println!("\n=== Dynamic management ===");
    se.remove(amount_idx)?;
    println!("  Removed amount expression (idx {})", amount_idx);

    let new_idx = se.compile(r#"$uppercase(user)"#)?;
    println!("  Added $uppercase(user) at idx {}", new_idx);

    let input = Value::from_json_str(events[0])?;
    let results = se.eval_many(&input, &[new_idx, amount_idx])?;
    println!("  $uppercase(user) = {:?}", results[0]);
    println!("  removed expr    = {:?} (None expected)", results[1]);

    // --- Custom functions in streaming ---

    println!("\n=== Streaming with custom functions ===\n");

    let classify: CustomFunc = Arc::new(|args, _| {
        let amount = args.first().and_then(Value::as_f64).unwrap_or(0.0);
        let label = if amount > 100.0 { "high" } else { "low" };
        Ok(Value::String(label.into()))
    });

    let mut se =
        StreamEvaluator::new(vec![]).with_custom_functions(vec![("classify".into(), classify)]);
    let idx = se.compile(r#"type = "purchase" ? $classify(amount) & " value" : "n/a""#)?;

    for event_json in &events {
        let input = Value::from_json_str(event_json)?;
        let result = se.eval_one(&input, idx)?;
        println!("  {}", result.stringify(false)?);
    }

    // --- Stats ---
    let stats = se.stats();
    println!("\nStream stats: {} expression(s) loaded", stats.expressions);

    Ok(())
}
