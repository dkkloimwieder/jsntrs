// Benchmark CLI for jsonata-core (https://github.com/txjmb/jsonata-core).
// Mirrors gnata-bench's interface so bench/run_full.sh can drive both:
//   jsonata-core-bench -expr 'Account.Name' -datafile data.json -n 1000
//
// Methodology matches jsonata-core's own criterion benchmarks
// (benches/evaluator_bench.rs): parse the expression once, then a fresh
// Evaluator::new() per evaluation.

use jsonata_core::evaluator::Evaluator;
use jsonata_core::parser;
use jsonata_core::value::JValue;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut expr_str = "";
    let mut data_str = String::from("{}");
    let mut n: u64 = 1;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-expr" => {
                expr_str = &args[i + 1];
                i += 2;
            }
            "-data" => {
                data_str = args[i + 1].clone();
                i += 2;
            }
            "-datafile" => {
                data_str = std::fs::read_to_string(&args[i + 1]).expect("read file failed");
                i += 2;
            }
            "-n" => {
                n = args[i + 1].parse().expect("invalid -n");
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    if expr_str.is_empty() {
        eprintln!("usage: jsonata-core-bench -expr EXPR [-data JSON | -datafile FILE] [-n ITERS]");
        std::process::exit(1);
    }

    let ast = parser::parse(expr_str).expect("parse failed");
    let input = JValue::from_json_str(&data_str).expect("invalid JSON input");

    let mut result = JValue::Undefined;
    for _ in 0..n {
        result = Evaluator::new().evaluate(&ast, &input).expect("eval failed");
    }

    if result.is_undefined() {
        return;
    }
    println!("{}", result.to_json_string().expect("serialize failed"));
}
