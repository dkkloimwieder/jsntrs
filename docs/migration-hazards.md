# Rust Migration Hazards

This document catalogs the non-obvious behaviors in gnata's Go implementation that are most likely to cause subtle bugs during a Rust port. Each hazard includes the Go pattern, why it's dangerous, and the recommended Rust approach.

---

## Hazard 1: Null vs Undefined Distinction

### Go Pattern
```go
// internal/evaluator/value.go:11-24
var Null any = jsonNullType{}  // JSON null sentinel
// Go nil = JSONata "undefined" (no value)
```

### Why It's Dangerous
Go uses `nil` for both "no value" and "uninitialized interface." JSONata requires a strict distinction:
- `undefined = undefined` returns `false` (not `true`)
- `null = null` returns `true`
- `undefined` propagates silently through most operators (returns `undefined`)
- `null` does NOT propagate -- it participates in operations

The equality asymmetry in `eval_binary.go:159-168`:
```go
case "=":
    if left == nil || right == nil {
        return false, nil  // undefined = anything -> false
    }
    return DeepEqual(left, right), nil
```

### Rust Approach
```rust
enum Value {
    Undefined,  // JSONata undefined (Go nil)
    Null,       // JSON null (Go jsonNullType{})
    // ...
}
```
Both must be distinct enum variants. Pattern match explicitly -- never conflate them.

### Test: `undefined = undefined` must return `false`, `null = null` must return `true`.

---

## Hazard 2: Dual Number Representation

### Go Pattern
```go
// internal/evaluator/value.go:105-116
func ToFloat64(v any) (float64, bool) {
    switch n := v.(type) {
    case float64:
        return n, true
    case json.Number:  // string-backed, preserves "12345678901234567"
        f, err := n.Float64()
        return f, err == nil
    }
    return 0, false
}
```

Numbers from JSON input arrive as `json.Number` (string-backed) via `json.Decoder.UseNumber()`. Numbers from computation are `float64`. The `normalizeNumber` function (`value.go:179-188`) bridges for equality.

### Why It's Dangerous
- `FormatFloat` (`eval_helpers.go:72-86`) must match JavaScript's `Number.toString()` exactly
- Decimal notation for `|n| in [5e-7, 1e21)`, scientific notation outside
- `FormatNumber` (`eval_helpers.go:57-67`) only converts to float64 when string contains `e/E`; plain integers/decimals returned verbatim to preserve precision beyond 2^53
- `NaN` and `Inf` format as `"null"` (not `"NaN"` or `"Infinity"`)

### Rust Approach
**As built, the port keeps a single `Value::Number(f64)`** -- Go's `json.Number` string preservation was NOT ported. `Value::from_json` and the simd-json visitors collapse every input number to `f64`, so precision beyond 2^53 is lost on input. `arbitrary_precision` remains enabled on `serde_json` for the interop path but does not preserve digits inside `Value`.

What *is* preserved is output formatting: `ryu-js` for exact `Number.toString()`, with the two-layer split described in Hazard 11.

### Test: `$string(12345678901234567)` round-trips at f64 precision -- the digit-preservation guarantee the Go engine gives does not hold here.

---

## Hazard 3: Dual Map Representation

### Go Pattern
```go
// internal/evaluator/ordered_map.go
type OrderedMap struct {
    keys []string
    data map[string]any
}

// Bridge functions (ordered_map.go:232-283)
func MapGet(v any, key string) (any, bool)  // handles both *OrderedMap and map[string]any
func MapKeys(v any) []string
func IsMap(v any) bool
```

Input JSON decoded via `DecodeJSON` produces `*OrderedMap`. Some Go code paths produce `map[string]any`. Every map operation must go through bridge functions.

### Why It's Dangerous
If you miss a code path that creates a plain map, key ordering breaks silently. Object equality (`DeepEqual`) must handle both types cross-compared.

### Rust Approach
Use one ordered map type uniformly. **Eliminates the duality entirely** -- this is a Rust advantage.

