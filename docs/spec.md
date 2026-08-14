# Gnata Behavioral Specification

This is the authoritative behavioral reference for the gnata JSONata 2.x engine, written to guide the Rust port. Every behavior is documented with exact Go source file paths and line numbers.

**Source repository:** github.com/recolabs/gnata
**Language:** Go 1.25.6
**Test suite:** 1,733 cases across 1,349 JSON files in `testdata/groups/` (112 groups); the Rust harness gates on >=1,700 passing (`crates/jsntrs/tests/conformance.rs`)

---

# Section 1: Value Model

## 1.1 Type Taxonomy

The gnata evaluator represents all JSONata values as Go `any` (empty interface). The complete set of runtime value types is:

| JSONata Type | Go Representation | Notes |
|---|---|---|
| Undefined | `nil` | Go nil; represents absence of a value |
| Null | `jsonNullType{}` (singleton `Null`) | Distinct from undefined (`nil`); see Section 1.2 |
| Boolean | `bool` | `true` or `false` |
| Number | `float64` or `json.Number` | Dual representation in Go; the Rust port uses a single f64 -- see Section 1.4 |
| String | `string` | Go string (UTF-8) |
| Array | `[]any` | Plain Go slice; collapsed from `*Sequence` |
| Object | `*OrderedMap` or `map[string]any` | Input data may be `map[string]any`; expressions produce `*OrderedMap`; see Section 1.9 |
| Sequence | `*Sequence` | Internal multi-value container; never exposed to user code after collapse |
| Function (builtin) | `BuiltinFunction`, `EnvAwareBuiltin`, `SignedBuiltin` | See Section 1.11 |
| Function (lambda) | `*Lambda` | See Section 1.12 |

**Source:** `/home/kaalin/dev/gnata/internal/evaluator/value.go` lines 1-263, `/home/kaalin/dev/gnata/internal/evaluator/env.go` lines 166-193.

## 1.2 Null vs Undefined Distinction

JSONata distinguishes between "null" (an explicit JSON null value) and "undefined" (absence of any value). In gnata:

- **Undefined** is represented by Go `nil`.
- **Null** is represented by the singleton `Null` of type `jsonNullType{}` (an unexported empty struct).

**Source:** `/home/kaalin/dev/gnata/internal/evaluator/value.go` lines 11-24.

The sentinel type implements `json.Marshaler`, producing the bytes `"null"` (the constant `parser.NullJSON`).

**`IsNull(v any) bool`** (line 21): Type-asserts `v` to `jsonNullType` to test for JSON null. Returns `false` for Go `nil`.

**Equality semantics:**
- `nil == nil` → undefined equals undefined at the Go level, BUT `DeepEqual(nil, nil)` returns `true`
- In JSONata evaluation: `undefined = undefined` returns `false` (handled in `eval_binary.go:159`: if either operand is nil, return false)
- `Null == Null` → true via `DeepEqual`
- `nil == Null` → **false** (they are distinct)

## 1.3 Sequence

`Sequence` is the core multi-value container used during evaluation. It is **never** returned as a final result -- it is always collapsed before being exposed.

**Source:** `/home/kaalin/dev/gnata/internal/evaluator/value.go` lines 29-103.

**Structure** (line 29):
```go
type Sequence struct {
    Values        []any
    KeepSingleton bool   // do NOT unwrap single-element sequences
    ConsArray     bool   // explicitly constructed via [...]; prevents flattening
    OuterWrapper  bool   // input was a JSON array; treated as a single document
    TupleStream   bool   // contains tuple objects {"@": value, varName: value}
}
```

**Flags:**
1. **`KeepSingleton`** (line 31): When true, a single-element sequence collapses to `[]any{elem}` instead of `elem`. Set by the `[]` suffix operator on path steps.
2. **`ConsArray`** (line 32): Marks the sequence as an explicitly constructed array (`[...]`). Prevents automatic flattening during `appendToSequence`.
3. **`OuterWrapper`** (line 33): Indicates the input document was a JSON array. The entire array is treated as a single document value, not as multiple inputs.
4. **`TupleStream`** (line 34): Indicates the sequence contains tuple objects with `"@"` keys, used internally for focus variable binding in path steps.

**`CreateSequence(items ...any) *Sequence`** (line 38): Allocates a sequence with initial capacity of `len(items)+4`, pre-populates with provided items.

**`CollapseSequence(s *Sequence) any`** (line 54): Applies singleton-collapsing rules:
- `len(Values) == 0` → `nil` (undefined)
- `len(Values) == 1` AND `KeepSingleton == false` → `Values[0]` (unwrap)
- `len(Values) == 1` AND `KeepSingleton == true` → `[]any{Values[0]}` (wrap in array)
- `len(Values) > 1` → `slices.Clone(Values)` (return as `[]any`)

**`CollapseAndKeep(result any, keepArray bool) any`** (line 72): Normalizes a function call result with optional `keepArray` semantics:
1. If `result` is a `*Sequence`: if `keepArray` is true, sets `KeepSingleton = true` on a copy before collapsing. Calls `CollapseSequence`.
2. If `keepArray` is true (after collapse): `[]any` → returned as-is; `nil` → returned as `nil`; any other type → wrapped in `[]any{result}`.
3. Otherwise, returned as-is.

**`appendToSequence(seq *Sequence, v any)`** (`eval_helpers.go` lines 13-25):
- `nil` → skip (do nothing)
- `*Sequence` → recursively append each element (flattens nested sequences)
- Anything else → append directly to `seq.Values`

## 1.4 Number Handling: Single f64 Representation

**Rust port (shipped behavior).** Numbers are `Value::Number(f64)` only -- there is no second numeric variant (`crates/jsntrs/src/value.rs:82`). JSON input is converted to f64 at parse time (`Value::from_json`, `value.rs:464-467`, via `n.as_f64()`), so integers beyond 2^53 are **not** preserved verbatim; digits past that limit are lost on ingest. JSON output re-renders through `ryu-js` (`value.rs:521-528`) and `&`/`$string` coercion goes through `format_float` (`value.rs:399-401`). The Go `json.Number` verbatim-precision path was deliberately not ported, and `FormatNumber` has no Rust counterpart.

**Go reference implementation.** Numbers have two Go representations:

1. **`float64`**: Used for computed values and literal numbers in AST.
2. **`json.Number`**: Used for numbers decoded from JSON input via `DecodeJSON` (which calls `dec.UseNumber()`). Preserves the original string form for precision beyond float64's 2^53 limit.

**Source:** `value.go` lines 106-143, `eval_helpers.go` lines 57-102.

**`ToFloat64(v any) (float64, bool)`** (value.go line 106): Converts either representation to float64.

**`IsNumeric(v any) bool`** (value.go line 118): Returns true for finite numeric values. Rejects `Inf` and `NaN`.

**`CheckNumeric(v any) error`** (value.go line 130): Returns `D1001` error for `Inf` or `NaN`.

**`normalizeNumber(v any) any`** (value.go line 179): Converts `json.Number` to `float64` for comparison purposes.

### FormatFloat and FormatNumber (Go reference)

Must match JavaScript's `Number.toString()` behavior.

**`FormatFloat(n float64) string`** (eval_helpers.go line 72):
1. `NaN` or `Inf` → `"null"`
2. Format with `strconv.FormatFloat(n, 'g', 15, 64)`.
3. If `abs(n) != 0` and (`abs(n) < 5e-7` or `abs(n) >= 1e21`): scientific notation via `'e'` format, cleaned exponent.
4. If `'g'` format produced `e`/`E` but number is NOT in extreme range: convert to fixed-point `'f'` format.
5. Otherwise, return `'g'` format.

**`FormatNumber(n json.Number) string`** (eval_helpers.go line 57):
1. If raw string does NOT contain `e` or `E`: return verbatim (preserves precision).
2. If contains scientific notation: convert to float64 and delegate to `FormatFloat`.

## 1.5 Boolean Coercion

**`ToBoolean(v any) bool`** (value.go lines 146-177):

| Input | Result |
|---|---|
| `nil` (undefined) | `false` |
| `jsonNullType` (null) | `false` |
| `bool` | identity |
| `string ""` | `false` |
| `string` non-empty | `true` (including `"0"` and `"false"`) |
| `float64 0` | `false` |
| `float64` non-zero | `true` |
| `json.Number 0` | `false` |
| `*OrderedMap` empty | `false` |
| `*OrderedMap` non-empty | `true` |
| `map[string]any` empty | `false` |
| `[]any` len 0 | `false` |
| `[]any` len 1 | `ToBoolean(elem[0])` (recursive) |
| `[]any` len > 1 | `true` if ANY element is truthy |
| `*Sequence` | `ToBoolean(CollapseSequence(val))` |

## 1.6 String Coercion

**`stringifyValue(v any) (string, error)`** (eval_helpers.go lines 27-50):

| Input | Result |
|---|---|
| `nil` | `""` (empty string) |
| `string` | identity |
| `json.Number` | `FormatNumber(val)` |
| `float64` | `FormatFloat(val)` |
| `bool true` | `"true"` |
| `bool false` | `"false"` |
| Other | JSON-serialized via `marshalNoHTMLEscape` (no `&`, `<`, `>` escaping) |

## 1.7 DeepEqual

**`DeepEqual(a, b any) bool`** (value.go lines 191-243):

1. Normalize numbers: both passed through `normalizeNumber` (json.Number → float64).
2. Nil/Null check (line 193): returns true only if both nil or both null. Mixed nil/null returns false.
3. Type-specific: bool, float64, string compared by value; `[]any` element-wise; maps compared via `IsMap`/`MapGet` helpers (cross-type comparison).

## 1.8 JSONataError

**Source:** value.go lines 247-263.

```go
type JSONataError struct {
    Code    string  // S0xxx, T0xxx, T1xxx, T2xxx, D1xxx, D2xxx, D3xxx, U1001
    Token   string
    Value   any
    Message string
}
```

`Error()`: `Code + ": " + Message` if both present; `Message` if only message; `Code` otherwise.

## 1.9 OrderedMap

**Source:** `ordered_map.go` lines 1-283.

Structure: `keys []string` + `data map[string]any`. Preserves insertion order.

Key methods: `Set`, `Get`, `Has`, `Delete`, `Keys`, `Len`, `Range`, `MarshalJSON` (insertion order), `UnmarshalJSON` (preserves order).

**Helper functions** (lines 232-283): `MapGet`, `MapKeys`, `MapLen`, `IsMap`, `MapRange` -- bridge functions handling both `*OrderedMap` and `map[string]any`.

**`DecodeJSON`** (line 145): Uses `json.Decoder` with `UseNumber()`. Objects → `*OrderedMap`, arrays → `[]any`, numbers → `json.Number`, null → `Null` sentinel.

## 1.10 Environment

**Source:** `env.go` lines 1-165.

Linked-list scoping chain. Each env has `parent`, `bindings map[string]any`, shared `*callCounter` (depth tracking), and `context.Context`.

- `defaultMaxCallDepth = 100`
- `Lookup`: walks parent chain
- `LookupWithEnv`: returns value AND the environment where found (for `%` operator)
- `LookupDirect`: this env only, no parent walk
- `ResetCallCounter`: fresh counter, decouples from parent
- `IncrEvalDepth`/`DecrEvalDepth`: `$eval` nesting tracking, D3121 on overflow
- `Clone`: shallow copy bindings, shares parent and calls

## 1.11 Function Types

**Source:** `env.go` lines 166-183.

- **`BuiltinFunction`**: `func(args []any, focus any) (any, error)`
- **`EnvAwareBuiltin`**: `func(args []any, focus any, env *Environment) (any, error)` -- for HOFs and `$eval`
- **`SignedBuiltin`**: Wraps `BuiltinFunction` with type signature string for validation

## 1.12 Lambda

**Source:** `env.go` lines 186-193.

```go
type Lambda struct {
    Params        []string
    Body          *parser.Node
    Closure       *Environment
    Thunk         bool
    Sig           string
    CapturedFocus any   // focus ($) at definition time for zero-param closures
}
```

---

# Section 2: Lexer Specification

## 2.1 Token Types

**Source:** `internal/lexer/token.go` lines 1-71.

54 token types total. Key tokens with binding powers (from parser):

