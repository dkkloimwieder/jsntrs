# Rust Migration Plan

> **Historical document (2026-04).** This is the plan the port started from,
> preserved for context. Several decisions changed during implementation: the
> `Value` enum is reference-counted (`Rc`/`CompactString`) rather than
> lifetime-free-and-Rc-less, the environment chain is `Rc`+`RefCell` (no
> bumpalo), errors are a hand-rolled `JsonataError` (no thiserror), datetime
> math is hand-rolled (no jiff), and arc-swap/dashmap/parking_lot were never
> needed. For the architecture as built, see the root `CLAUDE.md` and the
> code in `crates/jsntrs`.

This document captures all Rust-specific design decisions, dependency choices, and the implementation roadmap for porting gnata from Go to Rust.

---

## 1. Memory Model: Path B Hybrid

All architectural decisions follow the "Path B Hybrid" model. These rules are strict.

### 1.1 AST: Index-Based Arena

Go's AST uses pointer-based recursive `*Node` trees. In Rust, we use a **flat `Vec<Expr>`-backed arena** with `NodeId(u32)` indices. No `Box<Node>`, `Rc`, or lifetime-annotated references.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

pub enum Expr {
    Literal(serde_json::Value),
    Path { steps: Vec<NodeId> },
    Binary { lhs: NodeId, rhs: NodeId, op: BinaryOp },
    Unary { op: UnaryOp, operand: NodeId },
    FunctionCall { procedure: NodeId, args: Vec<NodeId> },
    Lambda { params: Vec<String>, body: NodeId, signature: Option<Signature> },
    Condition { cond: NodeId, then: NodeId, else_: Option<NodeId> },
    Block { expressions: Vec<NodeId> },
    Bind { variable: NodeId, value: NodeId },
    Sort { expr: NodeId, terms: Vec<SortTerm> },
    Transform { pattern: NodeId, update: NodeId, delete: Option<NodeId> },
    Name(String),
    StringLiteral(String),
    NumberLiteral(f64),
    ValueLiteral(ValueKind),     // true, false, null
    Variable(String),
    Wildcard,
    Descendant,
    Parent,
    Regex { pattern: String, flags: String },
    Partial { procedure: NodeId, args: Vec<NodeId> },
    Placeholder,
    // Metadata attached via separate parallel Vec or inline fields
}

#[derive(Default, Serialize, Deserialize)]
pub struct AstArena {
    nodes: Vec<Expr>,
}

impl AstArena {
    pub fn alloc(&mut self, expr: Expr) -> NodeId {
        let id = self.nodes.len() as u32;
        self.nodes.push(expr);
        NodeId(id)
    }

    pub fn get(&self, id: NodeId) -> &Expr {
        &self.nodes[id.0 as usize]
    }
}
```

**Why:**
1. **O(1) drop in WASM** -- no recursive destructor stack overflow for deep ASTs (500+ levels). Drop the single `Vec`, done.
2. **Serializable ASTs** -- `NodeId` is just `u32` and `Expr` has no pointers, so `#[derive(Serialize, Deserialize)]` would allow compiling on a server and sending pre-compiled ASTs to WASM edge clients. *(Not implemented: the as-built arena types derive only `Debug`/`Clone`, and compile-on-server/execute-on-client was never wired up.)*
3. **Cache-friendly** -- contiguous memory, CPU prefetching works naturally during tree-walking evaluation.
4. **Trivial `ArcSwap` sharing** -- `StreamEvaluator` wraps `ArcSwap<AstArena>`. Worker threads get a read-only ref and pass `u32` indices. Zero locks.

**Parser shift:** Parser takes `&mut AstArena`, returns `NodeId`. Each `parse_*` method allocates nodes bottom-up.

**Evaluator shift:** Evaluator takes `&AstArena`, dispatches via `arena.get(node_id)` + `match`.

### 1.2 Values: Owned Types, No Lifetimes

The `Value` enum uses standard owned types (`String`, `Vec`, `IndexMap`). It explicitly **DOES NOT** use lifetimes (`<'bump>`), ensuring the public API is clean and return values are fully owned.

