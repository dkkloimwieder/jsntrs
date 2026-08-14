//! Benchmark: JSON parsing into jsntrs::Value at different payload sizes.
//!
//! Compares three parsers:
//!   1. simd-json → Value (current default via from_json_str)
//!   2. serde_json → Value (direct Visitor, no intermediate tree)
//!   3. serde_json → serde_json::Value (baseline)
//!
//! Note on 2 and 3: serde_json is built with `arbitrary_precision`, so every
//! number that is not a plain i64/u64 reaches the visitor as a one-entry map
//! carrying the raw number text (see `NUMBER_TOKEN` in value.rs). Both
//! serde_json rows therefore pay a detour on number-heavy fixtures that row 1
//! does not — they measure the same *result*, not the same work.
//!
//! Fixtures live in bench/data*.json.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use jsntrs::Value;

/// Fixture directory, anchored at compile time so the benches work no matter
/// the current directory (workspace root, crate dir, or a direct binary run).
const BENCH_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../bench");

const FIXTURES: &[(&str, &str)] = &[
    ("tiny_4", "data.json"),
    ("1k", "data_1k.json"),
    ("10k", "data_10k.json"),
    ("10k_long", "data_10k_long.json"),
    ("10k_mixed", "data_10k_mixed.json"),
    ("100k", "data_100k.json"),
    ("100k_long", "data_100k_long.json"),
    ("100k_mixed", "data_100k_mixed.json"),
];

/// Read a fixture, or skip it with a notice: the large fixtures are
/// gitignored and regenerated with `python3 bench/generate_fixtures.py`.
fn load(file: &str) -> Option<String> {
    let path = format!("{BENCH_DIR}/{file}");
    match std::fs::read_to_string(&path) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("bench: skipping fixture {file}: {e}");
            None
        }
    }
}

fn bench_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse");

    for &(name, path) in FIXTURES {
        let Some(json_str) = load(path) else {
            continue;
        };
        let bytes = json_str.len() as u64;

        group.throughput(Throughput::Bytes(bytes));

        // 1. simd-json → jsntrs::Value (current default)
        group.bench_with_input(
            BenchmarkId::new("simd_to_value", format!("{name}_{bytes}B")),
            &json_str,
            |b, data| {
                b.iter(|| {
                    let v = Value::from_json_str(data).unwrap();
                    criterion::black_box(v);
                });
            },
        );

        // 2. serde_json → jsntrs::Value (direct Visitor, no intermediate tree)
        group.bench_with_input(
            BenchmarkId::new("serde_to_value", format!("{name}_{bytes}B")),
            &json_str,
            |b, data| {
                b.iter(|| {
                    let v: Value = serde_json::from_str(data).unwrap();
                    criterion::black_box(v);
                });
            },
        );

        // 3. serde_json → serde_json::Value (baseline)
        group.bench_with_input(
            BenchmarkId::new("serde_to_serde", format!("{name}_{bytes}B")),
            &json_str,
            |b, data| {
                b.iter(|| {
                    let v: serde_json::Value = serde_json::from_str(data).unwrap();
                    criterion::black_box(v);
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_parse);
criterion_main!(benches);