| BP | Tokens |
|---|---|
| 80 | `(` `[` |
| 75 | `.` `@` `#` |
| 70 | `{` |
| 60 | `**` `*` `/` `%` |
| 50 | `+` `-` `&` |
| 45 | `~>` |
| 40 | `=` `!=` `<` `>` `<=` `>=` `in` `^` |
| 30 | `and` |
| 25 | `or` |
| 20 | `..` `|` `?` `?:` `??` |
| 10 | `:=` |
| 0 | all others (separators, delimiters, EOF, NUD-only tokens) |

## 2.2 Context-Sensitive `/`

The `infix` parameter to `Next()` determines whether `/` starts a regex literal or is division:
- `infix == false` (prefix position): regex literal
- `infix == true` (infix position): division operator

Parser sets `infix = true` after value-producing NUD tokens, resets to `false` after operators/separators.

## 2.3 String Literal Lexing

**Source:** `lexer.go` lines 260-307.

- Delimited by `"` or `'`
- Standard JSON escapes: `\"`, `\\`, `\/`, `\b`, `\f`, `\n`, `\r`, `\t`
- Unicode escapes: `\uXXXX` with UTF-16 surrogate pair support (high + low surrogates decoded to single codepoint)
- Error codes: S0101 (unterminated), S0103 (bad escape), S0104 (bad unicode)

## 2.4 Number Literal Lexing

**Source:** `lexer.go` lines 311-349.

- No leading `+`; leading `0` only for `0` or `0.xxx`
- Optional fractional `.digits`, optional exponent `e[+-]digits`
- Validated with `strconv.ParseFloat`; Inf/NaN → S0102

## 2.5 Regex Literal Lexing

**Source:** `lexer.go` lines 70-124.

- Only in prefix position (when `infix == false`)
- A `\` escapes the next character: the pair is scanned as plain pattern text
  and takes part in neither the depth tracking nor the terminator search, so
  `/\(/`, `/\}/` and `/\//` all lex. Consuming escapes pairwise subsumes the
  even/odd backslash counting the Go code used for `/` alone: after an escaped
  backslash the next character is live again, so `/\\(x)/` still needs its `)`
  and `/a\\/` ends at that `/`.
- Tracks bracket depth for `()[]{}` over the remaining (unescaped) characters;
  the literal ends at the first unescaped `/` at depth 0, which lets `/` appear
  inside a character class (`/[/]/`). Unbalanced unescaped brackets — `/a(b/`,
  `/]/` — are S0302.
- Valid flags: `i`, `m`; flag `g` always appended implicitly
- Errors: S0301 (empty pattern), S0302 (bad flag or unterminated)

> **Deviation from jsonata-js (deliberate, narrow).** jsonata-js guards each
> depth change with a single-character `charAt(position - 1) !== '\\'` test, so
> it treats a bracket preceded by an *escaped* backslash as escaped. The two
> algorithms therefore disagree on exactly one shape: a bracket preceded by an
> even number (≥ 2) of backslashes. jsntrs counts it, so `/\\(x)/` and
> `/\\{2}/` lex here and are S0302 in jsonata-js; jsonata-js skips it, so
> `/\\}/`, `/\\]/` and `/\\{/` lex there and are S0302 here. Everything else,
> including every literal with singly-escaped brackets, lexes identically.
> Counting was chosen because it keeps `\\` meaning "a literal backslash" —
> the escape stops leaking into the character after it.

## 2.6 Other Lexer Features

- **Block comments**: `/* ... */`, no nesting. S0106 for unclosed.
- **Backtick identifiers**: `` `field name` `` for special characters. S0105 for unterminated.
- **Keywords**: `and` → TokenAnd, `or` → TokenOr, `in` → TokenIn, `true`/`false`/`null` → TokenValue
- **Variables**: `$name` → TokenVariable, `$$` → TokenVariable with value `"$"`, bare `$` → value `""`
- **Two-char operators** checked before single-char: `..`, `:=`, `!=`, `>=`, `<=`, `**`, `~>`, `?:`, `??`

---

# Section 3: Parser Specification

## 3.1 Pratt Parser Architecture

**Source:** `internal/parser/parser.go` lines 57-165.

Top-down operator-precedence parser. Core loop in `expression(bp)`:
1. Call `nud()` for prefix/primary expression
2. While `bindingPower(token) > bp`: call `led(left)` for infix extension

## 3.2 NUD (Prefix) Handlers

| Token | Result | Notes |
|---|---|---|
| Name | `NodeName` or `NodeLambda` | If `"function"` or `"λ"` followed by `(`, parses lambda |
| Variable | `NodeVariable` | `$name` reference |
| String | `NodeString` | Literal |
| Number | `NodeNumber` | Literal, `NumVal` populated |
| Value | `NodeValue` | `true`/`false`/`null` |
| Regex | `NodeRegex` | `/pattern/flags` |
| `-` | `NodeUnary` or folded `NodeNumber` | Unary negation; folds into number literal if possible |
| `*` | `NodeWildcard` | Wildcard |
| `**` | `NodeDescendant` | Descendant |
| `%` | `NodeParent` | Parent reference |
| `[` | `NodeUnary "["` | Array constructor; comma-separated expressions |
| `{` | `NodeUnary "{"` | Object constructor; key-value pairs in LHS |
| `(` | `NodeBlock` | Paren/block; semicolon-separated expressions |
| `?` | `NodePlaceholder` | For partial application |
| `\|` or `~` | `NodeTransform` | Transform expression |
| `and`/`or`/`in` | `NodeName` | Keywords as field names in prefix position |

## 3.3 LED (Infix) Handlers

| Token | Result | Assoc | Notes |
|---|---|---|---|
| `(` | `NodeFunction`/`NodePartial` | - | Function call; partial if any `?` placeholder |
| `[` | `NodeBinary "["` | - | Subscript/predicate; empty `[]` sets KeepArray |
| `.` | `NodeBinary "."` | Left (bp=74 for RHS) | Path step |
| `@` | modifies left | - | Sets Focus variable |
| `#` | modifies left | - | Sets Index variable |
| `?` | `NodeCondition` | Right | Ternary with optional `:` else |
| `:=` | `NodeBind` | Right (bp-1) | Variable binding |
| `~>` | `NodeBinary` | Right (bp-1) | Chain/pipe |
| `?:` | `NodeBinary` | Right (bp-1) | Elvis |
| `??` | `NodeBinary` | Right (bp-1) | Null coalescing |
| `..` | `NodeBinary` | Right (bp-1) | Range |
| `and`/`or` | `NodeBinary` | Left | Logical |
| `in` | `NodeBinary` | Left | Containment |
| `=`/`!=`/`<`/`>`/`<=`/`>=` | `NodeBinary` | Left | Comparison |
| `+`/`-`/`*`/`/`/`%`/`**`/`&` | `NodeBinary` | Left | Arithmetic/concat |
| `{` | modifies left | - | Group-by expression |
| `^` | `NodeSort` | - | Sort with terms |

## 3.4 AST Post-Processing (ProcessAST)

**Source:** `internal/parser/process.go`

Key transformations:
1. **Path flattening**: Binary `.` chains → `NodePath` with `Steps` slice
2. **String-to-name**: String literals in path context → `NodeName`
3. **KeepSingletonArray**: Propagated when any step has KeepArray
4. **Group propagation**: Group expressions attached to paths
5. **Focus/Index propagation**: `@`/`#` bindings survive path flattening
6. **Tail-call marking**: Lambda bodies analyzed for tail-position function calls → `Thunk = true`

## 3.5 Fast-Path Analysis (AnalyzeFastPath)

**Source:** `internal/parser/analysis.go`

Three classifiers checked in order:
1. **Pure path**: Simple dotted field navigation → GJSON path
2. **Comparison**: `<pure-path> = <literal>` → ComparisonFastPath
3. **Function**: `$func(<pure-path>)` → FuncFastPath (23 supported functions in Go; the Rust port covers 26 kinds -- see Section 6.5. `$round` excluded in both)

GJSON name escaping: `@`-prefix names excluded entirely; names with special chars backtick-escaped.

## 3.6 Signature Parsing

**Source:** `internal/parser/signature.go`

Format: `<params:returnType>`. Type specifiers: `b`(bool), `n`(num), `s`(str), `l`(null), `a`(array), `o`(obj), `f`(func), `j`(JSON), `x`(any). Modifiers: `?`(optional), `+`(variadic), `-`(separator). Content types: `a<n>` (array of numbers). Union types: `(sn)`.

---

# Section 4: Evaluator Specification

## 4.1 Core Dispatch (`evaluator.go`)

The entry point is `Eval(node *parser.Node, input any, env *Environment) (any, error)` (line 12). It returns `(nil, nil)` for undefined results.

### 4.1.1 Pre-dispatch Checks

1. **Nil node** (line 13): Returns `(nil, nil)` immediately.
2. **Context cancellation** (line 15): Checks `env.Context().Err()`; if non-nil, returns the context error.
3. **Group expression** (line 17): If `node.Group != nil`, delegates to `evalGroupBy` before any type dispatch. This handles expressions like `expr{key:val}`.

### 4.1.2 Node Type Dispatch (lines 23-77)

| Node Type | Handler | File:Line |
|-----------|---------|-----------|
| `NodeValue` | `evalValue(node)` | `eval_helpers.go:217` |
| `NodeString` | Returns `node.Value` directly | `evaluator.go:27` |
| `NodeNumber` | Returns `node.NumVal` directly | `evaluator.go:29` |
| `NodeVariable` | `evalVariable(node, input, env)` | `eval_helpers.go:230` |
| `NodeName` | `evalName(node, input, env)` | `eval_helpers.go:241` |
| `NodeWildcard` | `evalWildcard(node, input, env)` | `eval_helpers.go:310` |
| `NodeDescendant` | `descendantLookup(input)` | `path.go:1174` |
| `NodePath` | `evalPath(node, input, env)` | `path.go:19` |
| `NodeBinary`, `NodeApply` | `evalBinary(node, input, env)` | `eval_binary.go:11` |
| `NodeUnary` | `evalUnary(node, input, env)` | `eval_unary.go:9` |
| `NodeBlock` | `evalBlock(node, input, env)` | `eval_chain.go:92` |
| `NodeCondition` | `evalCondition(node, input, env)` | `eval_chain.go:105` |
| `NodeBind` | `evalBind(node, input, env)` | `eval_chain.go:119` |
| `NodeFunction` | `evalFunction(node, input, env)` | `eval_function.go:11` |
| `NodeLambda` | `evalLambda(node, input, env)` | `eval_function.go:82` |
| `NodePartial` | `evalPartial(node, input, env)` | `eval_function.go:101` |
| `NodeSort` | `evalSort` or `evalSortWithParentTracking` | `eval_sort.go:10`, `path.go:532` |
| `NodeRegex` | `evalRegex(node.Value)` | `eval_regex.go:199` |
| `NodeTransform` | `evalTransform(node, input, env)` | `eval_transform.go:33` |
| `NodeParent` | Env lookup of `parentKey` (`%%`) | `evaluator.go:71` |

### 4.1.3 Public API

`ApplyFunction(fn any, args []any, focus any, env *Environment)` (line 82) is the public API for calling function values from the standard library, delegating to `callFunction`.

---

## 4.2 Value Literals and Variables (`eval_helpers.go`)

### 4.2.1 `evalValue` (line 217)

Handles keyword literals:
- `"true"` -> `true` (Go bool)
- `"false"` -> `false` (Go bool)
- `parser.NullJSON` -> `Null` (the singleton `jsonNullType{}`, distinct from Go `nil`)
- Any other value -> `(nil, nil)` (undefined)

### 4.2.2 `evalVariable` (line 230)

- Empty variable name `""` (i.e., `$` or `$$`): returns `input` directly (line 232).
- Named variable: looks up `node.Value` in env via `env.Lookup()`. If not found, returns `(nil, nil)`.

### 4.2.3 `evalName` -- Field Lookup (line 241)

Handles field access on objects and auto-mapping over arrays:

- **`*OrderedMap`** (line 243): Gets field by `node.Value`. Missing key returns `nil`. Existing key with `nil` value returns the `Null` sentinel.
- **`map[string]any`** (line 252): Same logic as OrderedMap.
- **`[]any`** (line 261): **Auto-mapping** -- iterates each array element, recursively calls `evalName`. Arrays returned from sub-lookups are **flattened** into the result sequence (not nested). If no elements matched but at least one element had the field defined (e.g., as an empty array `[]`), returns `[]any{}` so `$exists` sees it as defined (line 293). Empty sequence with no field found returns `nil`.
- **`*Sequence`** (line 304): Collapses to the appropriate type and re-dispatches.
- **Default** (line 306): Returns `nil` (non-object input).