```rust
pub enum Value {
    Undefined,                              // Go nil
    Null,                                   // Go jsonNullType{}
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(IndexMap<String, Value>),
    Function(/* Rc<dyn Callable> or enum */),
    Sequence(Sequence),                     // internal only
}
```

### 1.3 Sequences: Dedicated Value Variant

Implement a dedicated `Sequence` struct wrapped in `Value::Sequence` to handle `KeepSingleton` and `ConsArray` flags, distinct from standard `Value::Array`. This is an internal-only variant -- never exposed to users.

```rust
pub struct Sequence {
    pub values: Vec<Value>,
    pub keep_singleton: bool,
    pub cons_array: bool,
    pub outer_wrapper: bool,
    pub tuple_stream: bool,
}
```

**As built:** one flag only (`keep_singleton`), boxed as `Value::Sequence(Box<Sequence>)` to keep `Value` small. `ConsArray` was dead state even in Go (never set; nesting is decided syntactically at the array-constructor node) and was dropped. Go's `OuterWrapper`/`TupleStream` were not ported -- tuple-stream handling moved into path evaluation.

### 1.4 State: bumpalo for Environment Chain Only

Use `bumpalo::Bump` internally per-evaluation **strictly** for the `Environment` chain and intermediate state to ensure fast O(1) cleanup. NOT for AST, NOT for return values.

```rust
pub struct Evaluator<'a> {
    arena: &'a AstArena,           // Immutable AST, shared across threads
    env_alloc: bumpalo::Bump,      // Per-eval: environments + intermediate state only
}
```

- **Environment scoping**: Allocate `Environment` structs in the bump allocator. Parent links are raw pointers within the same bump arena (safe: entire arena freed atomically when eval completes).
- **Call counter**: `Cell<u32>` in the bump arena, shared by all child environments in a single evaluation.
- **Return values**: Always fully owned `Value` (no bump references leak out). The public API surface is lifetime-free.
- **Thread-safe sharing**: `AstArena` is `Send + Sync` (immutable after parse). Each `eval()` creates its own bump allocator on the calling thread.

### 1.5 Concurrency

- `ArcSwap` for lock-free COW snapshots of the AST and execution plans
- `DashMap` for regex caches -- **must clone the `Arc<Regex>` before execution to drop the shard lock**
- `parking_lot::Mutex` for write serialization

### 1.6 Error Handling

`Result<Value, JsonataError>` everywhere. No panics in evaluation. Use `thiserror` for JSONata spec error codes.

### 1.7 Execution: TCO via Trampoline

TCO must be implemented via a **Trampoline loop** returning a `TailCall` state, preventing stack overflows on deep recursion.

---

## 2. Dependency Manifest

```toml
[dependencies]
# Concurrency
arc-swap = "1.9"          # Lock-free atomic pointer swaps (replaces Go atomic.Pointer for COW snapshots)
dashmap = "6.1"           # Concurrent sharded HashMap (replaces Go sync.Map for regex/expr caches)
parking_lot = "0.12"      # Faster Mutex/RwLock (replaces Go sync.Mutex, no poisoning)

# Parsing & Regex
regex = "1.12"            # RE2-semantics regex engine (1:1 match with Go regexp)

# JSON & Serialization
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1.0", features = ["arbitrary_precision", "preserve_order"] }
indexmap = { version = "2", features = ["serde"] }  # Insertion-ordered maps (replaces Go OrderedMap)

# Error handling
thiserror = "2.0"         # Derive macro for structured error types (JSONata error codes)

# Numeric formatting
ryu-js = "1.0"            # ECMAScript Number.toString() algorithm (replaces Go FormatFloat)

# Date/Time
jiff = "0.2"              # Modern timezone-aware datetime (replaces Go time package)

# Encoding
base64 = "0.22"           # Base64 encode/decode
percent-encoding = "2.3"  # URL encoding (replaces Go net/url)

# Unicode
unicode-segmentation = "1.13"  # Grapheme cluster awareness

# Memory
bumpalo = "3.17"          # Bump allocator for per-eval temporary allocations (envs, sequences)

# RNG
fastrand = "2"            # Fast non-crypto RNG for $random() and $shuffle()

[target.'cfg(target_arch = "wasm32")'.dependencies]
wasm-bindgen = "0.2"      # WASM JS interop (replaces Go syscall/js)
```

