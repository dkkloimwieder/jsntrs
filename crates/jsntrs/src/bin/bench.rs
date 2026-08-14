// Benchmark CLI for Rust JSONata.
// Usage: jsntrs-bench -expr 'Account.Name' -data '{"Account":{"Name":"Firefly"}}' [-n 1000]
//        jsntrs-bench -stream -datafile data.json -n 1000   (evaluates 4 expressions per iter)

#[cfg(all(feature = "mimalloc-alloc", not(target_arch = "wasm32")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use jsntrs::Expression;
use jsntrs::Value;

/// Exit status for a bad expression, unreadable input, or a failed evaluation.
const EXIT_FAILURE: i32 = 1;
/// Exit status for a malformed command line (missing or unparseable flag).
const EXIT_USAGE: i32 = 2;

/// Report a fatal error on stderr and exit with `status`.
///
/// Everything the caller can get wrong from the command line funnels through
/// here, so a syntax error in `-expr` prints `compile error: S0201: …` — the
/// spec code and message — instead of panicking with a `Debug`-formatted
/// `JsonataError` (jsntrs-6ug).
fn die(status: i32, msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(status)
}

fn flag_value(args: &[String], i: usize) -> &str {
    args.get(i + 1).map_or_else(
        || die(EXIT_USAGE, &format!("missing value for {} flag", args[i])),
        String::as_str,
    )
}

/// Parse the input document, reporting a malformed payload rather than
/// silently benchmarking against undefined.
fn parse_input(data_str: &str) -> Value {
    Value::from_json_str(data_str)
        .unwrap_or_else(|e| die(EXIT_FAILURE, &format!("input JSON error: {e}")))
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
                let path = flag_value(&args, i);
                data_str = std::fs::read_to_string(path)
                    .unwrap_or_else(|e| die(EXIT_FAILURE, &format!("cannot read {path}: {e}")));
                i += 2;
            }
            "-n" => {
                let raw = flag_value(&args, i);
                n = raw
                    .parse()
                    .unwrap_or_else(|e| die(EXIT_USAGE, &format!("invalid -n {raw:?}: {e}")));
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
        die(
            EXIT_FAILURE,
            "usage: jsntrs-bench -expr EXPR [-data JSON | -datafile FILE] [-n ITERS]",
        );
    }

    let compiled = Expression::compile(expr_str)
        .unwrap_or_else(|e| die(EXIT_FAILURE, &format!("compile error: {e}")));
    let input = parse_input(data_str);

    // Benchmark the public API directly: evaluate_value() runs on a
    // per-eval child of the thread-cached stdlib root, so per-iteration
    // env setup is a handful of allocations, not ~70 registrations. (The
    // pre-cached-root workaround here was a prebuilt new_custom_env.)
    let mut result = Value::Undefined;
    for _ in 0..n {
        result = compiled
            .evaluate_value(&input)
            .unwrap_or_else(|e| die(EXIT_FAILURE, &format!("evaluation error: {e}")));
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
        .map(|e| {
            se.compile(e)
                .unwrap_or_else(|err| die(EXIT_FAILURE, &format!("compile error in {e:?}: {err}")))
        })
        .collect();
    let input = parse_input(data_str);

    let mut results = Vec::new();
    for _ in 0..n {
        results = se
            .eval_many(&input, &indices)
            .unwrap_or_else(|e| die(EXIT_FAILURE, &format!("evaluation error: {e}")));
    }
    println!("{} expressions, {} results", exprs.len(), results.len());
}