### 4.2.4 `evalWildcard` -- Wildcard Operator `*` (line 310)

- **Map input** (line 311): Iterates all values. Array values are flattened (their elements added individually). Produces a sequence; collapses per standard rules.
- **`[]any` input** (line 332): Maps wildcard over each element that is a map, collecting values. Non-map array elements are included directly.
- **Other** (line 349): Returns `nil`.

### 4.2.5 `stringifyValue` -- String Coercion for `&` Operator (line 27)

Used by the `&` concatenation operator:
- `nil` -> `""` (empty string)
- `string` -> identity
- `json.Number` -> `FormatNumber()` (canonical form)
- `float64` -> `FormatFloat()` (JavaScript-compatible formatting)
- `bool` -> `"true"` / `"false"`
- Other (objects, arrays) -> JSON marshal with HTML escaping disabled

### 4.2.6 Number Formatting (`FormatFloat`, line 72)

Matches JavaScript `Number.toString()`:
- Numbers between `5e-7` and `1e21` (exclusive): decimal notation via `strconv.FormatFloat('g', 15, 64)`
- Numbers outside that range: scientific notation with cleaned exponents -- leading zeros stripped, sign always present (`1e+21`, `1e-7`)
- `NaN` / `Inf` -> `"null"`

### 4.2.7 `compareValues` -- Relational Operators (line 104)

For `<`, `<=`, `>`, `>=`:
- If left operand is non-nil and neither number nor string: **T2010**.
- If either operand is `nil`: returns `(nil, nil)` (undefined propagation).
- Both numeric: standard float64 comparison.
- Both strings: lexicographic Go string comparison.
- Mixed number/string: **T2009** ("must be both numbers or both strings").
- Other non-numeric, non-string type: **T2010**.

### 4.2.8 `compareOrder` -- Sort Comparison (line 158)

Used internally by sort:
- Both nil: 0
- One nil: nil sorts after non-nil (nil = +1, non-nil = -1)
- Both numeric: standard comparison
- Both strings: lexicographic
- Mixed string/number: **T2007**
- Other types: **T2008**

### 4.2.9 `containsValue` -- `in` Operator (line 194)

- `nil` right-hand side: `false`
- `[]any`: iterates, uses `DeepEqual`
- `*Sequence`: iterates values, uses `DeepEqual`
- Single value: `DeepEqual(arr, elem)`

### 4.2.10 `ToBoolean` -- Boolean Coercion (`value.go:146`)

- `nil`, `Null` -> `false`
- `bool` -> identity
- `string` -> `str != ""`
- `float64` -> `n != 0`
- `json.Number` -> parsed float, `f != 0`
- `*OrderedMap` -> `len > 0`
- `map[string]any` -> `len > 0`
- `[]any`:
  - Length 0 -> `false`
  - Length 1 -> recurse on element
  - Length > 1 -> `true` if **any** element is truthy
- `*Sequence` -> collapse, then recurse
- Default -> `false`

---

## 4.3 Binary Operators (`eval_binary.go`)

### 4.3.1 Short-Circuit Operators (lines 13-76)

#### `and` (line 13)
- Evaluates left; if `ToBoolean(left)` is false, returns `false` (short-circuit).
- Otherwise evaluates right; returns `ToBoolean(right)`.

#### `or` (line 27)
- Evaluates left; if `ToBoolean(left)` is true, returns `true` (short-circuit).
- Otherwise evaluates right; returns `ToBoolean(right)`.

#### `?:` -- Elvis/Default (line 41)
- Returns left if `ToBoolean(left)` is true, else evaluates and returns right.
- Note: checks **truthiness**, not just non-nil.

#### `??` -- Null-Coalescing (line 52)
- Returns left if `left != nil`, else evaluates and returns right.
- Note: checks **non-nil** (not truthiness). `false`, `0`, `""` pass through.

#### `~>` -- Chain/Pipe (line 63)
- Evaluates left first; delegates to `evalChain(node.Right, left, input, env)`.
- See Section 4.4 for chain semantics.

#### `[` -- Subscript/Filter (line 73)
- Delegates to `evalSubscript(node, input, env)`.
- See Section 4.3.3.

### 4.3.2 Eager Binary Operators (lines 78-191)

Both sides are evaluated before the operator is applied.

#### Arithmetic: `+`, `-`, `*`, `/`, `%`, `**` (lines 89-145)

- If left is non-nil and not a number: **T2001** (or **T2002** if left is `Null`).
- If left is nil: returns `(nil, nil)` -- undefined propagation.
- Same validation for right.
- If both nil after validation: `(nil, nil)`.
- Division `/` (line 126): Division by zero produces `+Inf`/`-Inf`/`NaN` which **propagates** without error. Downstream consumers (e.g., `$string`) produce context-dependent errors.
- Modulo `%` (line 135): Division by zero produces **D3001** immediately.
- `**` (line 140): `math.Pow(l, r)`.
- Result check (line 142): If result is `Inf` or `NaN`, **D1001** ("Number out of range").

#### String Concatenation `&` (line 147)
- Both sides coerced to string via `stringifyValue`.
- `nil` coerces to `""`.

#### Equality `=` (line 159)
- If either is nil: returns `false`.
- Otherwise: `DeepEqual(left, right)`.

#### Inequality `!=` (line 165)
- If either is nil: returns `false`.
- Otherwise: `!DeepEqual(left, right)`.

#### Comparison `<`, `<=`, `>`, `>=` (lines 170-181)
- Delegated to `compareValues` (see 4.2.7).

#### Membership `in` (line 183)
- Delegated to `containsValue(right, left)` (see 4.2.9).

#### Range `..` (line 186)
- Delegated to `evalRange(left, right, env)` (see Section 4.9).

### 4.3.3 Subscript / Filter: `evalSubscript` (line 203)

The `[` binary operator handles subscript (index) and filter (predicate) operations.

**Special case: Parent-tracking subscript** (lines 206-213):
When Left is a Block containing a single path expression and the predicate references `%` (parent), evaluates the inner path in tuple mode via `evalSubscriptBlockParent` so each item retains its parent context for the `%` operator.

**Standard flow:**

1. **Evaluate left side** via `evalSubscriptLeft` (line 214, defined at line 314):
   - `NodeDescendant` left: creates a sequence with `input` + `descendantLookup(input)`.
   - Otherwise: standard `Eval`.
   - Normalizes result to `[]any` items.
   - Returns `(nil, nil, nil)` if left is nil.

2. **KeepArray** (line 222): Propagates from `node.KeepArray` or any node in the left chain (via `hasKeepArrayInChain`).

3. **Try numeric indexing** (line 256):
   - Evaluates right with a representative context (first item).
   - If right evaluates to a number: uses it as an index. Negative indices count from end.
   - Out-of-range: `nil`.
   - If item at index is nil, substitutes `Null`.

4. **Array-of-indices** (line 273, `selectByIndices` at line 351):
   - If right is `[]any` of all-numeric values: selects multiple elements.
   - Indices are sorted ascending before selection.
   - Non-numeric arrays fall through to predicate filter.

5. **Predicate filter** (line 280, `filterByPredicate` at line 291):
   - Keeps items where predicate evaluates to truthy (`ToBoolean`).
   - Binds `parentKey` (`%%`) to the parent value for `%` access.
   - If Left has an `Index` field, binds the index variable to loop position.

---

## 4.4 Chain, Pipe, Block, Condition, Bind (`eval_chain.go`)

### 4.4.1 `evalChain` -- Pipe Operator `~>` (line 9)

1. **Right-associative chaining** (line 11): `a ~> (f ~> g)` is processed as `(a ~> f) ~> g` via recursive decomposition.
2. **Function call with existing args** (line 20): When right is `NodeFunction` with a Procedure, evaluates the function reference, prepends `piped` as the first argument to the existing args, and calls `callFunction`.
3. **Regex on right** (line 49): When right evaluates to a map with a `"pattern"` key, applies regex test via `applyRegexTest`. Returns a match object or nil.
4. **Function validation** (line 60): Right must be a callable type (`BuiltinFunction`, `EnvAwareBuiltin`, `*Lambda`, `*SignedBuiltin`). `nil` -> **T1006**; other types -> **T2006**.
5. **Function composition** (line 70): When `piped` is itself a function, returns a new `BuiltinFunction` that composes `piped` then `fn` (e.g., `$trim ~> $uppercase`).
6. **Normal pipe** (line 85): Calls `fn` with `[]any{piped}` as args.
7. All results are collapsed via `CollapseAndKeep(result, false)`.

### 4.4.2 `evalBlock` (line 92)

- Creates a child environment.
- Evaluates each expression in `node.Expressions` sequentially.
- Returns the value of the **last** expression.

### 4.4.3 `evalCondition` (line 105)

- Evaluates `node.Condition`.
- If truthy: evaluates and returns `node.Then`.
- If falsy and `node.Else != nil`: evaluates and returns `node.Else`.
- If falsy and no Else: returns `(nil, nil)`.

### 4.4.4 `evalBind` -- Variable Assignment `:=` (line 119)

- Evaluates `node.Right`.
- Binds the result to `node.Left.Value` in the **current** environment (not a child).
- Returns the bound value.

---

## 4.5 Function Calls, Lambdas, Partial Application (`eval_function.go`)

### 4.5.1 `evalFunction` (line 11)

1. **Resolve function** (line 13): Evaluates `node.Procedure`. If `%` produces S0217, converts to `nil` (will become T1006).
2. **Name resolution** (line 29): For `NodeName` procedures (no `$` prefix): if the name exists in env as a function but the lookup on input returned nil, produces **T1005** ("no definition -- accessed without $"). If completely unknown, **T1006**.
3. **Evaluate arguments** (line 39): Placeholders produce `nil`.
4. **Signature validation for SignedBuiltins** (line 54): Calls `processCallArgs` with the parsed signature. If validation fails, returns the error. If `returnUndefined`, returns `(nil, nil)`.
5. **Tail-call optimization** (line 69): If `node.Thunk` is true and `fn` is a `*Lambda`, returns `&TailCall{Fn: fn, Args: args}` instead of calling.
6. **Call and collapse** (line 75): Calls `callFunction`, then `CollapseAndKeep(result, node.KeepArray)`.

### 4.5.2 `evalLambda` (line 82)

Creates a `*Lambda` struct:
- `Params`: extracted from argument nodes' `Value` fields.
- `Body`: `node.Body`.
- `Closure`: current `env` (lexical scope capture).
- `Thunk`: `node.Thunk` (TCO marker).
- `Sig`: raw signature string from `node.Signature`.
- `CapturedFocus`: current `input` (used when lambda has 0 params and is called with 0 args).

### 4.5.3 `evalPartial` -- Partial Application (line 101)

1. **Resolve function** (line 102).
2. **Error codes for NodeName** (line 109): T1007 (env has the name, accessed without `$`), T1008 (completely unknown or non-function).
3. **Type validation** (line 116): Must be callable. `nil` -> **T1007**; non-function -> **T1008**.
4. **Build partial** (line 125): Records which args are placeholders. Creates a `BuiltinFunction` closure that, when called, fills placeholders with supplied args in order and delegates to `callFunction`.

### 4.5.4 `callFunction` -- Trampoline (line 153)

The central function-calling mechanism with TCO support.

1. **Nil check** (line 154): `nil` fn -> **T1006**.
2. **Counter setup** (line 157): Gets the call counter from env. `maxIter = counter.max * 10000`.
3. **Context check** (line 166): Checks for cancellation each iteration.
4. **Dispatch by type** (lines 169-224):
   - `*SignedBuiltin`: calls `f.Fn(args, focus)` directly (signature already validated at call site).
   - `BuiltinFunction`: calls `f(args, focus)`.
   - `EnvAwareBuiltin`: calls `f(args, focus, env)`.
   - `*Lambda`:
     a. **Signature validation** (line 178): If lambda has a `Sig`, calls `processCallArgs`. Undefined propagation or errors returned.
     b. **Stack depth check** (line 189): Increments depth counter; if > max -> **U1001** ("stack overflow").
     c. **Bind params** (line 195): Creates child env from lambda's closure. Binds each param to corresponding arg (excess params get `nil`).
     d. **Focus** (line 202): When lambda has 0 params and 0 args, uses `CapturedFocus`; otherwise uses `focus`.
     e. **Evaluate body** (line 206).
     f. **TCO trampoline** (line 211): If result is `*TailCall`, increments iteration counter (exceeding `maxIter` -> **U1001**), updates `fn` and `args`, continues loop.
   - Default: **T1006** ("not a function").