Total: **15 direct dependencies** (Go has 1 external + stdlib). All actively maintained.

**As built** -- the real manifest is `crates/jsntrs/Cargo.toml`. Seven crates above were never used: `arc-swap`, `dashmap`, `parking_lot` (no shared mutable state -- `Expression` is immutable and shared via `Arc`), `thiserror` (hand-rolled error struct), `jiff` (hand-rolled calendar math), `bumpalo` (`Rc`+`RefCell` environments), and `unicode-segmentation`. `regex` became optional and feature-gated. Crates adopted during implementation:

| Crate | Purpose |
|---|---|
| `simd-json` | Input JSON parsing straight into `Value` |
| `compact_str` | Inline small strings for keys and string values |
| `foldhash` | Hasher for `ObjectMap` |
| `regex-lite` | Alternate regex backend for small WASM builds |
| `stacker` (non-WASM) | Segmented stack growth at recursion entry points |
| `mimalloc` (optional, default on native) | Global allocator |
| `dhat` (optional) | Heap profiling for the `jsntrs-dhat` bin |
| `js-sys` (WASM) | JS interop alongside `wasm-bindgen` |

---

## 3. Design Decisions with Rationale

### 3.1 Concurrency: `arc-swap` (not a redesign)

The Go `StreamEvaluator` uses `atomic.Pointer` for lock-free COW reads of the expression list and `BoundedCache` for schema-keyed plan snapshots. **`arc-swap::ArcSwap<T>`** is the direct Rust equivalent -- lock-free reads, serialized writes. Same architecture, different primitives:

| Go Pattern | Rust Equivalent |
|---|---|
| `atomic.Pointer[T]` for COW snapshots | `ArcSwap<T>` -- lock-free `load()` returns `Guard<Arc<T>>` |
| `sync.Map` for regex/expr caches | `DashMap<K, V>` -- sharded concurrent map, all ops take `&self` |
| `sync.Mutex` for write serialization | `parking_lot::Mutex` -- faster, no poisoning |
| `atomic.Uint32` for handle counter | `std::sync::atomic::AtomicU32` -- identical |

No architectural redesign needed. The lock-free read path translates directly.

### 3.2 Regex: `regex` crate (direct match)

Rust's `regex` crate uses finite automata (like RE2), guaranteeing linear-time matching with no backtracking. This is semantically identical to Go's `regexp` package. Supports:
- Named captures: `(?P<name>...)`
- Flags: `(?i)`, `(?m)`, `(?s)`, `(?x)`
- Does NOT support backreferences or lookahead (same limitation as Go)

The regex compilation cache (`sync.Map` in Go) becomes `DashMap<String, Arc<Regex>>`. Must clone the `Arc` before execution to drop the shard lock.

**As built:** there is no regex cache -- `stdlib::regex::compile_regex` compiles a fresh `Regex` on every call, and `dashmap` is not a dependency. Caching remains an open optimization.

**As built:** two interchangeable backends sit behind Cargo features -- `regex` (default, full Unicode) and `regex-lite` (~700 KB smaller WASM: 1.3 MB → 579 KB). At least one must be enabled; if both are, `regex` wins. `scripts/build-wasm.sh` selects `regex-lite` for the shipped WASM build.

### 3.3 Parser: Hand-written Pratt (direct translation)

The existing Go parser is a hand-written Pratt parser. **Translate it directly to Rust** -- Pratt parsers map cleanly to Rust's `match` expressions. Parser combinator libraries (`nom`, `winnow`, `pest`) would require rethinking the architecture and introduce unnecessary divergence from the Go reference.

### 3.4 Fast-Path: Defer to Phase 2

Go uses `tidwall/gjson` for zero-copy JSON field extraction. There is no mature Rust equivalent of `gjson.GetManyBytes` (single-scan multi-path extraction). Options considered:

