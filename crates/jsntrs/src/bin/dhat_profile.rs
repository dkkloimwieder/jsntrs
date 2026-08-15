//! DHAT allocation profiler for jsntrs.
//!
//! Usage:
//!   cargo run --features dhat-heap --bin jsntrs-dhat -- &lt;mode&gt; \[options\]
//!
//! Modes:
//!   parse  -datafile FILE          Profile JSON parsing
//!   eval   -expr EXPR -datafile FILE [-n ITERS]  Profile evaluation
//!
//! Produces dhat-heap.json in the working directory.
//! View at <https://nnethercote.github.io/dh_view/dh_view.html>

// No `#[cfg(feature = "dhat-heap")]` guards below: the `[[bin]]` entry in
// Cargo.toml declares `required-features = ["dhat-heap"]`, so cargo skips
// this target entirely when the feature is off and the guards could never
// choose the other arm. The one that existed printed "rebuild with
// --features dhat-heap" from a binary that only exists with the feature on.
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() {
    {
        use jsntrs::Environment;
        use jsntrs::Expression;
        use jsntrs::Value;
        use std::rc::Rc;

        let _profiler = dhat::Profiler::new_heap();

        let args: Vec<String> = std::env::args().collect();
        if args.len() < 2 {
            eprintln!("usage: jsntrs-dhat <parse|eval> [options]");
            eprintln!("  parse -datafile FILE");
            eprintln!("  eval  -expr EXPR -datafile FILE [-n ITERS]");
            std::process::exit(1);
        }

        let find_arg = |args: &[String], flag: &str| -> Option<String> {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1).cloned())
        };

        match args[1].as_str() {
            "parse" => {
                let datafile = find_arg(&args[2..], "-datafile").unwrap_or_else(|| {
                    eprintln!("-datafile required");
                    std::process::exit(1);
                });
                let data = std::fs::read_to_string(&datafile).unwrap_or_else(|e| {
                    eprintln!("read {datafile}: {e}");
                    std::process::exit(1);
                });
                eprintln!("parsing {} bytes...", data.len());
                let value = Value::from_json_str(&data).unwrap();
                eprintln!(
                    "done. type: {}",
                    if value.is_object() { "object" } else { "other" }
                );
            }
            "eval" => {
                let expr_str = find_arg(&args[2..], "-expr").unwrap_or_else(|| {
                    eprintln!("-expr required");
                    std::process::exit(1);
                });
                let datafile = find_arg(&args[2..], "-datafile").unwrap_or_else(|| {
                    eprintln!("-datafile required");
                    std::process::exit(1);
                });
                let n: u64 = find_arg(&args[2..], "-n")
                    .map(|s| s.parse().unwrap_or(1))
                    .unwrap_or(1);
                let data = std::fs::read_to_string(&datafile).unwrap_or_else(|e| {
                    eprintln!("read {datafile}: {e}");
                    std::process::exit(1);
                });
                let input = Value::from_json_str(&data).unwrap_or(Value::Undefined);
                let expr = Expression::compile(&expr_str).unwrap_or_else(|e| {
                    eprintln!("compile: {e}");
                    std::process::exit(1);
                });
                let mut env = Environment::new();
                jsntrs::register_all(&mut env);
                if !input.is_undefined() {
                    env.bind("$", input.clone());
                }
                let env = Rc::new(env);
                eprintln!("evaluating '{expr_str}' x {n} on {} bytes...", data.len());
                let mut result = Value::Undefined;
                for _ in 0..n {
                    result = expr.evaluate_with_env(&input, &env).unwrap();
                }
                eprintln!(
                    "result type: {}",
                    if result.is_array() { "array" } else { "scalar" }
                );
            }
            other => {
                eprintln!("unknown mode: {other}");
                std::process::exit(1);
            }
        }
    }
}