As built: `ObjectMap = IndexMap<CompactString, Value, foldhash::fast::RandomState>`, held behind `Rc` as `Value::Object(Rc<ObjectMap>)` (`src/value.rs`). Input is parsed by simd-json directly into `Value` rather than through `serde_json`'s `preserve_order` path, so ordering is preserved by construction.

---

## Hazard 4: Sequence Collapse + ConsArray

### Go Pattern
```go
// internal/evaluator/value.go:29-35
type Sequence struct {
    Values        []any
    KeepSingleton bool  // [] suffix -> don't unwrap single elements
    ConsArray     bool  // [...] constructor -> don't flatten when nested
    OuterWrapper  bool  // top-level JSON array input
    TupleStream   bool  // tuple objects for index tracking
}

// Collapse rules (value.go:54-66)
func CollapseSequence(s *Sequence) any {
    switch len(s.Values) {
    case 0: return nil           // undefined
    case 1:
        if s.KeepSingleton { return []any{s.Values[0]} }
        return s.Values[0]      // unwrap
    default: return slices.Clone(s.Values)  // array
    }
}
```

### Why It's Dangerous
The interaction between `KeepSingleton` and `ConsArray` in `evalUnary` (`eval_unary.go:30-60`) is subtle:
- Explicit arrays `[x]` must not flatten when nested inside another `[...]` (the `ConsArray` field exists for this but is never set, even in Go -- the syntactic check below does the work)
- Implicit arrays (from path evaluation) are spread into parent sequences
- The decision is AST-structural (is the sub-expression a `NodeUnary "["`) not runtime

```go
// eval_unary.go:40-55 (simplified)
for _, expr := range node.Expressions {
    val := Eval(expr, input, env)
    if expr.Type == parser.NodeUnary && expr.Value == "[" {
        // Explicit sub-array: keep nested
        seq.Values = append(seq.Values, val)
    } else {
        // Implicit: flatten into parent
        appendToSequence(seq, val)
    }
}
```

### Rust Approach
`Value::Sequence(Box<Sequence>)` as a dedicated variant. As built the struct carries **one** flag (`keep_singleton`); `ConsArray` was never set even in Go, and Go's `OuterWrapper`/`TupleStream` are not flags in Rust -- tuple mode is selected from AST shape by `path_has_tuple_step` (`src/evaluator/path.rs`). Array constructor evaluation must check the AST node type (via `NodeId` lookup in arena) to decide flatten vs nest.

### Test: `[[1,2], [3,4]]` must produce `[[1,2],[3,4]]` (nested), but path expressions returning arrays must flatten.

---

## Hazard 5: Parent Operator (%)

### Go Pattern
The `%` operator navigates the **environment chain**, not the data structure. Implementation spread across:
- `path.go:119-149`: Binding parent context as `%%` key in child environments
- `path.go:198-275`: Tuple-aware parent tracking
- `env.go:86-94`: `LookupWithEnv` returns both value and the environment where the binding was found

### Why It's Dangerous
- Chained `%.%` works by: (1) find `%%` binding in current env, (2) get the environment where it was bound, (3) look up `%%` in THAT environment's parent
- Join steps (`@` operator) add a `parentJoinFlag` marker to environments -- these must be skipped during `%` navigation
- Tuple mode maintains per-element `(value, env)` pairs where each element's env has its parent bound under `%%`

### Rust Approach
As built, environments are reference-counted rather than bump-allocated: `Environment { parent: Option<Rc<Environment>>, bindings: RefCell<HashMap<CompactString, Value>>, .. }`, with children built by `Environment::new_child(parent: Rc<Environment>)` (`src/evaluator/environment.rs`). The `%%` binding (`PARENT_BINDING`, `src/evaluator/mod.rs`) is set on child environments during path evaluation, and the lookup returns both the value and the environment that held it.

### Test: `Account.Order.Product.(%.%.`Account Name`)` must navigate up two levels to the Account object.

---

## Hazard 6: Context-Sensitive Lexer

