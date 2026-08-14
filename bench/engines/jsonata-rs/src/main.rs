// Benchmark CLI for jsonata-rs (https://github.com/Stedi/jsonata-rs).
// Mirrors jsntrs-bench's interface so bench/run_matrix.sh can drive both:
//   jsonata-rs-bench -expr 'Account.Name' -datafile data.json -n 1000
//
// METHODOLOGY ASYMMETRIES (also documented in bench/README.md):
//   * jsonata-rs has no public pre-parsed-input path: evaluate() takes the
//     input document as &str and re-parses it on every call, so its timings
//     include per-iteration JSON parsing that the other engines pay once.
//     The harness registry labels this engine `parse_per_iter`.
//   * The Bump arena never frees during a run, so memory grows with -n and
//     payload size. Large payloads are expected to be probe-gated by the
//     orchestrator's --probe-budget rather than run at full iteration count.

use bumpalo::Bump;
use jsonata_rs::JsonAta;

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
        eprintln!("usage: jsonata-rs-bench -expr EXPR [-data JSON | -datafile FILE] [-n ITERS]");
        std::process::exit(1);
    }

    let arena = Bump::new();
    let jsonata = JsonAta::new(expr_str, &arena).expect("parse failed");

    let mut out = String::new();
    for _ in 0..n {
        let result = jsonata
            .evaluate(Some(&data_str), None)
            .expect("eval failed");
        if !result.is_undefined() {
            out = result.serialize(false);
        }
    }

    if !out.is_empty() {
        println!("{out}");
    }
}