---

## 4.6 Tail-Call Optimization (`tailcall.go`)

### 4.6.1 `TailCall` Sentinel (line 5)

```go
type TailCall struct {
    Fn   any
    Args []any
}
```

A sentinel value returned by tail-position calls within lambdas. The trampoline loop in `callFunction` catches these and re-invokes without growing the Go stack.

### 4.6.2 Thunk Protocol

1. **Parser marks** `node.Thunk = true` for calls in tail position of a lambda body.
2. **evalFunction** (line 69): When `node.Thunk` is true and fn is `*Lambda`, returns `&TailCall{...}`.
3. **callFunction trampoline** (line 211): Catches `*TailCall` results, extracts `Fn` and `Args`, loops.
4. **Iteration limit**: `counter.max * 10000` iterations. Prevents infinite tail-recursive loops.

---

## 4.7 Path Evaluation (`path.go`)

This is the most complex subsystem in the evaluator. Path evaluation has two modes: **simple** and **tuple**.

### 4.7.1 Mode Selection: `evalPath` (line 19)

- If any step has `#$var` index bindings, `@$var` focus bindings, or references `%` (NodeParent): uses **tuple mode** via `evalPathTuple`.
- Otherwise: **simple mode** via `evalPathSimple`.

### 4.7.2 Detecting Tuple Requirements: `pathHasTupleStep` (line 29)

Returns true when any step:
- Has non-empty `Index` or `Focus` field.
- Is a subscript whose left has `Index` or `Focus`.
- Is a sort whose left contains index bindings (`nodeHasIndexBinding`).
- References `%` (NodeParent) anywhere in the subtree (`nodeHasParentRef`).

### 4.7.3 Simple Path Mode: `evalPathSimple` (line 105)

Steps are evaluated left-to-right, threading each step's result into the next:

1. **NodeParent `%`** (line 122): Looks up `parentKey` (`%%`) via `LookupWithEnv`, then advances env to the binding env's parent. Handles join-flag (`%%j`) chains by skipping ancestor envs that are join bindings with the same parent value.
2. **Standard steps** (line 152): Calls `evalPathStep(step, result, env, prevWasMapper, node.KeepSingletonArray)`.
3. **Empty array pruning** (line 165): Empty arrays from auto-mapping mean "nothing found" -> nil. Exception: `NodeName`/`NodeString` steps preserve empty arrays as genuine values.
4. **Mapper tracking** (line 170): After each step, `prevWasMapper` is set if the result is `[]any` or `*Sequence`.
5. **KeepSingletonArray** (line 175): If the path has `[]`, wraps non-nil scalar results in `[]any{result}`.

### 4.7.4 Tuple Path Mode: `evalPathTuple` (line 198)

Maintains a list of `pathCtx` (value + env) contexts so position variables bound at one step remain accessible in subsequent steps.

**Step-by-step processing** (line 205):

1. **Strip step-level Group** (line 212): If a step has an inline Group (e.g., `Product{key:val}`), it's removed from the step and saved as `finalGroup` to be applied after all contexts are collected.

2. **Sort steps** (line 227): Applied globally via `evalTupleSort` to all tuples simultaneously.

3. **NodeParent `%` steps** (line 235): Navigates up the parent chain using env bindings. Uses `LookupWithEnv` to find the binding env and its parent. Handles join-flag chains.

4. **Subscript with `%[predicate]`** (line 281): Navigates `%` to parent first, then applies predicate using parent's env.

5. **Block-subscript with parent ref** (line 314): For `(path)[predicate with %]`, evaluates inner path in tuple mode to preserve parent bindings.

6. **Block containing single path** (line 366): Expands inner path via `expandPathTuple` to preserve parent bindings.

7. **Join operator `@`** (line 385): For `Contact@$c[$c.ssn = $e.SSN]`:
   - Evaluates left node against each context.
   - Binds `focusVar` to each element.
   - Applies predicate; keeps matching contexts.
   - Post-filter index binding if present.

8. **Compound join-filter** (line 412): For `books@$b[pred][1]`: processes inner join-filter first, then applies outer subscript (numeric index) to the collected tuples.

9. **Standard steps** (line 453): Evaluates step, flattens results into contexts via `appendTupleResults` or `appendTupleResultsNoParent` (for root-level `$`/`$$`).

**Final output** (line 496):
- Applies `finalGroup` or `node.Group` via `evalTupleGroup` if present.
- Otherwise collapses all context values into a sequence.
- Applies `KeepSingletonArray`.

### 4.7.5 `evalPathStep` (line 1027)

Evaluates a single path step against input, handling auto-mapping:

**Direct delegation** (line 1032):
- `NodeNumber`: **S0213** error (numeric literal not valid as path step).
- `NodeName`, `NodeWildcard`, `NodeVariable`, `NodeString`, `NodeValue`, `NodeSort`: Eval directly (they handle arrays natively).
- `NodeBlock` when not preceded by mapper: Eval directly (preserves whole-array context for sorts).
- `NodeDescendant`: Creates sequence of `input` + `descendantLookup(input)`. Arrays are NOT added directly (to avoid duplicates); only non-array inputs are prepended.
- `NodeBinary` subscript `[` when not preceded by mapper: Eval directly (subscript on whole array).
- `NodeUnary` `[` (array constructor) when not preceded by mapper: Eval directly.

**Auto-mapping over arrays** (line 1080):
For function calls and other step types on array input:
- Single item: delegates directly (function calls use `evalPathFunctionStep`).
- Array: maps step over each element. Group steps (`[...]`) preserve per-element arrays as nested. Other steps flatten `[]any` and `*Sequence` results.
- With `keepSingletonArray` + group step: returns `seq.Values` directly to prevent collapse.

### 4.7.6 `evalPathFunctionStep` (line 1135)

Function calls as path steps (e.g., `arr.fn(6)`):
- Resolves function reference.
- Evaluates declared arguments.
- **Lambda prepend** (line 1161): For user-defined lambdas, prepends the path element as first argument only when there are fewer explicit args than lambda parameters.
- For builtins: the path element is passed as `focus` (not prepended to args).

### 4.7.7 `appendTupleResults` (line 773)

Flattens a step's result into `(value, env)` contexts:
- Binds `parentKey` (`%%`) to the parent value.
- If step has `Index`: binds to output element position.
- If step has `Focus` (join): binds focus variable; context VALUE stays at parent level.
- `parentJoinFlag` (`%%j`) is set for join steps.

### 4.7.8 `descendantLookup` (line 1174)

Recursively collects all values at all depths:
- For maps: iterates values. Array values have their elements added individually (not the array as a whole).
- For arrays: iterates elements recursively.
- Returns `*Sequence`.

### 4.7.9 Parent Operator `%`

**Environment keys:**
- `parentKey = "%%"` (line 704): Stores the parent context value.
- `parentJoinFlag = "%%j"` (line 705): Marks join-step environments.

**In evaluator dispatch** (line 71): Looks up `parentKey` in env. If not found, **S0217**.

**In simple path mode** (line 122): Uses `LookupWithEnv` to find the binding env, then navigates to its parent. Handles join-flag chains.

**In tuple mode** (line 235): Same logic but applied per-context.

---

## 4.8 Sort (`eval_sort.go`)

### 4.8.1 `evalSort` (line 10)

1. Evaluates `node.Left` to get items.
2. Normalizes to `[]any`; tracks `wasArray` for singleton handling.
3. If no sort terms: returns items unchanged (with singleton handling).
4. Clones the array (stable sort preserves original order for equal elements).
5. Sorts via `SortItemsErr` with `compareSortTerms`.

### 4.8.2 `SortItemsErr` (line 68)

Generic stable sort wrapper:
- Uses `sort.SliceStable` (stable sort guarantees).
- Propagates the first error encountered during comparison.
- Only the sign of comparator return matters: negative = a < b.

### 4.8.3 `compareSortTerms` (line 87)

Multi-key comparison:
- Evaluates each sort term's expression against both values.
- Uses `compareOrder` for the actual comparison.
- If `term.Descending`, negates the result.
- Returns 0 (equal) only if all terms compare equal.

**Error codes from `compareOrder`:**
- Mixed string/number: **T2007**.
- Incomparable types: **T2008**.

### 4.8.4 `evalSortWithParentTracking` (`path.go:532`)

For sort expressions whose terms reference `%`:
- Evaluates left expression in tuple mode (via `buildSortCtxs`).
- Sorts tuples using per-item envs.
- Collapses sorted values.

### 4.8.5 `evalTupleSort` (`path.go:655`)

Sort within tuple path mode:
- If sort has Left navigation (e.g., `Product^(ProductID)`): expands via Left first, then sorts.
- For multi-step Left paths with `#$var` bindings: uses `expandPathTuple` to preserve index variables.
- If no Left navigation: sorts existing ctxs directly.

---

## 4.9 Range Operator (`eval_range.go`)

### `evalRange` (line 8)

- Left must be an integer (float64 with `math.Trunc(ln) == ln`). If non-numeric: **T2003**. If non-integer: **T2003**.
- Right must be an integer. If non-numeric: **T2004**. If non-integer: **T2004**.
- If either is nil: `(nil, nil)`.
- If `lo > hi`: `(nil, nil)`.
- **Maximum range**: 10,000,000 items. Exceeding -> **D2014**.
- Returns `[]any` of `float64` values from `lo` to `hi` inclusive.
- Checks context cancellation every 10,000 iterations.

---

## 4.10 Regex (`eval_regex.go`)

### 4.10.1 Regex Engine

Uses Go's `regexp` package (RE2 engine). Key properties:
- **Guaranteed linear-time matching** -- no backtracking, no timeouts.
- Supports inline flags `(?ims)` for case-insensitive, multiline, dotall.
- Does NOT support lookahead/lookbehind (RE2 limitation).

### 4.10.2 `Regex` Type (line 14)

Wraps `*regexp.Regexp`:
- `FindStringMatch(s)`: Returns all matches via `FindAllStringSubmatchIndex`. First match returned immediately; subsequent matches available via `FindNextMatch`.
- `MatchString(s)`: Boolean test.

### 4.10.3 `Match` Type (line 19)

Fields: `Index` (byte offset), `Length` (byte length), plus internal fields for submatch data.
- `String()`: Returns matched text.
- `GroupCount()`: Number of groups (including group 0).
- `GroupByNumber(i)`: Returns `*Group` for capture group `i`.
- `FindNextMatch()`: Returns next match from pre-computed results. Pre-computing preserves anchor semantics (`^`, `$`, `\b`).

### 4.10.4 `CachedCompileRegex` (line 135)

Thread-safe compiled regex cache using `sync.Map`. Cache key is `inlineFlags + ":" + pattern`.

### 4.10.5 Flag Translation: `re2InlineFlags` (line 116)

Translates JSONata regex flags to RE2 inline flags:
- `i` -> case insensitive
- `m` -> multiline (`^`/`$` match line boundaries)
- `s` -> dotall (`.` matches `\n`)
- `g` flag: **ignored** at compilation (global matching is handled by iteration in match/replace functions)
- `x`, `u` flags: **ignored** (not supported by RE2)

### 4.10.6 `evalRegex` (line 199)

Parses raw regex string `"pattern/flags"`:
- Finds last `/` separator.
- Validates flags suffix.
- Returns `map[string]any{"pattern": ..., "flags": ...}`.

### 4.10.7 `applyRegexTest` -- Chain Operator with Regex (line 162)

When `~>` has a regex on the right:
- Input must be string (non-string returns nil).
- Compiles and finds first match.
- Returns match object: `{"match": text, "start": runeIdx, "end": runeIdx, "groups": [...]}`
- Start/end are **rune-based** (Unicode code point positions), not byte positions.
- Uncaptured groups produce `""`.

---

## 4.11 Unary Operators (`eval_unary.go`)

### 4.11.1 Unary Negation `-` (line 11)

- Evaluates expression.
- `nil` -> `nil`.
- Non-numeric -> **D1002**.
- Returns negated float64.

### 4.11.2 Array Constructor `[` (line 28)

- Evaluates each expression in `node.Expressions`.
- `nil` values are **skipped** (not included).
- **Flattening rules**:
  - `*Sequence` with `ConsArray` flag OR explicit inner array constructor: appended as single element (no flattening).
  - `*Sequence` without ConsArray: individual values spread into result.
  - `[]any` from explicit array constructor: appended as single element.
  - `[]any` from other sources: **spread** into result (flattened one level).
  - Scalars: appended directly.

