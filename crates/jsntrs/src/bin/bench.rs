// Benchmark CLI for Rust JSONata.
// Usage: jsntrs-bench -expr 'Account.Name' -data '{"Account":{"Name":"Firefly"}}' [-n 1000]
//        jsntrs-bench -stream -datafile data.json -n 1000   (evaluates 4 expressions per iter)

#[cfg(all(feature = "mimalloc-alloc", not(target_arch = "wasm32")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use jsntrs::Expression;
use jsntrs::Value;

fn flag_value(args: &[String], i: usize) -> &str {
    args.get(i + 1).map_or_else(
        || {
            eprintln!("missing value for {} flag", args[i]);
            std::process::exit(2);
        },
        String::as_str,
    )
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut expr_str = "";
    let mut data_str = String::from("{}");
    let mut n: u64 = 1;
    let mut stream_mode = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-expr" => {
                expr_str = flag_value(&args, i);
                i += 2;
            }
            "-data" => {
                data_str = flag_value(&args, i).to_string();
                i += 2;
            }
            "-datafile" => {
                data_str = std::fs::read_to_string(flag_value(&args, i)).expect("read file failed");
                i += 2;
            }
            "-n" => {
                n = flag_value(&args, i).parse().expect("invalid -n");
                i += 2;
            }
            "-stream" => {
                stream_mode = true;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    if stream_mode {
        run_stream_bench(&data_str, n);
    } else {
        run_single_bench(expr_str, &data_str, n);
    }
}

fn run_single_bench(expr_str: &str, data_str: &str, n: u64) {
    if expr_str.is_empty() {
        eprintln!("usage: jsntrs-bench -expr EXPR [-data JSON | -datafile FILE] [-n ITERS]");
        std::process::exit(1);
    }

    let compiled = Expression::compile(expr_str).expect("compile failed");
    let input = Value::from_json_str(data_str).unwrap_or(Value::Undefined);

    // Benchmark the public API directly: evaluate_value() runs on a
    // per-eval child of the thread-cached stdlib root, so per-iteration
    // env setup is a handful of allocations, not ~70 registrations. (The
    // pre-cached-root workaround here was a prebuilt new_custom_env.)
    let mut result = Value::Undefined;
    for _ in 0..n {
        result = compiled.evaluate_value(&input).expect("eval failed");
    }

    println!("{}", result.to_json_string());
}

fn run_stream_bench(data_str: &str, n: u64) {
    use jsntrs::StreamEvaluator;

    let exprs = [
        "Account.Name",
        "Account.Order.Product.SKU",
        "Account.Order.Product[UnitPrice > 50].SKU",
        "$sum(Account.Order.Product.(UnitPrice * Quantity * (1 - Discount)))",
    ];

    let mut se = StreamEvaluator::new(Vec::new());
    let indices: Vec<usize> = exprs
        .iter()
        .map(|e| se.compile(e).expect("compile failed"))
        .collect();
    let input = Value::from_json_str(data_str).unwrap_or(Value::Undefined);

    let mut results = Vec::new();
    for _ in 0..n {
        results = se.eval_many(&input, &indices).expect("eval failed");
    }
    println!("{} expressions, {} results", exprs.len(), results.len());
}
