//! Real-world data transformation: reshape, filter, aggregate, sort, group.

use jsntrs::Expression;

const DATA: &str = r#"{
    "orders": [
        {"id": 1, "customer": "Alice", "items": [
            {"product": "Widget", "price": 25.00, "qty": 4, "category": "hardware"},
            {"product": "Gadget", "price": 99.99, "qty": 1, "category": "electronics"}
        ]},
        {"id": 2, "customer": "Bob", "items": [
            {"product": "Widget", "price": 25.00, "qty": 2, "category": "hardware"},
            {"product": "Doohickey", "price": 15.50, "qty": 10, "category": "hardware"}
        ]},
        {"id": 3, "customer": "Alice", "items": [
            {"product": "Thingamajig", "price": 199.99, "qty": 1, "category": "electronics"}
        ]}
    ]
}"#;

fn eval(label: &str, expr_str: &str) -> Result<(), Box<dyn std::error::Error>> {
    let expr = Expression::compile(expr_str)?;
    let result = expr.evaluate(DATA)?;
    println!("{label}:");
    println!("  {}", result.stringify(true)?);
    println!();
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Extract all product names (auto-flattened across nested arrays)
    eval("All products", "orders.items.product")?;

    // Filter: expensive items only
    eval("Expensive items (>50)", "orders.items[price > 50].product")?;

    // Map: compute line totals
    eval(
        "Line totals",
        "orders.items.(product & \": $\" & price * qty)",
    )?;

    // Aggregate: total revenue
    eval("Total revenue", "$sum(orders.items.(price * qty))")?;

    // Count items per order
    eval(
        "Items per order",
        "orders.{\"customer\": customer, \"itemCount\": $count(items)}",
    )?;

    // Sort items by price descending
    eval(
        "Sorted by price (desc)",
        "$sort(orders.items, function($a,$b){$a.price < $b.price}).product",
    )?;

    // Group by category
    eval("Group by category", "orders.items{category: product}")?;

    // Reduce: total revenue via $reduce
    eval(
        "Reduce total",
        "$reduce(orders.items, function($prev, $item){$prev + $item.price * $item.qty}, 0)",
    )?;

    // Transform: restructure data
    eval(
        "Order summaries",
        "orders.{\"id\": id, \"customer\": customer, \"total\": $sum(items.(price * qty)), \"products\": $count(items)}",
    )?;

    // Distinct values
    eval("Unique customers", "$sort(orders.customer) ~> $distinct")?;

    Ok(())
}