### Go Pattern
```go
// parser.go:60
type Parser struct {
    lex   *lexer.Lexer
    token lexer.Token
    infix bool  // tracks whether next token is in infix position
}
```

The `/` character is a regex literal in prefix position and division in infix position. The parser sets `infix` after consuming each token, and the lexer reads it on the next `advance()` call.

### Why It's Dangerous
- `a / b / c` -- three tokens: name, division, name, division, name
- `/pattern/` -- one regex literal token (only valid in prefix position)
- `$f(/pattern/)` -- regex as function argument (prefix position after `(`)
- Getting this wrong produces silent mis-parses (valid but wrong AST)

### Rust Approach
Pass a `LexerMode` enum (or boolean) to `next_token()`. The parser sets the mode after each token consumption. Alternative: the lexer holds a mutable reference to a shared mode flag.

---

## Hazard 7: Panic Recovery -> Result<T, E>

### Go Pattern
```go
// gnata.go:156-167
func evalCore(expr *Expression, ctx context.Context, input any, env *evaluator.Environment) (result any, err error) {
    defer func() {
        if r := recover(); r != nil {
            switch v := r.(type) {
            case *evaluator.JSONataError:
                err = v
            case error:
                err = v
            default:
                err = fmt.Errorf("panic: %v", r)
            }
        }
    }()
    // ... evaluation code that may panic
}
```

### Why It's Dangerous
Go uses panics for early exit in deep evaluation paths (e.g., stack overflow U1001). The `defer/recover` at the top catches everything. In Rust, there is no equivalent -- every function must explicitly propagate errors via `Result<T, E>`.

### Rust Approach
`Result<Value, JsonataError>` return type on every evaluator function. Use `?` operator for propagation. No `panic!()` in evaluation code.

As built, `JsonataError` is a hand-rolled **struct** carrying `code`/`token`/`value`/`message` (`src/error.rs`), with `Display` and `std::error::Error` implemented manually -- `thiserror` was not needed, since every error shares one shape.

---

## Hazard 8: Zero-Param Closure Focus Capture

### Go Pattern
```go
// internal/evaluator/env.go:185-193
type Lambda struct {
    Params        []string
    Body          *parser.Node
    Closure       *Environment
    Thunk         bool
    Sig           string
    CapturedFocus any  // focus ($) at definition time for zero-param closures
}

// eval_function.go:202-205 (during call)
if len(lambda.Params) == 0 && lambda.CapturedFocus != nil {
    input = lambda.CapturedFocus  // use definition-time focus, not call-site focus
}
```

### Why It's Dangerous
When a zero-parameter lambda is defined, it captures the current focus value (`$`). When called later, the body evaluates with the **definition-time** focus, not the call-site focus. This is a non-obvious semantic that affects closures used as callbacks in HOFs.

### Rust Approach
The `Lambda` representation in the Rust `Value` enum must include an `Option<Value>` for captured focus. During lambda creation (evalLambda), if params is empty, store the current input as captured focus.

---

## Hazard 9: Array Constructor Flattening (AST-Structural)

### Go Pattern
In `eval_unary.go`, the array constructor `[...]` decides whether to flatten sub-expressions based on **AST node type**, not runtime value type:

```go
// eval_unary.go (simplified)
if expr.Type == parser.NodeUnary && expr.Value == "[" {
    // This sub-expression IS an explicit array constructor
    // -> keep nested (don't flatten)
    seq.Values = append(seq.Values, val)
} else {
    // This is any other expression that happens to return an array
    // -> flatten into parent sequence
    appendToSequence(seq, val)
}
```

### Why It's Dangerous
Two expressions can return the same runtime value (`[1, 2]`) but be treated differently:
- `[[1,2], [3,4]]` -- inner `[1,2]` is `NodeUnary "["` -> kept nested -> result: `[[1,2],[3,4]]`
- `[foo, bar]` where foo/bar evaluate to arrays -> flattened -> result: `[1,2,3,4]`

This distinction exists only in the AST structure. In Rust with the index-based arena, the evaluator must check `arena.get(child_id)` to determine if the child is an explicit array constructor.

