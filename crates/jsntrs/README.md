# jsntrs

A [JSONata](https://jsonata.org) 2.x query and transformation engine for
Rust, ported from [gnata](https://github.com/recolabs/gnata), a Go
implementation of JSONata. It passes all
1,733 cases of the ported JSONata conformance suite, preserves the
reference implementation's semantics (`undefined` vs `null`, sequence
collapsing, insertion-ordered objects, ECMAScript number formatting), and
compiles to a compact WebAssembly module.

## Quick start

```rust
use jsntrs::Expression;

fn main() -> Result<(), jsntrs::JsonataError> {
    let expr = Expression::compile("$sum(Order.Product.(Price * Quantity))")?;
    let result = expr.evaluate(r#"{
        "Order": [
            {"Product": [{"Price": 34.5, "Quantity": 2}]},
            {"Product": [{"Price": 21.5, "Quantity": 1}]}
        ]
    }"#)?;
    println!("{}", result.stringify(false)?); // 90.5
    Ok(())
}
```

A compiled `Expression` is `Send + Sync` and cheap to `Clone` (the AST is
shared), so compile once and evaluate many times — including from multiple
threads. If you already have a parsed [`Value`], use `evaluate_value` to
skip JSON parsing.

## Custom functions

```rust
use std::sync::Arc;
use jsntrs::{CustomFunc, Expression, Value, new_custom_env};

let double: CustomFunc = Arc::new(|args, _focus| {
    let n = args.first().and_then(Value::as_f64).unwrap_or(0.0);
    Ok(Value::Number(n * 2.0))
});

let env = new_custom_env(&[("double".into(), double)]);
let expr = Expression::compile("$double(21)")?;
let result = expr.evaluate_with_env(&Value::Undefined, &env)?;
```

See [`examples/`](examples/) for variables, error handling, streaming
evaluation, and more.

## Cargo features

| Feature | Default | Purpose |
|---|---|---|
| `regex` | yes | Full Unicode regex backend (RE2 semantics, no backtracking) |
| `regex-lite` | no | Lighter regex backend; shrinks WASM builds by ~700 KB (1.3 MB → 579 KB in this repo's build) |
| `mimalloc-alloc` | yes | Links mimalloc and sets it as the global allocator in the bundled `jsntrs-bench` binary; a library cannot set a consumer's allocator |

At least one regex backend must be enabled; if both are, `regex` takes
precedence. To use `regex-lite`, disable default features and re-enable what
you need:

```toml
jsntrs = { version = "0.1", default-features = false, features = ["regex-lite", "mimalloc-alloc"] }
```

## WebAssembly

The crate builds for `wasm32-unknown-unknown` and ships `wasm-bindgen`
bindings (compile, evaluate, format, highlight) in its `wasm` module.
Combined with `regex-lite` and `wasm-opt`, the optimized module is a small
fraction of the size of the equivalent Go/TinyGo build.

## Performance

Compiled natively, this engine outperforms the Go implementation on all 32
benchmarks in the cross-language suite (median 1.7x, up to 2.5x), the
competing Rust implementation
[jsonata-core](https://crates.io/crates/jsonata-core) (v2.2.5) on 31 of 32
(median 2.2x), and the reference JavaScript implementation by roughly an
order of magnitude (median 9x). The WASM build (WASI) typically lands
within ~1.4x of native Go. Those numbers were measured before this crate
was extracted from the combined Go+Rust repository; the full data lives at
<https://github.com/dkkloimwieder/gnata/blob/main/bench/benchmark_results.csv>.
To reproduce against the current tree (optionally pulling in jsonata-js,
Go gnata, jsonata-core, and jsonata-rs), see `bench/README.md` in this
repository.

## Semantics guarantees

- `undefined` and `null` are distinct: `undefined = undefined` is `false`,
  `null = null` is `true`
- Object key insertion order is preserved through all operations
- Number formatting matches JavaScript's `Number.toString()`
- Tail calls are trampolined — deep recursion cannot overflow the stack
- Regex uses finite-automaton semantics (no catastrophic backtracking)

## Minimum supported Rust version

Rust 1.88 (edition 2024 plus let-chains).

## License

MIT. See [LICENSE](LICENSE).
