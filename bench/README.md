# Benchmarks

Cross-engine benchmark harness for jsntrs. Every engine besides jsntrs
itself is **optional**: the orchestrators detect what is available on this
machine and skip the rest, loudly. Nothing here is required to build, test,
or use the library.

## Engines

| id | what | acquired via | pin | needs |
|---|---|---|---|---|
| `jsntrs` | this repo, native | `cargo build --release -p jsntrs --features bench-bin` | — | cargo |
| `jsntrs-wasi` | this repo, wasm32-wasip2 | same, `--target wasm32-wasip2` | — | cargo, `rustup target add wasm32-wasip2`, wasmtime |
| `go-gnata` | the Go implementation jsntrs was ported from | `engines/go/go.mod` → Go module proxy | `recolabs/gnata v0.2.3` | go ≥ 1.25.6 (network on first build) |
| `jsonata-js` | the reference JavaScript implementation | `engines/js/package.json` → npm | `jsonata 2.1.0` (lockfile) | node, npm |
| `jsonata-core` | competing Rust implementation (txjmb) | `engines/jsonata-core/Cargo.toml` → crates.io | `=2.2.7` | cargo |
| `jsonata-rs` | competing Rust implementation (Stedi) | `engines/jsonata-rs/Cargo.toml` → crates.io | `=0.3.4` (alpha, unstable API) | cargo |

Bumping any pin is a deliberate act that invalidates comparisons with
earlier runs — note it alongside the numbers.

To benchmark a local Go gnata checkout instead of the pinned release, drop a
(gitignored) `go.work` into `engines/go/` with `use .` and `use
/path/to/gnata`.

## Running

```sh
bench/run_matrix.sh --list-engines          # availability banner only
bench/run.sh                                # tiny preset: 8 expressions, 477-byte fixture
bench/run_matrix.sh --preset full           # 1k/10k/100k grid → results/matrix.csv
bench/run_matrix.sh --preset full --payloads 1k --engines jsntrs,jsonata-js
bench/run_matrix.sh --preset tiny --smoke   # correctness diff, no timing
bench/run_bench.sh --quick                  # catalog runner: ~17 representative rows
bench/run_bench.sh --section string,regex --size 1k --csv results/catalog.csv
bench/mem_profile.sh                        # peak RSS + Go MEMSTATS + DHAT
```

Orchestrator requirements (hard, not per-engine): `hyperfine`, `python3`,
GNU coreutils. `bench/summarize.py results/matrix.csv --format
table|markdown|wide-csv` renders the tidy CSV.

## Protocol

Every engine harness speaks the same CLI:

```
<engine-bench> -expr EXPR (-data JSON | -datafile FILE) [-n ITERS]
```

Compile the expression once, evaluate N times, print the final result as
JSON on stdout. Timing is external and process-level (hyperfine), so it
includes startup and input parse once per *process*, amortized over N inner
iterations.

An engine only produces a number after five gates (see `lib.sh`): tool
presence → build → handshake probe → per-row probe (wall-clock capped by
`--probe-budget`, default 5 s) → hyperfine itself, which runs **without**
`--ignore-failure` deliberately: a crashing runner once registered as a fake
~2 ms result. Anything that may legitimately fail is filtered by the probes;
a hyperfine failure is a real regression. Skipped or failed cells appear in
the tidy CSV as `status=skipped|error` with a reason — never as `0.000000`.

## Known methodology asymmetries

- **Allocator**: `jsntrs-bench` runs with mimalloc (the crate's default
  feature); the other engines ship their stock allocators. Competitors are
  measured as they ship.
- **jsonata-rs parses per iteration**: its public API takes the input
  document as a string and re-parses it on every `evaluate()` call, and its
  arena does not free between calls. Its rows are labelled
  `method=parse_per_iter` in the CSV and footnoted by `summarize.py`; expect
  the per-row probe to drop it at large payloads.
- **jsonata-core** constructs a fresh `Evaluator` per evaluation, matching
  that project's own criterion benchmarks.

## Fixtures

Committed (small): `data.json` (477 B), `data_1k.json`, and
`fixtures/{logs,strings,datetime}/1k.json`. Everything larger is gitignored
and regenerated deterministically:

```sh
python3 bench/generate_fixtures.py               # generate whatever is missing
python3 bench/generate_fixtures.py --check       # verify against fixtures.sha256
```

The orchestrators auto-generate a missing fixture on demand (disable with
`--no-autogen`). `fixtures/account/*.json` are symlinks into the `data_*`
files; a dangling symlink simply makes the size-fallback chain pick a
smaller fixture. There is no committed inventory fixture: point
`JSNTRS_INV_JSON=/path/to/inv.json` at one to enable the `inv.*` catalog
rows.

## History

The pre-extraction cross-engine results (Go vs Rust vs WASI vs JS vs
jsonata-core, 32 rows) live in the original combined repository:
<https://github.com/dkkloimwieder/gnata/blob/main/bench/benchmark_results.csv>
(with baselines under `bench/baselines/`). Those numbers measured the crate
under its old name at pins `jsonata-core =2.2.5`, engine ids
`go/rust/wasi/js/jsonata-core`, and a wide CSV schema; this repo's harness
starts fresh with the tidy schema above.