### Rust Approach
During array constructor evaluation, for each child `NodeId`, check `arena.get(child_id)` -- if it's `Expr::Unary { op: UnaryOp::ArrayCons, .. }`, keep nested; otherwise, flatten via `append_to_sequence`.

---

## Hazard 10: $eval Depth Sharing

### Go Pattern
```go
// internal/evaluator/env.go:14-20
type callCounter struct {
    depth     int
    evalDepth int  // $eval nesting counter
    max       int
}

// env.go:105-119
func (e *Environment) IncrEvalDepth(maxDepth int) error {
    c := e.callCounter()
    c.evalDepth++
    if c.evalDepth > maxDepth {
        c.evalDepth--
        return &JSONataError{Code: "D3121", Message: "$eval: maximum nesting depth exceeded"}
    }
    return nil
}
```

The `callCounter` is a **shared pointer** across all child environments in an evaluation. When `$eval()` creates a nested compile-and-evaluate cycle, it shares the same call counter with the parent evaluation.

### Why It's Dangerous
In Rust, the call counter lives in the bumpalo arena. But `$eval()` creates a NEW parser + evaluator for the nested expression. The nested evaluator must share the same depth counter. With bumpalo raw pointers, this means passing a `*mut u32` (or `&Cell<u32>`) from the parent evaluator into the nested one.

### Rust Approach
The call counter is an `Rc<CallCounter>` holding `Cell<u32>` depth and eval-depth counters, cloned into each child environment (`src/evaluator/environment.rs`) -- no bump arena and no raw pointers. Since all evaluations in a single `eval()` call stay on one thread, `Rc`+`Cell` suffices. The `$eval` function receives the counter when creating the nested evaluator, which preserves the shared-D3121 behavior this hazard warns about.

---

## Hazard 11: Two Number-Formatting Layers

### Go Pattern
```go
// internal/evaluator/eval_helpers.go:72 — $string() casting only
func FormatFloat(n float64) string {
    // strconv.FormatFloat(n, 'g', 15, 64) — 15 significant digits
}

// bench/go_bench.go:68 — JSON output of results
out, _ := json.Marshal(result)  // shortest round-trip, ES6-style
```

### Why It's Dangerous
The Go reference formats numbers differently at two layers: `$string()` and
string coercion use `FormatFloat` ('g', 15 — an approximation of JS
`Number.toString()` that truncates to 15 significant digits), while JSON
serialization of results uses `encoding/json` (shortest round-trip, matching
`JSON.stringify`). The two agree for almost all doubles, so a port that wires
the `$string` formatter into JSON output passes the conformance suite and
still silently corrupts any value whose shortest form needs 16–17 digits:
`25.1 * 3 * (1 - 0.1)` = `67.77000000000001` serializes as `67.77`, which
re-parses one ULP off. Found only by byte-diffing benchmark output against
the reference engines (gnata-1jc).

### Rust Approach
Keep the layers separate: `format_float` ('g' 15) for `$string()`/casting per
`docs/spec.md`, and ryu-js (`Buffer::format_finite`, exact ECMAScript
`Number.toString()`) in `Value::to_json`/`write_json`. The regression test
`json_number_output_round_trips` pins both behaviors.

---

## Summary: Migration Risk Ranking

| # | Hazard | Risk | Complexity |
|---|--------|------|------------|
| 5 | Parent operator (%) | Critical | Very High |
| 4 | Sequence collapse + ConsArray | Critical | High |
| 1 | Null vs Undefined | High | Medium |
| 11 | Two number-formatting layers | High | Low (two call sites) |
| 10 | $eval depth sharing | High | Medium |
| 8 | Zero-param focus capture | High | Low |
| 9 | Array constructor flattening | Medium | Medium |
| 2 | Dual number representation | Medium | Medium |
| 6 | Context-sensitive lexer | Medium | Low |
| 7 | Panic recovery -> Result | Medium | Low (pervasive) |
| 3 | Dual map representation | Low | Low (eliminated in Rust) |