### 4.11.3 Object Constructor `{` (line 70)

`evalObjectConstructor`:
- Iterates key-value pairs from `node.LHS` (pairs at indices `[i, i+1]`).
- Key must evaluate to string; nil keys are skipped.
- Non-string key: **T1003**.
- Duplicate key: **D1009**.
- Values that are sequences are collapsed.
- Nil values are skipped (key not added to object).
- Returns `*OrderedMap` (preserves insertion order).

---

## 4.12 Group-By (`eval_group.go`)

### 4.12.1 `evalGroupBy` (line 14)

Entry point for `expr{key:val}` expressions.

**Tuple delegation** (line 18): If the base expression is a path with tuple steps or the group expression references `%`, delegates to `evalPathTuple`.

**Standard flow:**
1. Copies node with Group cleared, evaluates base expression.
2. Normalizes to `[]any` items.

   **Rust port deviation (jsntrs-6wr.9).** The Go reference propagated a nil
   base: `Missing{'k': 'v'}` returned nil. The Rust port instead normalizes
   an undefined *or empty* base to a single undefined item, matching
   jsonata-js `evaluateGroupExpression`, which wraps a non-array input with
   `createSequence` and then pushes `undefined` when the sequence is empty.
   A group therefore always yields an object: `Missing{'k': 'v'}` is
   `{"k": "v"}` (both pair halves are literals), `Missing{'k': $}` is `{}`
   (the key is defined, the value is undefined, so only the pair drops), and
   `Missing{Other: 'v'}` is `{}` (the key is undefined, so the item is
   skipped). This is what makes a per-item group over a mapped step emit the
   trailing empty object — `items.($string(x){'k': $})` over
   `{"items":[{"x":3},{"x":"s"},{"y":1}]}` is `[{"k":"3"},{"k":"s"},{}]`.
   Pinned by `testdata/groups/rust-group-undefined-value/`.

   The tuple route (`evalTupleGroup`) is *not* aligned: `eval_path_tuple`
   still returns undefined when the tuple stream empties, because jsonata-js
   dereferences `item['@']` on the pushed `undefined` and throws a raw
   `TypeError` there.
