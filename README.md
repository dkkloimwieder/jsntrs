# jsntrs

A [JSONata](https://jsonata.org) 2.x query and transformation engine in
Rust, compiling native and to WebAssembly. All 2,775 cases of the ported
JSONata conformance suite pass, and the gate is strict: `cargo test` fails
on a single regression.

The engine crate lives in [`crates/jsntrs`](crates/jsntrs/) — see its
[README](crates/jsntrs/README.md) for the library API, cargo features, and
semantics guarantees.

## Quick start

```rust
use jsntrs::Expression;

let expr = Expression::compile("$sum(Order.Product.(Price * Quantity))")?;
let result = expr.evaluate(r#"{"Order": [{"Product": [{"Price": 34.5, "Quantity": 2}]}]}"#)?;
```

## Repository layout

| Path | What |
|---|---|
| `crates/jsntrs/` | The engine crate (library, wasm bindings, criterion benches, fuzz targets) |
| `testdata/` | Conformance suite: 162 groups, 2,775 cases (seeded from jsonata-js, since extended and in places deliberately divergent — see `testdata/NOTICE` and `docs/spec.md`) |
| `docs/` | Behavioral spec and reference docs distilled from the Go reference implementation |
| `bench/` | Optional cross-engine benchmark harness (jsonata-js, Go gnata, jsonata-core, jsonata-rs) — see `bench/README.md` |
| `scripts/build-wasm.sh` | Optimized WASM build (wasm-pack + wasm-opt → `pkg/`); wasm-opt is required |
| `playground.html` | Browser playground running the WASM build |

## WASM + playground

```sh
./scripts/build-wasm.sh          # → pkg/jsntrs_bg.wasm (~830 KB), pkg/jsntrs.js
python3 -m http.server 8000      # then open http://localhost:8000/playground.html
```

The ~830 KB is the post-`wasm-opt` size, so a missing wasm-opt (binaryen) is
an error rather than a silently larger module. `--allow-unoptimized` builds
one anyway: it warns, says `UNOPTIMIZED` in the final line, and exits 3.

## Development

```sh
cargo test --workspace                             # conformance gate is strict
cargo clippy --workspace --release --all-targets   # budget: ≤121 warnings, 0 errors
cargo fmt --all --check
cargo check -p jsntrs --target wasm32-unknown-unknown --no-default-features --features regex-lite
```

## Provenance

jsntrs began as a Rust port of [gnata](https://github.com/recolabs/gnata),
an independent Go implementation of JSONata. The port was developed in the
combined repository [dkkloimwieder/gnata](https://github.com/dkkloimwieder/gnata),
which retains the full development history and the Go reference
implementation. See [NOTICE](NOTICE).

## License

MIT. See [LICENSE](LICENSE).