1. `serde_json::RawValue` for deferred parsing -- partial zero-copy, but no multi-path single-scan
2. `gjson` Rust crate -- exists but less mature than the Go original
3. Custom implementation using `simd-json` or hand-rolled byte scanning
4. **Defer fast-path to Phase 2** -- get full AST evaluation correct first, then optimize

**Decision: Option 4.** Build the full evaluator first, validate against all 1,349 tests, then add fast-path as an optimization pass. The fast-path is a performance feature, not a correctness feature.

### 3.5 Go Stdlib Gaps in Rust

| Go Feature | Rust Solution | Notes |
|---|---|---|
| `context.Context` | `Arc<AtomicBool>` cancellation flag | Checked at expression boundaries; no async needed |
| `json.Decoder.UseNumber()` | `serde_json` `arbitrary_precision` feature | Preserves numeric precision as strings |
| `encoding/json` ordered decode | `serde_json` `preserve_order` feature | Uses `IndexMap` internally |
| `strconv.FormatFloat` (JS compat) | **`ryu-js`** crate | Implements ECMAScript `Number.toString()` exactly |
| `math.Round` (round-half-away) | Rust `f64::round()` (direct match) | `f64::round()` is ties-away-from-zero like Go `math.Round`; used for `$formatNumber` mantissas |
| `$round` (round-half-to-even) | Custom `bankers_round` helper | JSONata `$round` is half-to-even, so neither `f64::round()` nor `math.Round` fits -- hand-rolled in `src/stdlib/numeric.rs` |
| `time` package | hand-rolled calendar math in `src/stdlib/datetime/` | `jiff` was dropped for WASM size; no external datetime crate |
| `net/url` encoding | **`percent-encoding`** crate | Configurable encode sets |
| `encoding/base64` | **`base64`** crate | Standard |
| `math/rand` | **`fastrand`** crate | For `$random()` and `$shuffle()` |

### 3.6 WASM

Go WASM binaries are 2-3 MB minimum (runtime + GC). Rust WASM binaries are **50-200 KB** optimized -- a 10-50x reduction.

**As built:** the Rust WASM artifact lands at ~580 KB (`opt-level = "z"`) to ~840 KB (`opt-level = 3`, the shipped build) against ~5.4 MB for the Go build -- roughly 6-9x smaller, not 10-50x. The extra size buys runtime speed: opt-level 3 evaluates ~30% faster than `"z"`.

**Toolchain:** `wasm-bindgen` + `wasm-pack` + `wasm-opt`

**Key `Cargo.toml` profile settings (as built):**
```toml
[profile.release]
opt-level = 3
lto = true
codegen-units = 1
strip = true
```

WASM size is handled by `scripts/build-wasm.sh` -- `regex-lite` plus a manual `wasm-opt -O3 --enable-bulk-memory` pass -- not by an `opt-level = "z"` release profile. The release profile shown here is the native one; the script overrides `CARGO_PROFILE_RELEASE_OPT_LEVEL` itself. `panic = "abort"` is not set.

The existing `npm/` package structure and TypeScript API can be preserved with much smaller bundle size.

**Why the arena architecture matters for WASM:**
- Go WASM must include GC runtime; Rust has no GC
- `Box<Node>` trees cause recursive `drop()` which can stack-overflow in WASM
- `AstArena` drops as a single `Vec` -- O(1), no recursion
- Serializable ASTs enable compile-on-server, execute-on-WASM-client

---

## 4. Implementation Sequence

Each phase should be validated against the conformance test suite. Run tests after each phase to track progress toward the 1,349 target.

### Phase 1: Foundation
- Rust crate initialization (`Cargo.toml`, module structure)
- `Value` enum with all variants
- `Sequence` struct with 4 flags and collapse rules
- `JsonataError` with all error codes via `thiserror`
- JSON decoder using `serde_json` with `arbitrary_precision` + `preserve_order`
- `NormalizeValue` equivalent
- `DeepEqual` implementation
- Number formatting via `ryu-js`

### Phase 2: Lexer
- `Token` enum (54 token types)
- `Lexer` struct with byte-by-byte scanning
- Context-sensitive `/` via mode parameter
- String escapes including UTF-16 surrogate pairs
- Number, regex, backtick, and comment lexing
- All S0xxx error codes