3. For each group pair `(keyNode, valNode)`:
   a. Evaluates key for each item. Nil keys are skipped. Non-string keys: **T1003**.
   b. Groups items by key value (maintains insertion order via `groupOrder`).
   c. For each group:
      - Duplicate key across pairs: **D1009**.
      - Group input: array of items if multiple, single item if one.
      - Binds `$index` (first item's index in the input array) and `$key` (key string) in child env.
      - Evaluates value expression against group input.
      - Applies `KeepArray` to value result.
4. Returns `*OrderedMap`.

### 4.12.2 `evalTupleGroup` (`path.go:892`)

Group-by in tuple mode:
- Phase 1: Groups contexts by key. Key must be string (**T1003** if not).
- Phase 2: Evaluates value expression per group. Single-item groups get single value as context; multi-item groups get array. Multi-item groups use `mergeGroupEnvs` to merge per-tuple environments.

### 4.12.3 `mergeGroupEnvs` (`path.go:955`)

Merges multiple environments for a group:
- Finds common ancestor as parent.
- Collects variable names from tuple-specific envs (stops at envs without `parentKey`).
- For each variable: if all values identical, keep single; otherwise collect into array.
- Special handling for `parentKey` and `parentJoinFlag`: uses first env's value.

---

## 4.13 Transform Operator (`eval_transform.go`)

### 4.13.1 `evalTransform` (line 33)

Returns a `BuiltinFunction` closure. When called, applies the transform to the first argument (or focus).

### 4.13.2 `applyTransform` (line 54)

1. **Nil input** (line 56): Returns `(nil, nil)`.
2. **Deep clone** (line 58): `deepClone` recursively copies `*OrderedMap`, `map[string]any`, and `[]any`. Scalars are returned by value.
3. **Pattern matching** (line 60): Evaluates `node.Pattern` against cloned input.
4. **Target extraction** (line 65): Targets must be objects (`*OrderedMap` or `map[string]any`). Arrays are iterated for object elements. Non-object results are ignored for mutation but update/delete clauses are still type-validated.
5. **Non-object pattern match** (line 79): Returns cloned input after validating clauses.
6. **Apply to each target** (line 85): Calls `applyTransformTarget`.

### 4.13.3 `applyTransformTarget` (line 117)

- **Update clause** (line 119): If non-nil and non-Null, must be a map (**T2011** otherwise). Merges via `transformMerge` (copies all keys from update into target).
- **Delete clause** (line 128): If non-nil and non-Null, must be `[]any` (of strings) or a single string (**T2012** otherwise). Deletes named keys via `transformDelete`.

### 4.13.4 `deepClone` (line 7)

- `*OrderedMap`: Creates new ordered map, recursively clones values.
- `map[string]any`: Creates new map, recursively clones values.
- `[]any`: Creates new slice, recursively clones elements.
- Default (scalars): Returns as-is.

---

## 4.14 Signature Validation (`signature.go`)

### 4.14.1 `processCallArgs` (line 26)

Three pre-call concerns:
1. **Nil propagation**: If a non-optional typed arg is nil and `l` (null) is not in its accepted types, the entire call returns undefined (`returnUndefined = true`).
2. **Singleton coercion**: When spec expects `a` (array) and arg is a matching element type, wraps in `[]any{arg}`.
3. **Type validation**: Delegates to `validateCallArgs`.

### 4.14.2 `validateCallArgs` (line 62)

Walks specs and args in parallel:
- **Variadic** (line 70): Validates all remaining args against the variadic spec.
- **Missing non-optional arg** (line 84): **T0410** ("too few arguments").
- **Extra args beyond specs** (line 101): **T0410** ("too many arguments").

### 4.14.3 `validateOneCallArg` (line 112)

- Base type mismatch with content type: **T0412** ("must be an array of X").
- Base type mismatch without content type: **T0410**.
- Content type validation: if arg is array, validates every element against content type.

### 4.14.4 Type Characters (line 160)

| Char | Type | Description |
|------|------|-------------|
| `x` | any | Anything including functions |
| `j` | JSON | Any JSON value (not a function) |
| `n` | number | `float64` or `json.Number` |
| `s` | string | Go `string` |
| `b` | boolean | Go `bool` |
| `l` | null | `nil` (Go nil) |
| `a` | array | `[]any` |
| `o` | object | Map types (`*OrderedMap` or `map[string]any`) |
| `f` | function | Callable types |

---

# Section 5: Standard Library Specification

**Missing-argument errors (Rust port).** The Rust port raises **T0410** for a call with too few arguments, *except* `$not`, `$eval`, `$match`, `$formatNumber`, `$formatInteger` and `$parseInteger`, which raise **D3006**. `$boolean()` with no arguments does not error at all -- it falls back to the focus (arity is deliberately unenforced so HOF callbacks such as `$filter($boolean)`, which pass `value, index, array`, keep working). No conformance case exercises D3006, so nothing pins the Go codes; the per-function bullets below give the shipped Rust code.

## 5.1 Registration (`functions/register.go`)

### 5.1.1 Function Types

- **`BuiltinFunction`**: `func(args []any, focus any) (any, error)` -- plain function, no env access.
- **`EnvAwareBuiltin`**: `func(args []any, focus any, env *Environment) (any, error)` -- needs environment for HOF callbacks.
- **`*SignedBuiltin`**: `{Fn: BuiltinFunction, Sig: string}` -- plain function with a type signature that the evaluator validates at call sites.

### 5.1.2 Registration

`RegisterAll(env, evalFn)` (line 82) binds all functions:
- Most as plain `BuiltinFunction`.
- `$uppercase`, `$lowercase`: `*SignedBuiltin` with sig `"s-:s"`.
- `$match`, `$replace`, `$eval`, `$sort`, `$sift`, `$each`, `$map`, `$filter`, `$single`, `$reduce`: `EnvAwareBuiltin` (created via `makeFnXxx(evalFn)` closures).

### 5.1.3 HOF Callback Arity: `hofArgs` (`hof_funcs.go:16`)

For lambda callbacks, argument list is trimmed to declared parameter count:
- 0 params: `[]any{}`
- 1 param: `[]any{value}`
- 2 params: `[]any{value, index}`
- 3+ params: `[]any{value, index, array}`
For builtins: always `[]any{value}` only.

---

## 5.2 String Functions

### 5.2.1 `$string` (`string_funcs.go:17`)

- **Signature**: No formal signature (manually validated).
- **Parameters**: `(value [, prettify])` -- 0 to 2 args.
- **0 args**: Uses `focus`; functions return undefined.
- **1 arg**: Converts to string.
- **2 args**: Second arg must be boolean (`prettify`). Functions as second arg: **D3011**. Other types: **T0410**.
- **Coercion** (`valueToString`, line 49):
  - `Null` -> `"null"`
  - `string` -> identity
  - `json.Number` -> `FormatNumber`
  - `float64`: `Inf`/`NaN` -> **D3001** ("Number out of range"). Otherwise `FormatFloat`.
  - `bool` -> `"true"` / `"false"`
  - Functions -> `""` (empty string)
  - Objects/arrays -> JSON marshal (no HTML escaping). `prettify=true` adds 2-space indentation. Functions within objects/arrays are sanitized to `""`.
- **Error codes**: D3001, D3011, T0410.

### 5.2.2 `$length` (`string_funcs.go:126`)

- **Parameters**: `(string)` -- exactly 1 arg.
- **0 args**: **T0411** ("argument 1 is required").
- **>1 args**: **T0410**.
- **nil arg**: undefined propagation.
- **Non-string**: **T0410**.
- **Return**: `float64` Unicode rune count (`utf8.RuneCountInString`).

### 5.2.3 `$substring` (`string_funcs.go:145`)

- **Parameters**: `(string, start [, length])` -- 2 or 3 args.
- **<2 args**: **T0410** (Go reference: D3006).
- **>3 args**: **T0410**.
- **nil first arg**: undefined propagation.
- **start**: float64, negative values count from end (clamped to 0).
- **length**: if < 0, returns `""`. If absent, returns from start to end.
- **Operates on runes** (Unicode-aware).

### 5.2.4 `$substringBefore` / `$substringAfter` (`string_funcs.go:200`)

- **Parameters**: `(string, separator)` -- 2 args. Can be called with 1 arg using context/focus.
- **Context mode** (1 arg): Uses focus as string, arg as separator.
- **nil string**: undefined propagation.
- **Non-string args**: **T0410** or **T0411** (context mode).
- **Separator not found**: returns original string unchanged.

### 5.2.5 `$uppercase` (`string_funcs.go:247`)

- **Signature**: `s-:s` (registered as SignedBuiltin).
- **Parameters**: `(string)` -- 0 or 1 arg.
- **0 args**: Uses focus.
- **nil**: undefined propagation.
- **Non-string**: **T0410**.
- **Return**: `strings.ToUpper(s)`.

### 5.2.6 `$lowercase` (`string_funcs.go:264`)

- **Signature**: `s-:s` (registered as SignedBuiltin).
- Same behavior as `$uppercase` but with `strings.ToLower`.

### 5.2.7 `$trim` (`string_funcs.go:281`)

- **Parameters**: `(string)` -- 0 or 1 arg.
- **nil**: undefined propagation.
- **Non-string**: **T0410**.
- **Behavior**: `strings.Join(strings.Fields(s), " ")` -- splits on whitespace, rejoins with single space. Trims leading/trailing and collapses internal whitespace.

### 5.2.8 `$pad` (`string_funcs.go:300`)

- **Parameters**: `(string, width [, char])` -- 2 or 3 args.
- **<2 args**: **T0410** (Go reference: D3006).
- **nil first arg**: undefined propagation.
- **width**: positive = right-pad, negative = left-pad. `|width| > 10,000` is rejected with **D3010** (matches the Go reference).
- **char**: padding character string (default `" "`). Empty string treated as `" "`. Repeats cyclically for multi-char pad strings.
- **Operates on runes** (Unicode-aware).

### 5.2.9 `$contains` (`string_funcs.go:356`)

- **Parameters**: `(string, pattern)` -- 2 args. Can use focus prepend (1 arg + focus).
- **nil first arg**: undefined propagation.
- **Array auto-mapping**: If first arg is array, maps `$contains` over string elements; returns true if any match.
- **String pattern**: `strings.Contains`.
- **Regex pattern** (map with `pattern` key): Compiles regex, tests via `MatchString`.
- **Error codes**: T0410 (bad arity/types), D3010 (invalid regex argument).

### 5.2.10 `$split` (`string_funcs.go:418`)

- **Parameters**: `(string, separator [, limit])` -- 2 or 3 args.
- **nil first arg**: undefined propagation.
- **limit**: non-negative integer. Negative -> **D3020**.
- **String separator**: `strings.SplitN` (respects limit).
- **Regex separator**: Custom `splitRegex` function.
- **Function as separator**: **T1010**.
- **Returns**: `[]any` of strings.

### 5.2.11 `$join` (`string_funcs.go:484`)

- **Parameters**: `(array [, separator])` -- 1 or 2 args.
- **0 args**: **T0410**.
- **nil first arg**: undefined propagation.
- **Single string input**: returns unchanged.
- **Array**: All elements must be strings (**T0412** if not).
- **separator**: string, default `""`.
- **Returns**: `strings.Join`.

### 5.2.12 `$match` (`string_match_replace.go:14`) -- EnvAwareBuiltin

- **Parameters**: `(string, pattern [, limit])`.
- **<2 args**: **D3006**.
- **nil first arg**: undefined propagation.
- **Non-string first arg**: **T0410**.
- **Pattern**: regex map or function (custom matcher).
- **limit**: numeric, limits result count.
- **Match object**: `{"match": text, "start": runeIndex, "end": runeIndex, "groups": [strings]}`.
- **Custom matcher**: Called with `(string, 0)`. Returns map with `match`, `start`, `groups`, `next` (function). Iterates by calling `next` until nil.
- **Returns**: `*Sequence` of match objects, or nil.
- **Error codes**: D3006, T0410, D3137.

### 5.2.13 `$replace` (`string_match_replace.go:131`) -- EnvAwareBuiltin

- **Parameters**: `(string, pattern, replacement [, limit])`.
- **nil first arg**: undefined propagation.
- **pattern**: string or regex map. Empty string pattern: **D3010**.
- **replacement**: string (with `$N` back-references) or function.
  - Back-references: `$0` = full match, `$1`-`$N` = groups. `$$` = literal `$`. Invalid group numbers use greedy-left prefix matching.
  - Function replacement: Called with match object, must return string (**D3012** if not).
- **limit**: non-negative number. Negative: **D3011**.
- **Zero-length regex match**: **D1004**.
- **Error codes**: T0410, D3010, D3011, D3012, D1004, D3137.

### 5.2.14 `$eval` (`string_encoding.go:16`) -- EnvAwareBuiltin

- **Parameters**: `(expression [, context])`.
- **0 args**: **D3006**.
- **nil first arg**: undefined propagation.
- **Non-string**: **T0410**.
- **Max eval depth**: 5 (nested `$eval` calls).
- **Parse error**: **D3120**.
- **Runtime T1005/T1006 errors**: Wrapped as **D3121**.
- Evaluates in child environment.

### 5.2.15 `$base64encode` (`string_encoding.go:63`)

- **Parameters**: `(string)`.
- **nil/0 args**: undefined propagation.
- **Non-string**: **T0410**.
- **Returns**: Standard base64 encoded string.

### 5.2.16 `$base64decode` (`string_encoding.go:74`)

- **Parameters**: `(string)`.
- **nil/0 args**: undefined propagation.
- **Non-string**: **T0410**.
- Tries standard base64 first, then URL-safe base64.
- **Decode error**: **D3137**.

### 5.2.17 `$encodeUrl` (`string_encoding.go:98`)

- **Parameters**: `(string)`.
- **nil/0 args**: undefined propagation.
- **Non-string**: **T0410**.
- **Lone surrogates**: **D3140**.
- Safe characters: `A-Za-z0-9-_.!~*'();/?:@&=+$,#`.

### 5.2.18 `$encodeUrlComponent` (`string_encoding.go:112`)

- Same as `$encodeUrl` but narrower safe set: `A-Za-z0-9-_.!~*'()`.

### 5.2.19 `$decodeUrl` / `$decodeUrlComponent` (`string_encoding.go:152`, `167`)

- **Parameters**: `(string)`.
- Uses `url.PathUnescape`.
- **Decode error**: **D3137**.

### 5.2.20 `$formatNumber` (`string_format_number.go:15`)

- **Parameters**: `(number, picture [, options])`.
- **<2 args**: **D3006**.
- **nil first arg**: undefined propagation.
- **options**: Object with keys: `decimal-separator`, `grouping-separator`, `percent`, `per-mille`, `zero-digit`, `digit`, `pattern-separator`, `exponent-separator`.
- **Picture syntax**: XPath 3.1 `format-number` compatible.
- **Sub-pictures**: Separated by pattern-separator (default `;`). Max 2 sub-pictures (**D3080**).
- **Error codes**: D3006, T0410, D3080-D3093 (picture validation errors).

### 5.2.21 `$formatBase` (`string_format_integer.go:16`)

- **Parameters**: `(number [, radix])`.
- **<1 arg**: **T0410** (Go reference: D3006).
- **nil**: undefined propagation.
- **radix**: 2-36, default 10. Out of range: **D3100**.
- **Returns**: `strconv.FormatInt(int64(math.Round(n)), base)`.

### 5.2.22 `$formatInteger` (`string_format_integer.go:47`)

- **Parameters**: `(number, picture)`.
- **<2 args**: **D3006**.
- **nil first arg**: undefined propagation.
- **Picture formats**:
  - `"w"` / `"W"` / `"Ww"`: Words (lowercase/uppercase/title case). Modifier `;o` for ordinals.
  - `"i"` / `"I"`: Roman numerals (lower/upper).
  - Single letter `a-z` / `A-Z`: Alphabetic representation.
  - Digit patterns (`0`, `01`, `001`): Minimum width. `#` for optional digits.
  - Grouping via separators in pattern.
  - Modifier `;o`: Ordinal suffix (st, nd, rd, th).
- **Unicode digit families**: Supports Arabic-Indic, Devanagari, and 35+ other numeral systems.
- **Error codes**: D3006, T0410, D3130 (unsupported picture), D3131 (mixed digit families), D3137 (too large).

### 5.2.23 `$parseInteger` (`string_format_integer.go:578`)

- **Parameters**: `(string, picture)`.
- **<2 args**: **D3006**.
- **nil first arg**: undefined propagation.
- **Picture formats**: `"w"`/`"W"`/`"Ww"` (word parsing), `"i"`/`"I"` (Roman), `"a"`/`"A"` (alphabetic), decimal with digit patterns.
- Supports ordinal words ("first", "twenty-third", etc.).
- **Error codes**: D3006, T0410, D3130, D3137.

---

## 5.3 Numeric Functions

### 5.3.1 `$number` (`numeric_funcs.go:16`)

- **Parameters**: `(value)` -- 0 or 1 arg.
- **0 args**: Uses focus.
- **>1 args**: **T0410**.
- **nil**: undefined propagation.
- **Null**: **T0410** ("cannot cast null to number").
- **Coercion**:
  - `float64`: identity.
  - `json.Number`: parses to float64. Parse error: **D3030**.
  - `string`: Trims whitespace. Supports `0x`/`0X` (hex), `0b`/`0B` (binary), `0o`/`0O` (octal) prefixes. Standard float parsing. `Inf`/`NaN`: **D3030**.
  - `bool`: `true` -> `1.0`, `false` -> `0.0`.
  - `[]any`: **T0410** ("cannot cast array").
  - Objects: **T0410** ("cannot cast object").

### 5.3.2 `$abs` (`numeric_funcs.go:84`)

- **Parameters**: `(number)`.
- **nil**: undefined propagation.
- **Non-number**: **T0410**.
- **Returns**: `math.Abs(n)`.

### 5.3.3 `$floor` (`numeric_funcs.go:97`)

- **Parameters**: `(number)`.
- **Returns**: `math.Floor(n)`.

### 5.3.4 `$ceil` (`numeric_funcs.go:110`)

- **Parameters**: `(number)`.
- **Returns**: `math.Ceil(n)`.

### 5.3.5 `$round` (`numeric_funcs.go:122`)

- **Parameters**: `(number [, precision])`.
- **0 args**: **T0410**.
- **nil first arg**: undefined propagation.
- **precision**: integer, default 0. Negative precision rounds to powers of 10.
- **Rounding**: **Bankers rounding** (round half to even) via `bankersRound` (line 148).
  - Works from shortest decimal string representation to avoid IEEE 754 artifacts.
  - For exactly 0.5: checks last kept digit; if odd, rounds up; if even, rounds down.
- **Error codes**: T0410.

### 5.3.6 `$power` (`numeric_funcs.go:249`)

- **Parameters**: `(base, exponent)`.
- **<2 args**: **T0410**.
- **nil**: undefined propagation.
- **Result `Inf`/`NaN`**: **D3061**.

### 5.3.7 `$sqrt` (`numeric_funcs.go:270`)

- **Parameters**: `(number)`.
- **Negative**: **D3060**.

### 5.3.8 `$random` (`numeric_funcs.go:286`)

- **Parameters**: none.
- **Returns**: `rand.Float64()` -- [0, 1).

### 5.3.9 `$sum` (`numeric_funcs.go:292`)

- **Parameters**: `(array)` -- exactly 1 arg.
- **0/2+ args**: **T0410**.
- **nil**: undefined propagation.
- **Non-array/non-number input**: **T0412**.
- **Empty array**: `0.0`.
- **Non-number element**: **T0412**.

### 5.3.10 `$max` (`numeric_funcs.go:322`)

- **Parameters**: `(array)` -- exactly 1 arg.
- **Empty array**: `nil` (undefined).
- Same type validation as `$sum`.

### 5.3.11 `$min` (`numeric_funcs.go:354`)

- Same as `$max` but returns minimum.

### 5.3.12 `$average` (`numeric_funcs.go:386`)

- **Parameters**: `(array)` -- exactly 1 arg.
- **Empty array**: `nil` (undefined).
- **Returns**: `sum / float64(len)`.
- Same type validation as `$sum`.

---

## 5.4 Array Functions

### 5.4.1 `$count` (`array_funcs.go:14`)

- **Parameters**: `(value)` -- 0 or 1 arg.
- **>1 args**: **T0410**.
- **nil/0 args**: `0.0`.
- **`[]any`**: `float64(len(v))`.
- **Other**: `1.0`.

### 5.4.2 `$append` (`array_funcs.go:31`)

- **Parameters**: `(array1, array2)`.
- **<2 args**: **T0410** (Go reference: D3006).
- **nil arg**: Returns the other arg unchanged.
- **Max result size**: no cap in the Rust port (`crates/jsntrs/src/stdlib/array.rs:24-46`). Go reference: results over 10,000,000 elements raise **D3010**.
- **Returns**: Concatenation of both arrays (wrapped via `wrapArray`).

### 5.4.3 `$sort` (`array_funcs.go:83`) -- EnvAwareBuiltin

- **Parameters**: `(array [, comparator])` -- flexible arity with focus.
- **0 args**: Uses focus as array.
- **1 arg**: If function, uses focus as array + arg as comparator. Otherwise, arg as array.
- **2 args**: arg[0] = array, arg[1] = comparator.
- **nil array**: undefined propagation.
- **Without comparator**: Default sort. All elements must be same type (all numbers or all strings). Mixed: **D3070**.
- **With comparator**: `fn(a, b)` returns true when a should come before b. Internally reverses args: calls `fn(b, a)` and maps `true -> -1`.
- **Stable sort** (preserves insertion order for equal elements).

### 5.4.4 `$reverse` (`array_funcs.go:178`)

- **Parameters**: `(array)`.
- **nil/0 args**: undefined propagation.
- **Returns**: Reversed clone.

### 5.4.5 `$shuffle` (`array_funcs.go:190`)

- **Parameters**: `(array)`.
- **nil/0 args**: undefined propagation.
- **Returns**: Randomly shuffled clone (`rand.Shuffle`).

### 5.4.6 `$distinct` (`array_funcs.go:204`)

- **Parameters**: `(array)`.
- **nil/0 args**: undefined propagation.
- **Null input**: Returns `Null`.
- **Non-array**: Returns input unchanged.
- **Single-element or empty array**: Returns unchanged (no dedup needed).
- **Dedup logic**: Primitive types use map-based dedup. Complex types (objects, arrays) use `DeepEqual` comparison. `json.Number` normalized to float64 key.
- **Returns**: `*Sequence` (subject to singleton collapse for multi-element input that deduped to 1).

### 5.4.7 `$flatten` (`array_funcs.go:272`)

- **Parameters**: `(array [, depth])`.
- **nil/0 args**: undefined propagation.
- **depth**: numeric, default unlimited (-1). Flattens nested arrays to specified depth.
- **Returns**: Flattened `[]any`.

### 5.4.8 `$zip` (`array_funcs.go:308`)

- **Parameters**: `(array1 [, array2, ...])` -- variadic.
- **0 args**: `[]any{}`.
- **nil args**: Treated as empty arrays.
- **Returns**: Array of tuples, length = shortest input array. Each tuple contains corresponding elements from each input array.

---

## 5.5 Object Functions

### 5.5.1 `$keys` (`object_funcs.go:13`)

- **Parameters**: `(object)`.
- **nil/0 args**: undefined propagation.
- **`*OrderedMap`**: Returns keys in insertion order.
- **`map[string]any`**: Returns keys in **sorted** order.
- **`[]any`**: Collects unique keys from all map elements (preserves first-seen order).
- **Returns**: `*Sequence` of key strings.

### 5.5.2 `$values` (`object_funcs.go:60`)

- **Parameters**: `(object)`.
- **nil/0 args**: undefined propagation.
- **`*OrderedMap`**: Returns values in insertion order.
- **`map[string]any`**: Returns values in sorted key order.
- **`[]any`**: Collects values from all map elements.

### 5.5.3 `$spread` (`object_funcs.go:100`)

- **Parameters**: `(object)`.
- **nil/0 args**: undefined propagation.
- **Map input**: Returns `*Sequence` of single-key `*OrderedMap` objects.
- **Array input**: Spreads each map element; non-map elements passed through.
- **Non-map scalar**: Returns input unchanged.

### 5.5.4 `$merge` (`object_funcs.go:137`)

- **Parameters**: `(array)`.
- **nil/0 args**: undefined propagation.
- **Single map (not array)**: Returns unchanged.
- **Non-array/non-map**: **T0410**.
- **Non-map array element**: **T0412**.
- **Returns**: Single `*OrderedMap` with all keys. Later values overwrite earlier for same key.

### 5.5.5 `$lookup` (`object_funcs.go:278`)

- **Parameters**: `(object, key)`.
- **<2 args**: **T0410** (Go reference: D3006).
- **nil args**: undefined propagation.
- **Non-string key**: **T0410**.
- **Map input**: Returns value for key, or nil if not found.
- **Array input**: Collects values from all map elements that have the key. Singleton collapse applies.

### 5.5.6 `$error` (`object_funcs.go:261`)

- **Parameters**: `([message])` -- 0 or 1 arg.
- **>1 args**: **T0410**.
- **Non-string arg**: **T0410**.
- **Default message**: "an error was thrown".
- **Always returns**: **D3137** error.

### 5.5.7 `$sift` (`object_funcs.go:179`) -- EnvAwareBuiltin

- **Parameters**: `(object, function)` or `(function)` with focus.
- **0 args**: **T0410** (Go reference: D3006).
- **nil object**: undefined propagation.
- **Non-map**: **T0410**.
- **Callback arity** (`siftArgs`, line 161): Lambda param count determines args:
  - 0 params: `[]`
  - 1 param: `[value]`
  - 2 params: `[value, key]`
  - 3+ params: `[value, key, object]`
  Builtin callbacks get `[value]` only, as in 5.1.3 -- a one-argument builtin
  such as `$exists` would otherwise reject the key with **T0410**
  (jsonata-js: `$sift(obj, $exists)` returns `obj`).
- **Returns**: `*OrderedMap` of key-value pairs where predicate is truthy. Empty result returns nil.

### 5.5.8 `$each` (`object_funcs.go:222`) -- EnvAwareBuiltin

- **Parameters**: `(object, function)` or `(function)` with focus.
- **0 args**: **T0410** (Go reference: D3006).
- **nil object**: undefined propagation.
- **Non-map**: **T0410**.
- **Callback arity**: same trimming as `$sift` above (`(value, key, object)`
  cut to the callback's declared parameter count; builtins get `[value]`).
  **Deviation from the Go reference**, which always called back with exactly
  `(value, key)`: jsonata-js routes `$each` through `hofFuncArgs` like every
  other HOF, so `$each(obj, $exists)` succeeds and
  `$each(obj, function($v,$k,$o){...})` sees the whole object in `$o`. jsntrs
  follows jsonata-js (jsntrs-p0v.2).
- **Returns**: `*Sequence` of callback results.

---

## 5.6 Higher-Order Functions (HOF)

### 5.6.1 `$map` (`hof_funcs.go:35`) -- EnvAwareBuiltin

- **Parameters**: `(array, function)` or `(function)` with focus.
- **0 args**: **T0410** (Go reference: D3006).
- **1 arg function**: Uses focus as array.
- **nil array (2-arg form)**: undefined propagation.
- **nil array (1-arg focus form)**: **T0410** ("array argument is undefined").
- **Callback**: Called with HOF arity trimming (value [, index [, array]]).
- **Returns**: Collapsed sequence of callback results. Nil callback results are excluded.

### 5.6.2 `$filter` (`hof_funcs.go:75`) -- EnvAwareBuiltin

- **Parameters**: `(array, function)` or `(function)` with focus.
- **nil array**: undefined propagation.
- **Callback**: Called with HOF arity trimming. Keeps items where callback is truthy.
- **Array input**: Returns `[]any` (preserves array type). Empty filter result returns nil.
- **Non-array input**: Returns collapsed sequence.

### 5.6.3 `$single` (`hof_funcs.go:121`) -- EnvAwareBuiltin

- **Parameters**: `(array [, function])` or `(function)` with focus. Or `()` with focus.
- **0 args/no function**: Returns single element. 0 items: **D3139**. >1 items: **D3138**.
- **With function predicate**: Filters first, then expects exactly 1 match. 0 matches: **D3139**. >1 matches: **D3138**.
- **nil array**: undefined propagation.

### 5.6.4 `$reduce` (`hof_funcs.go:176`) -- EnvAwareBuiltin

- **Parameters**: `(array, function [, init])` or `(function)` with focus.
- **0 args**: **T0410** (Go reference: D3006).
- **nil array**: undefined propagation.
- **Lambda with <2 params**: **D3050** ("must have arity of at least 2").
- **Empty array with init**: Returns init.
- **Empty array without init**: Returns nil.
- **Callback arity** (for lambdas):
  - 0-1 params: `[acc]`
  - 2 params: `[acc, value]`
  - 3 params: `[acc, value, index]`
  - 4+ params: `[acc, value, index, array]`
- For builtins: `[acc, value]`.

---

## 5.7 Boolean Functions

### 5.7.1 `$boolean` (`boolean_funcs.go:7`)

- **Parameters**: `(value)` -- nominally 1 arg.
- **0 args**: no error -- uses the focus (`crates/jsntrs/src/stdlib/boolean.rs:10-17`). Go reference: **D3006**.
- **>1 args**: no error -- extra arguments are ignored. Arity is deliberately unenforced so HOF callbacks such as `$filter($boolean)`, which pass `(value, index, array)`, keep working. Go reference: **T0410**.
- **nil**: undefined propagation.
- **Returns**: `ToBoolean(args[0])` -- see Section 4.2.10 for coercion rules.

### 5.7.2 `$not` (`boolean_funcs.go:22`)

- **Parameters**: `(value)`.
- **0 args**: **D3006**.
- **nil**: undefined propagation.
- **Returns**: `!ToBoolean(args[0])`.

### 5.7.3 `$exists` (`boolean_funcs.go:34`)

- **Parameters**: `(value)` -- exactly 1 arg.
- **0 args**: **T0410**.
- **>1 args**: **T0410**.
- **Returns**: `args[0] != nil` (boolean). Does NOT propagate undefined.

---

## 5.8 DateTime Functions

### 5.8.1 `$now` (`datetime_funcs.go:10`)

- **Parameters**: `([picture [, timezone]])`.
- **0 args**: Returns an ISO 8601 UTC timestamp with milliseconds, `YYYY-MM-DDTHH:MM:SS.sssZ` (JSONata-spec format). Go reference: RFC 3339 Nano.
- **With picture**: Formats current time using XPath picture string.
- **Non-string picture**: **T0410**.
- **timezone**: Named timezone or numeric offset (e.g., `"+05:30"`, `"America/New_York"`).
- **Unknown timezone**: **D3137**.

### 5.8.2 `$millis` (`datetime_funcs.go:34`)

- **Parameters**: none.
- **Returns**: `float64` Unix milliseconds of current time.

### 5.8.3 `$fromMillis` (`datetime_funcs.go:38`)

- **Parameters**: `(millis [, picture [, timezone]])`.
- **0 args**: Uses focus, or returns nil.
- **nil first arg**: undefined propagation.
- **Non-number**: **T0410**.
- **No picture**: Returns ISO 8601 with milliseconds and timezone offset (via `formatDefaultISO`, line 81). UTC offset renders as `"Z"`.
- **With picture**: Formats via `formatWithPicture`.
- **Non-string picture**: **T0410**.
- **timezone**: Same as `$now`.

### 5.8.4 `$toMillis` (`datetime_funcs.go:102`)

- **Parameters**: `(string [, picture])`.
- **nil/0 args**: undefined propagation.
- **Non-string**: **T0410**.
- **Without picture**: Tries ISO 8601 formats in order (RFC 3339 Nano, RFC 3339, various offset formats, date-only, year-only). Parse failure: **D3110**.
- **With picture**: Uses `parseWithPicture` from `datetime_parse.go`.
- **Non-string picture**: **T0410**.
- **Returns**: `float64` Unix milliseconds.

### 5.8.5 DateTime Formatting (`datetime_format.go`)

`formatWithPicture(t, picture)` (line 68):

**Pre-scan** (line 70): Checks for unclosed `[...` brackets -> **D3135**.

**Token parsing**:
- `[[` -> literal `[`; `]]` -> literal `]`.
- `[component modifier]` -> formatted value.
- Unknown component: **D3134**.
- Unsupported modifier: **D3133** (e.g., `[YN]`).

**Components** (line 135):

| Component | Description | Line |
|-----------|-------------|------|
| `Y` | Calendar year | `formatYearComponent` line 185 |
| `M` | Month (1-12 or name) | `formatMonthToken` line 377 |
| `D` | Day of month | `formatDayComponent` line 202 |
| `d` | Day of year | `formatDayOfYearToken` line 269 |
| `H` | Hour (0-23) | line 148 |
| `h` | Hour (1-12) | line 150 |
| `m` | Minute (00-59, default `01` format) | line 152 |
| `s` | Second (00-59, default `01` format) | line 155 |
| `f` | Fractional seconds (milliseconds) | `formatFracSecond` line 221 |
| `F` | Day of week (name or number) | `formatWeekdayToken` line 233 |
| `Z` | Timezone offset | `formatTimezone` line 441 |
| `z` | Timezone offset with GMT prefix | `formatTimezone` line 441 |
| `P` | AM/PM | `formatAMPM` line 258 |
| `W` | ISO week number | line 173 |
| `w` | Week of month | line 178 |
| `X` | ISO week-based year | line 175 |
| `x` | ISO week-based month | line 180 |
| `E`, `C` | Calendar/era -> "ISO" | line 168 |

**Modifiers**:
- Numeric: `01` (2-digit zero-padded), `1` (no padding), `001` (3-digit), etc.
- Named: `N` (uppercase), `n` (lowercase), `Nn` (title case). Truncation via `,n` or `,n-m`.
- Ordinal: `o` suffix (e.g., `Do` -> "1st", "2nd").
- Words: `w` (lowercase), `W` (uppercase), `Ww` (title case).
- Roman: `I` (uppercase), `i` (lowercase).
- Alphabetic: `A` (uppercase), `a` (lowercase).
- Year truncation: `Y,2` -> last 2 digits.

### 5.8.6 DateTime Parsing (`datetime_parse.go`)

`parseWithPicture(input, picture)` (line 29):

**Validation** (lines 80-130):
- **D3132**: Unknown picture component.
- **D3133**: Unsupported modifier (e.g., `[YN]`).
- **D3136**: Underspecified picture:
  - Day without month (and no day-of-year).
  - Minutes/seconds without hour.
  - Week-based year without calendar year.

**Parsing** (lines 133-269):
- Consumes literal characters from input (case-insensitive matching).
- Parses numeric, name-based (months, weekdays), word-based, Roman, alphabetic values.
- 12-hour clock adjustment: PM + hour!=12 -> +12; AM + hour==12 -> 0.
- Time-only pictures: fills in today's date.
- Day-of-year: converts via `time.Date(year, 1, 1, ...).AddDate(0, 0, dayOfYear-1)`.
- Timezone: parsed offset subtracted from local time to get UTC.

---

## 5.9 Misc Functions

### 5.9.1 `$assert` (`hof_funcs.go:249`)

- **Parameters**: `(condition [, message])`.
- **0 args**: **T0410**.
- **>2 args**: **T0410**.
- **Non-boolean first arg**: **T0410**.
- **Assertion failure**: **D3141** with message (default "assertion failed").
- **Success**: returns nil.

### 5.9.2 `$type` (`hof_funcs.go:273`)

- **Parameters**: `(value)`.
- **nil/0 args**: undefined (nil).
- **Null**: `"null"`.
- **Returns**:

| Input Type | Result |
|-----------|--------|
| `float64`, `json.Number` | `"number"` |
| `string` | `"string"` |
| `bool` | `"boolean"` |
| `[]any` | `"array"` |
| `*OrderedMap`, `map[string]any` | `"object"` |
| `BuiltinFunction`, `EnvAwareBuiltin`, `*Lambda`, `*SignedBuiltin` | `"function"` |
| Other | nil (undefined) |

---

# Section 6: Fast-Path System

> **Go reference implementation.** The struct layouts, the GJSON tiering in 6.3-6.4 and the API table in 6.6 describe the Go engine. The Rust port keeps the same three-way classification (pure path / comparison / function, `fast_path::analyze`) but not the machinery below.
>
> **Rust port (shipped behavior).** There is no GJSON tier -- the crate has no GJSON-equivalent dependency. Fast paths run either over a raw JSON byte tape (`fast_path::eval_tape_path`, `crates/jsntrs/src/fast_path.rs:843`) or over an already-built `Value` (`fast_path::eval_fast`, `fast_path.rs:322`); both are dispatched from `crates/jsntrs/src/expression.rs:108,127,145,173` and fall back to full evaluation when they return `None`.
>
> Public API (`crates/jsntrs/src/expression.rs:78-258`): `Expression::compile`, `evaluate`, `evaluate_value`, `evaluate_bytes`, `evaluate_with_vars`, `evaluate_with_custom_funcs`, `evaluate_with_cancel`, `is_fast_path`, `fast_path_info`. Helpers live on `Value` (`from_json_str`, `is_null`, `deep_equal`). The Go names `EvalBytes`, `NormalizeValue`, `DecodeJSON`, `IsNull`, `DeepEqual`, `IsFuncFastPath` and `IsComparisonFastPath` have no Rust counterpart.

## 6.1 Expression Struct

**File:** `gnata.go`, lines 26-38

```go
type Expression struct {
    src      string
    ast      *parser.Node
    fastPath bool                          // pure-path eligible
    paths    []string                      // GJSON path strings
    cmpFast  *parser.ComparisonFastPath    // comparison optimization
    funcFast *parser.FuncFastPath          // function call optimization
}
```

Three fast-path flags are mutually exclusive: `AnalyzeFastPath` returns only one as non-zero.

## 6.2 Compile Function

**File:** `gnata.go`, lines 42-61

Four-stage pipeline:
1. **Lex + Parse**: `parser.NewParser(expr).Parse()` → raw AST
2. **ProcessAST**: Post-parse transformations (path flattening, tail-call marking)
3. **AnalyzeFastPath**: Classifies for fast-path eligibility (read-only)
4. **Construct**: Populates `Expression` from analysis result

## 6.3 Three Evaluation Tiers in EvalBytes

**File:** `gnata.go`, lines 200-224

Cascade of three fast-path tiers with fallback:

**Tier 1: Pure-Path** (lines 202-208): `gjson.GetBytes(data, path)`. Falls through if GJSON cannot resolve (e.g., array requiring auto-mapping).

**Tier 2: Comparison** (lines 209-213): `evalComparison(cmpFast, data, nil)`. Returns `(result, handled, error)`. Falls through if `handled == false`.

**Tier 3: Function** (lines 214-218): `evalFunc(funcFast, data, nil)`. Same three-value return contract.

**Full Evaluation** (lines 219-223): `evaluator.DecodeJSON(data)` then `expr.Eval(ctx, v)`.

## 6.4 Fallback Conditions

1. GJSON result does not exist (path traverses JSON array requiring auto-mapping)
2. Comparison LHS is a JSON array (JSONata auto-maps element-wise; exception: null checks short-circuit)
3. Function handler returns `handled == false` (unsupported GJSON type)
4. No handler registered (e.g., `FuncFastRound` deliberately excluded)

## 6.5 FuncFastKind Handlers

**File:** `func_fast.go`

23 handlers in Go's `funcFastHandlers` map (`func_fast.go:49-73`); the Rust port covers 26 kinds (`FuncFastKind`, `crates/jsntrs/src/fast_path.rs:94-121`), adding `$values`, `$shuffle` and `$flatten` to the Go set. `$round` is excluded in both.

| Kind | Handler | Behavior |
|---|---|---|
| `$exists` | `evalFuncExists` | Always `(true, true, nil)` if path resolved |
| `$contains` | `evalFuncContains` | Substring check on strings; element check on arrays |
| `$string` | `evalFuncString` | Type conversion; falls through for objects/arrays |
| `$boolean` | `evalFuncBoolean` | Truthiness; falls through for compounds |
| `$number` | `evalFuncNumber` | Parse/convert; falls through on failure |
| `$keys` | `evalFuncKeys` | Object keys; singleton unwrap for 1 key |
| `$distinct` | `evalFuncDistinct` | Dedup scalars; falls through for complex elements |
| `$not` | `evalFuncNot` | Boolean negation |
| `$lowercase` | `evalFuncLowercase` | `strings.ToLower` on strings |
| `$uppercase` | `evalFuncUppercase` | `strings.ToUpper` on strings |
| `$trim` | `evalFuncTrim` | Collapse whitespace |
| `$length` | `evalFuncLength` | Unicode codepoint count |
| `$type` | `evalFuncType` | Type name string |
| `$abs`/`$floor`/`$ceil`/`$sqrt` | math functions | On numbers; sqrt falls through for negatives |
| `$count` | `evalFuncCount` | Array elements; scalar → 1 |
| `$reverse` | `evalFuncReverse` | Reverse array |
| `$sum`/`$max`/`$min`/`$average` | aggregate functions | On numeric arrays |

`$round` deliberately excluded (requires banker's rounding).

## 6.6 Public API

| Method | Description |
|---|---|
| `Compile(expr)` | Parse and compile |
| `Eval(ctx, data)` | Full AST evaluation |
| `EvalBytes(ctx, data)` | Three-tier fast-path + fallback |
| `EvalWithVars(ctx, data, vars)` | Eval with extra variable bindings |
| `EvalWithCustomFuncs(ctx, data, env)` | Eval with custom function environment |
| `IsFastPath()` / `IsFuncFastPath()` / `IsComparisonFastPath()` | Query optimization status |
| `NormalizeValue(v)` | Convert internal types to standard Go |
| `DecodeJSON(b)` | Parse JSON with UseNumber + OrderedMap |
| `IsNull(v)` | Test for null sentinel |
| `DeepEqual(a, b)` | Public equality (normalizes null to nil) |

---

# Section 7: StreamEvaluator & Concurrency

> **Go reference implementation only.** Everything in 7.1-7.9 below (COW expression list, `sync.Mutex`, schema-keyed `GroupPlan`/`BoundedCache`, four-method `MetricsHook`, concurrent `EvalMany`) describes the Go engine and was **not** ported.
>
> **Rust port (shipped behavior).** The Rust `StreamEvaluator` (`crates/jsntrs/src/stream.rs`) is single-threaded by design: "each JSON stream gets its own evaluator on its own thread. No locking, no atomic operations" (`stream.rs:1-4`). The struct (`stream.rs:38-42`) is just `exprs: Vec<Option<Expression>>`, an optional `metrics` hook and `custom_funcs` -- no COW snapshot pointer, no mutex, no `BoundedCache`, no `GroupPlan`, no schema-keyed caching. `MetricsHook` has a single method, `on_eval` (`stream.rs:15-25`), and `StreamStats` (`stream.rs:27-31`) exposes only an expression-slot count -- no hits/misses/evictions.
>
> Concurrency is achieved by giving each thread its own evaluator: `Value` is deliberately `!Send` (`crates/jsntrs/src/value.rs:57-63`), so concurrent evaluation inside one evaluator is impossible, while a compiled `Expression` is `Send + Sync` and cheap to clone -- compile once, share the `Expression`, build input `Value`s per thread.

## 7.1 StreamEvaluator Struct

**File:** `stream.go`, lines 16-28

```go
type StreamEvaluator struct {
    exprs     atomic.Pointer[[]*Expression]  // COW expression list
    mu        sync.Mutex                      // write serialization
    cache     *BoundedCache                   // schema-keyed GroupPlan cache
    metrics   MetricsHook                     // optional telemetry
    customEnv *evaluator.Environment          // custom functions
}
```

## 7.2 Constructor Options

| Option | Default | Effect |
|---|---|---|
| `WithPoolSize(n)` | 0 | Reserved for future context pooling |
| `WithMaxCachedSchemas(n)` | 10000 | BoundedCache capacity |
| `WithMetricsHook(hook)` | nil | Telemetry callbacks |
| `WithCustomFunctions(fns)` | nil | Register custom functions |

## 7.3 Expression Management

- **`Compile(src)`**: Parse + Add. Returns stable index.
- **`Add(expr)`**: Append to COW list, invalidate cache. Index is append-only.
- **`Replace(idx, expr)`**: Overwrite in-place, invalidate cache.
- **`Remove(idx)`**: Set slot to nil (index NOT reused), invalidate cache.
- **`Reset()`**: Clear all, invalidate cache.

All writes serialized by `mu`. Reads are lock-free.

## 7.4 EvalMany Pipeline

**File:** `stream.go`, lines 216-287

1. Lock-free snapshot load: `expressions := *se.exprs.Load()`
2. GroupPlan resolution (cache lookup or build)
3. Per-expression: try fast-paths (pure → comparison → function), fallback to full eval
4. Lazy JSON parsing on first full-eval in batch

## 7.5 GroupPlan

**File:** `bounded_cache.go`, lines 10-23

```go
type GroupPlan struct {
    FastPaths    []string                      // GJSON paths (nil if no pure-path exprs)
    ExprFastPath []bool                        // pure-path eligibility
    CmpFast      []*parser.ComparisonFastPath  // comparison paths (nil if none)
    FuncFast     []*parser.FuncFastPath        // function paths (nil if none)
}
```

Indexed by position within `exprIndices` array (not absolute expression index).

## 7.6 Schema-Keyed Caching

Cache key: `"<schemaKey>|<idx0>,<idx1>,...,<idxN>"`. Different schemas or different index subsets produce different entries. All write operations invalidate the entire cache.

## 7.7 BoundedCache

**File:** `bounded_cache.go`

FIFO ring-buffer with lock-free reads:

- **`Get(key)`**: Atomic snapshot load → O(1) map lookup. No mutex.
- **`Set(key, plan)`**: Mutex-protected. Creates new snapshot (entries slice + index map), publishes atomically. FIFO eviction when at capacity.
- **`Invalidate()`**: Mutex-protected. Zeros ring buffer, stores empty snapshot.

Stats: `hits`, `misses`, `evictions` via `atomic.Int64`; `entries` via mutex-protected `count`.

## 7.8 MetricsHook

```go
type MetricsHook interface {
    OnEval(exprIndex int, fastPath bool, duration time.Duration, err error)
    OnCacheHit(schemaKey string)
    OnCacheMiss(schemaKey string)
    OnEviction()
}
```

All call sites guarded by nil check. Implementations must be goroutine-safe.

## 7.9 Thread Safety Guarantees

| Operation | Concurrency | Mechanism |
|---|---|---|
| EvalMany/EvalOne/EvalMap | Fully concurrent | Lock-free snapshot of expressions + cache |
| Add/Compile/Replace/Remove/Reset | Serialized | `sync.Mutex` |
| Writes concurrent with reads | Safe | Copy-on-write: new slices published atomically |
| BoundedCache Get + Set | Safe | Atomic pointer snapshot pattern |
| Expression | Goroutine-safe | Immutable after Compile |
