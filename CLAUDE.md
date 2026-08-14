# CLAUDE.md

Guidance for Claude Code sessions in this repo.

## What this is

**jsntrs** is a JSONata 2.x query and transformation engine in Rust
(`crates/jsntrs`), building native and WASM. It began as a port of
**gnata**, a Go implementation of JSONata; the Go code is *not* in this
repo — it lives in the original combined repository
(github.com/dkkloimwieder/gnata), which also retains the full development
history. `docs/spec.md` is the behavioral contract distilled from it. The
port is feature-complete: all 1,808 conformance cases pass, and
`tests/conformance.rs` asserts zero failures.

## Commands (run at the workspace root)

```sh
cargo test --workspace                             # all tests; conformance gate is strict
cargo clippy --workspace --release --all-targets   # budget: ≤121 warnings, 0 errors — do not add any
cargo fmt --all --check                            # must stay clean
cargo bench -p jsntrs                              # criterion; skips missing bench/ fixtures
cargo check -p jsntrs --target wasm32-unknown-unknown --no-default-features --features regex-lite
./scripts/build-wasm.sh                            # optimized WASM build → pkg/
python3 -m http.server 8000                        # then open /playground.html
cd crates/jsntrs && cargo fuzz build               # nightly; fuzz/ is its own workspace
```

## Repo layout

- Root `Cargo.toml` is a virtual workspace (`crates/jsntrs`); **`[profile.*]`
  lives there** — cargo ignores profile sections in member manifests.
- `clippy.toml` / `rustfmt.toml` sit at the root (walk-up resolution covers
  the crate and fuzz).
- `testdata/` and `bench/` sit at the repo root: `tests/conformance.rs`
  reaches `../../testdata` from the crate, and `benches/{eval,parse}.rs`
  reach `../../bench` (missing fixtures skip with a notice; regenerate with
  `python3 bench/generate_fixtures.py`).
- `crates/jsntrs/fuzz/` is its own workspace (cargo-fuzz convention, own
  `Cargo.lock`). `bench/engines/*` are standalone packages excluded from the
  workspace (independent pins for competing engines).

## Architecture (as built)

- **AST**: index-based arena (`AstArena` = `Vec<Expr>` + `NodeId(u32)`), built
  by a hand-written Pratt parser; `process_ast` flattens `.` chains into paths
  and marks tail calls. `Expression` wraps the arena in an `Arc`: `Send + Sync`,
  cheap to clone — compile once, evaluate from any thread.
- **`Value`**: compact 32-byte enum (`assert!(size <= 32)` in `value.rs`); the
  size is load-bearing (Rc-wrapping experiments that grew it regressed
  benchmarks 12–40%). Strings are `CompactString` (inline
  ≤24 bytes; beat `Rc<str>` on cache locality in A/B tests), arrays/objects are
  `Rc<[Value]>` / `Rc<ObjectMap>` with copy-on-write via `Rc::make_mut`,
  functions are `Box<FunctionValue>`. Deliberately `!Send`: share the
  `Expression`, build input `Value`s per thread.
- **`Sequence`**: internal-only `Value` variant carrying the
  `keep_singleton` flag. `eval()` collapses it at the API
  boundary; it must never reach users.
- **`Environment`**: `Rc` parent chain with `RefCell` bindings and a small
  cache for non-local lookups; carries the shared call counter and an
  `Arc<AtomicBool>` cancellation flag.
- **Errors**: hand-rolled `JsonataError { code, token, value, message }` with
  JSONata spec codes; `Result` everywhere. Panics only for documented caller
  bugs (e.g. foreign `NodeId` in `AstArena::get`).
- **Recursion**: tail calls run through a trampoline over a `TailCall` value
  (max iterations = max call depth × 10,000); other deep recursion is covered
  by `stacker::maybe_grow` on native targets (wasm cannot grow its stack).
- **Fast paths**: `fast_path.rs` (simple path/tape evaluation) and
  `stdlib/hof_fast.rs` (lambda recognition for HOFs) bypass general dispatch.
  They must be semantics-preserving: `tests/differential.rs` and the fuzz
  targets compare them against the general path. When a pattern is ambiguous,
  don't lift it.
- **JSON**: simd-json parses input; a hand-rolled `write_json` emits compact
  output; serde_json (`arbitrary_precision` + `preserve_order`) is used only
  for pretty-printing and `serde_json::Value` interop; `ryu-js` gives exact
  ECMAScript `Number.toString()`. indexmap keeps object key order.
- **Datetime / encodings**: hand-rolled calendar math (no jiff/chrono);
  base64 and percent-encoding crates for the encoding builtins.
- **Regex**: `regex` (default) or `regex-lite` (small WASM builds) — at least
  one must be enabled (neither is a `compile_error!`); if both are, `regex`
  wins. Both are finite-automaton, no backtracking.
- **Public API**: the curated re-exports in `lib.rs`; all modules are
  `pub(crate)` except `wasm`, which stays public on wasm32 as the
  wasm-bindgen surface. The `#[doc(hidden)]` re-exports carry no stability
  guarantee and come in two tiers: type-reachability items (`FunctionValue`
  family, `Sequence`, AST types) are always present; the test/bench/fuzz
  hooks (`Parser`, `Lexer`, `register_all`, `eval`, …) additionally require
  the `internals` feature, enabled only by the self-referential
  dev-dependency, the fuzz crate, and `dhat-heap`.

## Behavioral invariants (DO NOT VIOLATE)

1. `undefined = undefined` → `false`; `null = null` → `true`
2. Sequence collapse: 0 items → undefined, 1 → unwrapped (unless
   keep-singleton), >1 → array
3. Field access on arrays auto-maps and flattens
4. Object key insertion order is preserved through all operations
5. Number output matches JS `Number.toString()`; `$round` is half-to-even
6. Boolean coercion: `"0"` truthy, `""` falsy, `"false"` truthy
7. Sort is stable; nils sort after non-nils
8. `$eval()` shares the call counter with its parent evaluation

## Conventions

- Lint suppressions use `#[expect(...)]`, with a `reason` when non-obvious;
  tests may unwrap/panic (see `clippy.toml`). Keep `cargo fmt --check` clean.
- Gate performance changes with a same-session A/B (saved criterion baselines
  drift); gate correctness changes on the conformance suite.

## Issue tracking

Current work uses `bd` (beads) with the `jsntrs-` prefix. `gnata-XXX.N` IDs
in doc comments are historical citations of closed issues in the original
combined repo's tracker — do not rename them.

## References

- `docs/spec.md` — authoritative behavioral spec (derived from the Go code)
- `docs/behaviors.md` — truth tables, error codes, equality rules
- `docs/migration-hazards.md` — Go→Rust pitfalls encountered in the port
- `docs/rust-migration-plan.md` — **historical**: the original plan; several
  decisions changed during implementation. Where it disagrees with the code
  or this file, the code wins.
- `docs/*.md` cite the Go reference implementation by filename (`gnata.go`,
  `internal/…`) and sometimes by absolute path; those files live in the
  original gnata repo, not here.
- Test suite: `testdata/groups/` (112 groups) + `testdata/datasets/`
- Benchmarks: `bench/README.md` (engines, methodology, fixture regeneration)
- JSONata language: https://jsonata.org


<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:970c3bf2 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   bd dolt push
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->