### Phase 3: Parser
- `AstArena` + `NodeId` types
- `Expr` enum with all 22+ node type variants
- Pratt parser: `expression(bp)`, NUD handlers, LED handlers
- Complete binding power table
- Lambda parsing with signature strings

### Phase 4: AST Post-Processing
- `process_ast()`: path flattening, KeepSingletonArray propagation
- Tail-call marking (`mark_tail_calls`)
- Fast-path analysis (deferred -- just the data structures, not evaluation)

### Phase 5: Core Evaluator + Environment
- `Environment` struct with bumpalo allocation
- Linked-list scoping with parent pointers
- Shared call counter (`Cell<u32>` in bump arena)
- `eval()` dispatch by `Expr` variant
- Context cancellation via `Arc<AtomicBool>`

### Phase 6: Path Evaluation
- Simple mode: sequential step evaluation, auto-mapping
- `eval_name` with array auto-mapping and flattening
- Wildcard and descendant operators
- Tuple mode: `(value, env)` pairs for `#$var`, `@$var`, `%`
- Parent operator: environment chain navigation

### Phase 7: Binary & Unary Operators
- Arithmetic with nil propagation and error codes
- String concatenation
- Equality (nil asymmetry) and comparison
- Short-circuit operators (`and`, `or`, `?:`, `??`)
- Range operator with limits
- Subscript: numeric index, array-of-indices, predicate filter
- Unary negation and array constructor (ConsArray flattening)

### Phase 8: Function Machinery
- `call_function` with trampoline TCO loop
- Signature validation with type coercion
- Partial application with `?` placeholders
- HOF callback arity trimming
- Lambda closure capture with `CapturedFocus`

### Phase 9: Standard Library
Start with functions used in most test cases, then expand:
1. String functions: `$string`, `$length`, `$substring`, `$contains`, `$split`, `$join`, `$lowercase`, `$uppercase`, `$trim`, `$replace`, `$match`, `$pad`, `$substringBefore`, `$substringAfter`
2. Numeric functions: `$number`, `$abs`, `$floor`, `$ceil`, `$round`, `$power`, `$sqrt`, `$random`, `$sum`, `$max`, `$min`, `$average`
3. Boolean functions: `$boolean`, `$not`, `$exists`
4. Array functions: `$count`, `$append`, `$sort`, `$reverse`, `$shuffle`, `$distinct`, `$flatten`, `$zip`
5. Object functions: `$keys`, `$values`, `$spread`, `$merge`, `$lookup`, `$sift`, `$each`
6. HOF functions: `$map`, `$filter`, `$reduce`, `$single`
7. Type/misc: `$type`, `$eval`, `$assert`, `$error`
8. Encoding: `$base64encode`, `$base64decode`, `$encodeUrl`, `$decodeUrl`, `$encodeUrlComponent`, `$decodeUrlComponent`
9. DateTime: `$now`, `$millis`, `$fromMillis`, `$toMillis`
10. Formatting: `$formatNumber`, `$formatInteger`, `$formatBase`, `$parseInteger`

### Phase 10: Transform, Sort, Group-By
- Transform: deep clone + pattern match + update/delete
- Sort: stable sort, multi-key, type checking
- Group-by: ordered keys, `$index`/`$key` bindings

### Phase 11: Fast-Path System (optimization)
- Evaluate Rust-native alternatives to GJSON
- Implement pure-path, comparison, and function fast paths
- Benchmark against full-eval baseline

### Phase 12: StreamEvaluator & Concurrency
- `ArcSwap<Vec<Expression>>` for COW expression list
- `BoundedCache` with FIFO ring-buffer and atomic snapshots
- `GroupPlan` construction and caching
- `EvalMany`, `EvalMap`, `EvalOne`
- `MetricsHook` trait

**As built (partially delivered):** `StreamEvaluator` holds a plain `Vec<Option<Expression>>` -- no `ArcSwap`, since `Expression` is already `Send + Sync` and callers share it via `Arc`. `BoundedCache`, `GroupPlan`, and `eval_map` were not ported. `MetricsHook`, `eval_many`, `eval_one`, and their `_with_cancel` variants exist (`src/stream.rs`).

### Phase 13: WASM + npm Package
- `wasm-bindgen` exports matching Go's `_gnataEval`, `_gnataCompile`, `_gnataEvalHandle`, `_gnataReleaseHandle`
- TypeScript wrapper preserving the existing `npm/src/` API
- Binary size optimization

---

## 5. Testing Strategy

### 5.1 Conformance Test Harness

Port `suite_test.go` to Rust. The harness loads JSON test cases from `testdata/groups/` with this format:

```json
{
    "expr": "JSONata expression",
    "data": { ... },           // or "dataset": "dataset5"
    "bindings": {},
    "result": expected_value,  // or "undefinedResult": true, or "code": "T2001"
    "unordered": true          // present in some fixtures; see note below
}
```

**As built:** `unordered` is honoured by `tests/conformance.rs`: flagged cases compare arrays as multisets at every depth (the reference harness's deep-equal-in-any-order). The nine fixtures carrying the flag also pass order-sensitively today, so the flag is robustness, not a pass/fail difference.

### 5.2 Progress Tracking

Track conformance as `X/1733` cases passing. 1,349 is the number of JSON *files*; 19 of them hold arrays of cases, so the suite expands to 1,733 individual cases:
- 1,667 cases from the official jsonata-js test suite (103 directories)
- 66 supplemental cases in `testdata/groups/rust-*/` (9 directories)

`tests/conformance.rs` expands the array files and gates on zero unexpected failures.

### 5.3 Validation Against Go

Any new test cases added during Rust development should also pass in Go:
```sh
go test -race -count=1 ./...
```

---

## 6. Key Source Files (Go Reference)

| Purpose | Path | Lines |
|---|---|---|
| Public API, fast-path dispatch | `gnata.go` | 436 |
| StreamEvaluator | `stream.go` | ~460 |
| Bounded cache | `bounded_cache.go` | ~140 |
| Fast-path functions | `func_fast.go` | ~420 |
| AST node types | `internal/parser/ast.go` | 131 |
| Pratt parser | `internal/parser/parser.go` | ~1000 |
| AST post-processing | `internal/parser/process.go` | ~300 |
| Fast-path analysis | `internal/parser/analysis.go` | ~325 |
| Tail-call marking | `internal/parser/tailcall.go` | ~50 |
| Lexer | `internal/lexer/lexer.go` | ~400 |
| Token types | `internal/lexer/token.go` | ~100 |
| Eval dispatch | `internal/evaluator/evaluator.go` | ~85 |
| Binary operators | `internal/evaluator/eval_binary.go` | ~410 |
| Path evaluation | `internal/evaluator/path.go` | ~1200 |
| Function calls/TCO | `internal/evaluator/eval_function.go` | ~225 |
| Value types/coercion | `internal/evaluator/value.go` | 264 |
| Environment/scoping | `internal/evaluator/env.go` | 194 |
| OrderedMap | `internal/evaluator/ordered_map.go` | ~284 |
| Helpers (stringify, format) | `internal/evaluator/eval_helpers.go` | ~200 |
| Signature validation | `internal/evaluator/signature.go` | ~200 |
| Transform | `internal/evaluator/eval_transform.go` | ~170 |
| Group-by | `internal/evaluator/eval_group.go` | ~107 |
| Sort | `internal/evaluator/eval_sort.go` | ~100 |
| Regex | `internal/evaluator/eval_regex.go` | ~220 |
| Function registry | `functions/register.go` | ~99 |
| String functions | `functions/string_funcs.go` | ~200 |
| HOF functions | `functions/hof_funcs.go` | ~150 |
| Conformance harness | `suite_test.go` | ~240 |
| Test data | `testdata/groups/` (112 dirs) | 1,349 files / 1,733 cases |

---

## 7. Related Documents

| Document | Purpose |
|---|---|
| `CLAUDE.md` | Project guide for Claude Code sessions |
| `docs/spec.md` | Complete behavioral specification (1,966 lines) |
| `docs/migration-hazards.md` | 11 ranked migration hazards with Go/Rust code examples |
| `docs/behaviors.md` | Type coercion truth tables, error code catalog, equality semantics |
