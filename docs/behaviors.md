# Expected Behaviors Catalog

This document provides complete truth tables and edge case documentation for gnata's JSONata implementation. It serves as the definitive behavioral reference for the Rust port.

---

## 1. Type Coercion Rules

### 1.1 Boolean Coercion (`ToBoolean`)

Source: `internal/evaluator/value.go:146-177`

| Input | Type | Result | Notes |
|---|---|---|---|
| `nil` | undefined | `false` | |
| `Null` | null | `false` | |
| `true` | bool | `true` | |
| `false` | bool | `false` | |
| `""` | string | `false` | empty string |
| `"0"` | string | `true` | non-empty string (differs from JS!) |
| `"false"` | string | `true` | non-empty string |
| `"hello"` | string | `true` | |
| `0` | float64 | `false` | |
| `0.0` | float64 | `false` | |
| `-0` | float64 | `false` | Go treats -0.0 == 0.0 |
| `1` | float64 | `true` | |
| `-1` | float64 | `true` | |
| `NaN` | float64 | `true` | NaN != 0, so truthy |
| `json.Number("0")` | json.Number | `false` | |
| `json.Number("1")` | json.Number | `true` | |
| `{}` (empty) | OrderedMap | `false` | Len() == 0 |
| `{"a":1}` | OrderedMap | `true` | Len() > 0 |
| `{}` (empty) | map[string]any | `false` | len() == 0 |
| `[]` | []any | `false` | len 0 |
| `[false]` | []any | `false` | len 1 -> recurse on element |
| `[0]` | []any | `false` | len 1 -> recurse on element |
| `[""]` | []any | `false` | len 1 -> recurse on element |
| `[true]` | []any | `true` | len 1 -> recurse on element |
| `[false, false]` | []any | `false` | len > 1 -> ANY truthy |
| `[false, true]` | []any | `true` | len > 1 -> ANY truthy |
| `[0, 0]` | []any | `false` | |
| `[0, 1]` | []any | `true` | |
| `*Sequence` | Sequence | recurse | CollapseSequence then ToBoolean |

**The non-finite rows above describe gnata, not jsntrs** (jsntrs-qr9, wave 8).
`NaN` is listed as `true` on the reasoning "NaN != 0, so truthy", and `Infinity`
has no row at all. jsntrs answers neither: `$boolean(0/0)` is **`false`** and
`$boolean(1/0)` **raises D1001**. It reaches that by the reference's route —
`isNumeric()` is a *type test* that returns false for NaN (so NaN misses the
numeric branch entirely and falls through to the `false` default) and throws
D1001 for Infinity (jsonata 2.2.2 `jsonata.js:9770-9780`). The same test is why
`[1,2,3][1/0]` is fatal while `1/0 > 5` is `true`: only the first type-tests the
value. None of this is derivable from the documentation, whose whole statement
about `/` is that it "divides the RHS into the LHS to produce the numerical
quotient. It is an error if either operand is not a number" (/numeric-operators)
— nothing about a result that is not finite. The shape is pinned, with that
provenance in each case, in `testdata/groups/rust-subscript-numeric-index/`.

### 1.2 Number Coercion (`$number` function)

Source: `functions/numeric_funcs.go`

| Input | Type | Result | Notes |
|---|---|---|---|
| `true` | bool | `1` | |
| `false` | bool | `0` | |
| `"123"` | string | `123` | |
| `"12.5"` | string | `12.5` | |
| `"-3"` | string | `-3` | |
| `"1e5"` | string | `100000` | scientific notation |
| `"0x1F"` | string | `31` | hex prefix supported |
| `" 42 "` | string | `42` | leading/trailing whitespace tolerated |
| `"123abc"` | string | error D3030 | non-numeric suffix |
| `""` | string | error D3030 | empty string |
| `null` | null | error T0410 | type mismatch |
| `[]` | array | error T0410 | type mismatch |
| `{}` | object | error T0410 | type mismatch |
| `42` | number | `42` | identity |
| `nil` | undefined | undefined | returns nil (signature propagation) |

### 1.3 String Coercion (`stringifyValue`)

Source: `internal/evaluator/eval_helpers.go:27-49`

| Input | Type | Result | Notes |
|---|---|---|---|
| `nil` | undefined | `""` | empty string |
| `"hello"` | string | `"hello"` | identity |
| `42` | float64 | `"42"` | via FormatFloat |
| `3.14` | float64 | `"3.14"` | |
| `1e20` | float64 | `"100000000000000000000"` | decimal (< 1e21) |
| `1e21` | float64 | `"1e+21"` | scientific (>= 1e21) |
| `1e-7` | float64 | `"1e-7"` | scientific (< 5e-7) |
| `5e-7` | float64 | `"0.0000005"` | decimal (>= 5e-7) |
| `NaN` | float64 | error **D3001** | Go said `"null"`; see below |
| `Inf` | float64 | error **D3001** | Go said `"null"`; see below |
| `true` | bool | `"true"` | |
| `false` | bool | `"false"` | |
| `Null` | null | `"null"` | JSON marshaled |
| `[1,2]` | []any | `"[1,2]"` | JSON marshaled (no HTML escape) |
| `{"a":1}` | OrderedMap | `{"a":1}` | JSON marshaled, preserves key order |
| `[Inf]` | []any | error **D1001** | *contains* `Inf` — matches jsonata 2.2.2 |
| `{"a":Inf}` | OrderedMap | error **D1001** | *contains* `Inf` — matches jsonata 2.2.2 |
| `[NaN]` | []any | error **D1001** | **deviation** — jsonata 2.2.2 gives `"[null]"` |
| `{"a":NaN}` | OrderedMap | error **D1001** | **deviation** — jsonata 2.2.2 gives `{"a":null}` |

**Non-finite numbers (corrected against jsonata 2.2.2, jsntrs-x0y).** The Go
reference formatted `Inf`/`NaN` as `"null"` here, but jsonata-js defines both
`$string` and `&` through one `string()` function
(`jsonata.js:1484-1490`) that throws **D3001** on a *bare* non-finite number —
`NaN` and `±Infinity` alike, since the guard is `!isFinite(arg)`. jsntrs
follows it: `1/0 & ''`, `(0/0) & ''`, `$string(1/0)` and `$string(0/0)` are
all D3001. The split is implemented once, in
`Value::stringify`/`stringify_into`, so the operator and the function cannot
disagree.

A *composite* that merely contains a non-finite number takes the
`JSON.stringify` path instead, and there the two engines part company —
**a documented, deliberate deviation, not a bug to be read as spec**:

| composite member | jsonata 2.2.2 | jsntrs |
|---|---|---|
| `Infinity` / `-Infinity` | **D1001** | **D1001** |
| `NaN` | member serialized as `null` (`"[null]"`) | **D1001** |

The reference's replacer calls `isNumeric` on every value, and `isNumeric`
(`jsonata.js:7491-7503`) returns `false` for `NaN` *without* throwing —
`isNum = !isNaN(n)` is already false, so the `!isFinite` throw is never
reached — leaving `JSON.stringify`'s own rule (non-finite numbers serialize
as `null`) to produce `"[null]"`. Only `Infinity` reaches the throw, which is
why the reference's composite rule is about `Infinity` alone. jsntrs's
`contains_non_finite` does not make that distinction and raises D1001 for
either. The behaviour is deliberately left as-is: it is the older, stricter
rule inherited from the port, refusing to emit a `null` that silently
destroys a NaN, and no conformance case in the suite depends on the
reference's answer. Anything relying on `$string`/`&` of a NaN-bearing
composite must expect D1001 here.

### 1.4 Number Formatting (`FormatFloat`)

Source: `internal/evaluator/eval_helpers.go:72-86`

Must match JavaScript's `Number.toString()` exactly. Rules:

| Condition | Format | Example |
|---|---|---|
| `NaN` or `Inf` | `"null"` | `NaN` -> `"null"` |
| `abs(n) == 0` | `"0"` | `0.0` -> `"0"` |
| `5e-7 <= abs(n) < 1e21` | decimal | `0.0000005` -> `"0.0000005"` |
| `abs(n) < 5e-7` | scientific | `1e-8` -> `"1e-8"` |
| `abs(n) >= 1e21` | scientific | `1e21` -> `"1e+21"` |
| scientific exponent | cleaned | `"1e+21"` (no leading zeros in exponent) |

Numbers have a **single** representation, the `Value::Number(f64)` variant in `crates/jsntrs/src/value.rs`. JSON input is converted to f64 at parse time (`Value::from_json`, via `n.as_f64()`), so integers beyond 2^53 are not preserved verbatim -- extra digits are lost on ingest. JSON output goes through `ryu-js` (`Value::write_json`), and `&`/`$string` coercion through `format_float` (`value::format`, re-exported from `value.rs`), which applies the 15-significant-digit cast first.

*Go reference:* the Go engine kept a second numeric type, `json.Number`, whose `FormatNumber` returned plain integers and decimals **verbatim** (converting to float64 only when the raw string contained `e` or `E`) to preserve precision beyond 2^53. That path was deliberately not ported.

---

## 2. Error Code Catalog

### 2.0 What the specification actually requires (provenance)

Read this before treating any row below as normative. **The JSONata
language documentation does not publish an error-code catalog.** Its
source tree (`jsonata-js/jsonata/docs/`) has 29 pages and none of them is
an error reference; only six codes are named anywhere in it, and only one
of those is named as a language rule:

- **S0217** — *"It is implemented by static analysis of the expression at
  compile time and can only be used within expressions that navigate
  through that target parent value in the first place. If, for any
  reason, the parent location cannot be determined, then a **static
  error (S0217)** is thrown."* (Path Operators, `%` (Parent)). This is
  the only code the documentation attaches to a language rule — and it
  requires the error to be **static**, i.e. raised when the expression is
  compiled. jsntrs raises S0217 at **evaluation** time
  (`evaluator/mod.rs`), so `false ? % : 1` evaluates to `1` here and is a
  compile error in the reference. Tracked as a finding, not fixed: making
  it static needs the compile-time parent-resolution analysis.
- **S0202** and **T1006** — named only inside two *illustrative* error
  objects (Embedding and Extending JSONata). They show shape, not law.
- **D1011**, **D1012**, **D2015** — the Configuring Guardrails page,
  which opens *"This page contains information relating to the JavaScript
  reference implementation of JSONata, and not the JSONata expression
  language itself."* These are options of that implementation
  (`stack`, `timeout`, `sequence`), not language codes. jsntrs implements
  none of them: its depth cap reports **U1001** and its cancellation flag
  reports **D3001**.

Everything else in this catalog is inherited vocabulary. The only
complete definition of the other codes is `errorCodes` in jsonata-js
(2.2.2, `jsonata.js:5446`), which is authority of last resort. Rows below
therefore describe **what jsntrs emits**, and the *Notes* column flags
where that sits outside the inherited definition.

### 2.0.1 What an error carries

The documentation specifies the error object only by example. For a parse
error it shows

```
{ code: "S0202", stack: "...", position: 16, token: "}", value: "]",
  message: "Syntax error: expected ']' got '}'" }
```

and for a run-time error

```
{ code: "T1006", stack: "...", position: 14, token: "notafunction",
  message: "Attempted to invoke a non-function" }
```

(Embedding and Extending JSONata, `jsonata(str[, options])` and
`expression.evaluate(...)`). Both are introduced with *"for example"*.
There is **no normative statement anywhere in the documentation that an
error must carry a token, nor any rule for what a token's value should
be.** `JsonataError.token` is therefore a compatibility nicety with no
spec claim behind it, and no expectation should be re-derived from it.

Two consequences jsntrs holds to:

1. jsntrs attaches a token only where it names something that appears in
   the source: an operator (`+`, `<=`, `&`, `..`, `@`), the name a call
   site invokes (`abs`, `map`, `substring`), a variable name (`x`, `B`),
   or a literal (`1e1000`). All 92 pinned `token` expectations in
   `testdata/` are of that kind — but see the `and`/`or` exception below:
   *appears in the source* is weaker than *names what went wrong*.
2. jsntrs does **not** reproduce the reference's AST-label tokens. For
   S0217, jsonata 2.2.2 reports `token: "parent"` for `%`, `token:
   "path"` for `%.a` and `token: "function"` for `$foo(%)` — the parser's
   enclosing *node type*, not a source token. jsntrs leaves the S0217
   token empty. Do not port those.

**Where the tokens come from, and the one place they are wrong.** 55 of the
92 sit in `rust-error-tokens`; the wave-8 audit (jsntrs-qr9) re-probed every
one against jsonata 2.2.2 and found all 55 byte-identical to it. That group
is therefore a compatibility mirror, not a derivation — which is fine for a
field the suite reports rather than enforces, and is why it may not grow.

The mirror carries one reference defect. `evaluateBinary` wraps the *whole*
of an `and`/`or` — including the deferred right operand — in a try whose
catch assigns `err.token = op` unconditionally (jsonata 2.2.2
`jsonata.js:3917-3924`), so an error raised inside the right operand loses
its own attribution: `false or $abs('x')` is `T0410` token `"or"`, and
`true and 1/0` is `D1001` token `"and"`. jsntrs reproduces this in
`evaluator/binary.rs::with_op_token`. Both tokens do name something in the
source, so consequence 1 above holds literally, but neither names the
construct that raised the error. The documentation supplies no rule to
re-derive them from, so they stay pinned with a note in the case
(`rust-error-tokens/case024`-`case026`) rather than being guessed at.

### 2.1 Syntax/Lexer Errors (S0xxx)

| Code | Trigger | Notes |
|---|---|---|
| S0101 | Unterminated string literal | |
| S0102 | Invalid number literal | |
| S0103 | Unsupported escape sequence in a string | |
| S0104 | `\u` not followed by four hex digits | |
| S0105 | Unterminated backtick-quoted name | |
| S0106 | Unclosed block comment `/*` | |
| S0201 | Syntax error at a token; also "unexpected end of expression" for a truncated prefix (`a[`, `a.b.`, `$a := `) | reference reports S0203/S0207 for those three |
| S0202 | Expected token `X`, got `Y` | |
| S0203 | Expected `X` before end of expression | |
| S0204 | Malformed array constructor (`[1,2` with no `]`) | reference reports S0203; its S0204 means "unknown operator" |
| S0207 | Nothing follows an infix operator (`1 + `) | |
| S0208 | Function-definition parameter is not a `$variable` | |
| S0209 | Predicate follows a grouping expression in a step | |
| S0210 | More than one grouping expression in a step | |
| S0211 | Symbol cannot be used as a unary/prefix operator | *not* "invalid regex grouping" — earlier revisions of this file said so |
| S0212 | Left side of `:=` is not a `$variable` | token left empty; reference names the offending LHS |
| S0213 | Literal value used as a step in a path | |
| S0214 | Right side of `@`/`#` is not a variable name | |
| S0215 | Context variable binding must precede predicates on a step | |
| S0216 | Context variable binding must precede the order-by clause | |
| S0217 | `%` cannot be resolved to a parent | **raised at evaluation, not compile time** — see §2.0 |
| S0301 | Empty regex literal | |
| S0302 | Invalid regex flag / unterminated regex | |
| S0401 | Type parameters applied to something other than a function or array | |
| S0402 | Malformed function signature (union group, content type) | |

jsntrs never emits **S0205** or **S0206**. An earlier revision of this
file listed S0206 as "unmatched bracket/paren"; in the inherited catalog
S0206 is "Unknown expression type" and jsntrs has no site for it.

### 2.2 Type Errors (T0xxx, T1xxx, T2xxx)

| Code | Trigger | Notes |
|---|---|---|
| T0410 | Argument `N` does not match the function signature; also arity violations | the general-purpose argument error |
| T0411 | Context value is not compatible with argument `N` | only for focus-substituted calls (`5.$uppercase()`) |
| T0412 | Argument `N` must be an array of `<type>` | also `$merge(1)` — a bare value is the one-element sequence, so it is the same condition as `$merge([1])` |
| T1003 | Object key expression did not evaluate to a string | |
| T1005 | Invoking a name bound as a function but written without `$` | the "did you mean `$x`?" variant |
| T1006 | Attempted to invoke a non-function | the generic variant; documented shape (§2.0.1) |
| T1007 | Partially applying a name bound without `$` | the "did you mean `$x`?" variant |
| T1008 | Attempted to partially apply a non-function | the generic variant, including `$undefinedvar(?)` |
| T1010 | Matcher function does not return the expected object structure | |
| T2001 | Left operand of an arithmetic operator is not a number | |
| T2002 | Right operand of an arithmetic operator is not a number | |
| T2003 | Left side of `..` is not an integer | |
| T2004 | Right side of `..` is not an integer | |
| T2006 | Right side of `~>` is not a function | |
| T2007 | Order-by comparison of mismatched types | |
| T2008 | Order-by expression is neither numeric nor string | |
| T2009 | Comparison operands are of different types | |
| T2010 | Comparison operands are neither numeric nor string | |
| T2011 | Transform insert/update clause did not evaluate to an object | |
| T2012 | Transform delete clause is not a string or array of strings | |

### 2.3 Domain/Runtime Errors (D1xxx, D2xxx, D3xxx)

| Code | Trigger | Notes |
|---|---|---|
| D1001 | Non-finite arithmetic result (`1e308 * 10`) | `/` is exempt: `5/0` yields Infinity and errors only when consumed |
| D1002 | Unary minus on a non-numeric value; **also** an invalid regex in the match path | the second use sits outside the inherited definition ("cannot negate a non-numeric value") |
| D1004 | `$replace` pattern matches a zero-length string | |
| D1009 | Two key definitions evaluate to the same key | |
| D2014 | `..` would allocate more than 1e7 elements | |
| D3001 | String function applied to Infinity/NaN; **also** modulo by zero; **also** evaluation cancelled | only the first matches the inherited definition — see §2.4 |
| ~~D3006~~ | *Retired 2026-08-15 (jsntrs-89k).* Was raised for a missing required argument by `$not`, `$eval`, `$formatNumber`, `$formatInteger`, `$parseInteger` and `$match`. It appeared in no catalog and in no documentation, and nothing pinned it, so those six sites now raise **T0410** like the other twenty. |
| D3010 | `$replace` empty pattern; invalid regex to `$contains`/`$split`; malformed `$base64decode`; **also** `$append`/`$pad` size caps | the size caps are jsntrs-local guardrails |
| D3011 | `$replace` fourth argument is not a positive number | |
| D3012 | `$replace` replacement value is not a string | |
| D3020 | `$split` third argument is not a positive number | |
| D3030 | `$number` cannot cast the value | |
| D3050 | `$reduce` function takes fewer than two arguments | |
| D3060 | `$sqrt` of a negative number | |
| D3061 | `$power` result cannot be represented as a JSON number | documented condition: Numeric Functions, `$power` |
| D3070 | Single-argument `$sort` on mixed types | |
| D3080–D3093 | `$formatNumber` picture-string validation | one code per XPath 3.1 F&O §4.7.3 rule |
| D3100 | `$formatBase` radix outside 2..36 | documented condition: Numeric Functions, `$formatBase` |
| D3110 | `$toMillis` argument is not an ISO 8601 timestamp | |
| D3120 | Syntax error in the expression passed to `$eval` | |
| D3121 | Dynamic error in `$eval`; also `$eval` nesting depth exceeded | |
| D3130 | Unsupported `format-integer` sequence | |
| D3131 | Decimal digit pattern mixes digit groups | |
| D3132 | Unknown component specifier in a date/time picture | |
| D3133 | `name` modifier applied to something other than months/days | |
| D3134 | Timezone specifier has more than four digits | |
| D3135 | No matching `]` in a date/time picture | |
| D3136 | Picture is missing specifiers needed to parse the timestamp | |
| D3137 | `$error()` | message passthrough |
| D3138 | `$single` matched more than one result | |
| D3139 | `$single` matched nothing | |
| D3140 | Malformed URL to `$decodeUrl`/`$decodeUrlComponent`; **also** an unpaired surrogate in a `\uXXXX` string escape | the lexer use sits outside the inherited definition |
| D3141 | `$assert` | message passthrough |

### 2.4 Non-language codes

| Code | Trigger | Notes |
|---|---|---|
| U1001 | Lambda call depth exceeds `DEFAULT_MAX_CALL_DEPTH` (100), or the tail-call trampoline exceeds depth × 10,000 iterations | U1001 is in no catalog; the documentation's only U1001 is a *user-supplied* code in a third-party ReDoS example (Configuring Guardrails). The reference's stack guardrail is D1011 |
| D3001 | Evaluation cancelled via the cancellation flag | the documented analogue is D1012 ("Evaluation timeout"), on a page that disclaims being the language |
| D0000 | Not a JSONata error: malformed JSON *input*, and internal invariant violations that should be unreachable | never produced by evaluating a well-formed input |

---

## 3. Equality Semantics

Source: `internal/evaluator/value.go:190-244`

### 3.1 DeepEqual Truth Table

| Left | Right | `=` Result | Notes |
|---|---|---|---|
| `undefined` | `undefined` | `false` | Both nil -> false (not true!) |
| `undefined` | `null` | `false` | nil = anything -> false |
| `undefined` | `42` | `false` | nil = anything -> false |
| `null` | `null` | `true` | Both IsNull -> true |
| `null` | `undefined` | `false` | right is nil -> false |
| `true` | `true` | `true` | |
| `true` | `false` | `false` | |
| `true` | `1` | `false` | No type coercion in equality |
| `1` | `1.0` | `true` | Same float64 |
| `1` | `json.Number("1")` | `true` | normalizeNumber converts to float64 |
| `"abc"` | `"abc"` | `true` | |
| `"abc"` | `"ABC"` | `false` | Case-sensitive |
| `[]` | `[]` | `true` | Empty arrays equal |
| `[1,2]` | `[1,2]` | `true` | Element-wise |
| `[1,2]` | `[2,1]` | `false` | Order matters |
| `{}` | `{}` | `true` | Empty objects equal |
| `{"a":1}` | `{"a":1}` | `true` | Key-value comparison |
| `{"a":1,"b":2}` | `{"b":2,"a":1}` | `true` | Key order irrelevant for equality |
| `{"a":1}` | `{"a":2}` | `false` | |
| `[1]` | `1` | `false` | Array != scalar |

### 3.2 Inequality (`!=`)

Same nil-check as `=`: if either operand is `undefined`, result is `false` (not `true`).

```
undefined != undefined  -> false  (NOT true!)
undefined != 42         -> false  (NOT true!)
null != null            -> false
1 != 2                  -> true
```

### 3.3 Comparison Operators (`<`, `<=`, `>`, `>=`)

Source: `internal/evaluator/eval_helpers.go:104-156`

| Left | Right | Behavior |
|---|---|---|
| `undefined` | anything | `undefined` (nil) |
| anything | `undefined` | `undefined` (nil) |
| number | number | Numeric comparison |
| string | string | Lexicographic comparison |
| number | string | Error T2009 |
| string | number | Error T2009 |
| non-num/str | anything | Error T2010 (checked on left first) |
| anything | non-num/str | Error T2010 |

---

## 4. Binary Operator Behaviors

Source: `internal/evaluator/eval_binary.go`

### 4.1 Arithmetic (`+`, `-`, `*`, `/`, `%`, `**`)

| Left | Right | Result | Notes |
|---|---|---|---|
| number | number | computed | Normal arithmetic |
| `undefined` | number | `undefined` | nil propagation |
| number | `undefined` | `undefined` | nil propagation |
| `undefined` | `undefined` | `undefined` | nil propagation |
| `null` | number | Error T2002 | null is not a number |
| string | number | Error T2001 | string is not a number |
| number | `0` (division) | `Inf` | No error at division site |
| number | `0` (modulo) | Error D3001 | Modulo by zero |
| number | number (overflow) | Error D1001 | Result is Inf/NaN |

Division by zero: produces `Inf` which propagates without error at the division site. Downstream operations (like `$string(1/0)`) may then produce domain errors.

### 4.2 String Concatenation (`&`)

| Left | Right | Result |
|---|---|---|
| `"a"` | `"b"` | `"ab"` |
| `"a"` | `42` | `"a42"` |
| `42` | `"b"` | `"42b"` |
| `undefined` | `"b"` | `"b"` (nil -> "") |
| `"a"` | `undefined` | `"a"` (nil -> "") |
| `[1,2]` | `"x"` | `"[1,2]x"` |

Both sides coerced via `stringifyValue`.

### 4.3 Short-Circuit Operators

| Operator | Left Behavior | Right Behavior |
|---|---|---|
| `and` | If `ToBoolean(left)` is false, return `false` immediately | Only evaluated if left is truthy |
| `or` | If `ToBoolean(left)` is true, return `true` immediately | Only evaluated if left is falsy |
| `?:` (elvis) | If `ToBoolean(left)` is true, return `left` (the value, not boolean) | Only evaluated if left is falsy |
| `??` (coalesce) | If `left != nil`, return `left` | Only evaluated if left is nil/undefined |

Key difference between `?:` and `??`:
- `?:` checks truthiness (false, 0, "" are falsy)
- `??` only checks for undefined (null is NOT undefined, so `null ?? "default"` returns `null`)

### 4.4 `in` Operator

Source: `eval_helpers.go:194-215`

`elem in collection` checks if `elem` is contained in `collection` using `DeepEqual`:
- If `collection` is `[]any`: checks each element
- If `collection` is `*Sequence`: checks each value
- If `collection` is a scalar: checks if `DeepEqual(collection, elem)`
- If `collection` is `nil`: returns `false`

### 4.5 Range Operator (`..`)

Source: `internal/evaluator/eval_range.go`

- Both operands must be integers (T2003/T2004 if not)
- Max 10,000,000 elements (D2014)
- `1..5` produces `[1, 2, 3, 4, 5]` (inclusive both ends)
- `5..1` produces nothing (undefined); wrapped in an array constructor, `[5..1]` evaluates to `[]`. The range operator never counts down.

---

## 5. Auto-Mapping Behaviors

Source: `internal/evaluator/eval_helpers.go:241-308`

When `evalName` encounters an `[]any` input, it maps the field lookup across elements:

### 5.1 Basic Auto-Mapping

```
Input: [{"a": 1}, {"a": 2}, {"a": 3}]
Expression: a
Result: [1, 2, 3]
```

### 5.2 Nested Array Flattening

```
Input: [{"a": [1, 2]}, {"a": [3]}]
Expression: a
Result: [1, 2, 3]  (flattened, NOT [[1,2],[3]])
```

Inner arrays from field lookups are flattened into the result sequence.

### 5.3 Missing Fields

```
Input: [{"a": 1}, {"b": 2}, {"a": 3}]
Expression: a
Result: [1, 3]  (elements without "a" are skipped)
```

### 5.4 Empty Array Edge Case

```
Input: [{"a": []}, {"a": []}]
Expression: a
Result: []  (empty array, NOT undefined)
```

When at least one element had the field defined (even as empty), return `[]` rather than `nil`. This ensures `$exists(arr.a)` returns `true`.

### 5.5 All Missing

```
Input: [{"b": 1}, {"c": 2}]
Expression: a
Result: undefined (nil)
```

No elements had the field -> return undefined.

### 5.6 Singleton Unwrap

```
Input: [{"a": 1}]
Expression: a
Result: 1  (unwrapped, not [1])
```

Single-element sequences are unwrapped unless `KeepSingleton` is set.

---

## 6. Wildcard Behaviors

Source: `internal/evaluator/eval_helpers.go:310-352`

### 6.1 Wildcard on Object

```
Input: {"a": 1, "b": 2, "c": 3}
Expression: *
Result: [1, 2, 3]  (values in insertion order)
```

Array values from fields are flattened:
```
Input: {"a": [1, 2], "b": 3}
Expression: *
Result: [1, 2, 3]
```

### 6.2 Wildcard on Array

Wildcards on arrays recurse into map elements; non-map items are appended as-is if non-nil:
```
Input: [{"a": 1}, {"b": 2}, 3]
Expression: *
Result: [1, 2, 3]
```

```
Input: [{"a": 1}, 3]
Expression: *
Result: [1, 3]
```

### 6.3 Empty Object/Array

```
Input: {}
Expression: *
Result: undefined (nil)
```

---

## 7. Sequence Collapse Rules

Source: `internal/evaluator/value.go:54-92`

### 7.1 CollapseSequence

| Sequence Length | KeepSingleton | Result |
|---|---|---|
| 0 | false | `nil` (undefined) |
| 0 | true | `nil` (undefined) |
| 1 | false | element (unwrapped) |
| 1 | true | `[element]` (kept as array) |
| 2+ | false | `[]any{...}` (cloned slice) |
| 2+ | true | `[]any{...}` (cloned slice) |

### 7.2 CollapseAndKeep (for `[]` suffix)

When `keepArray` is true:
- Sequence -> CollapseSequence with KeepSingleton forced true
- `[]any` -> returned as-is
- `nil` -> returned as-is (not wrapped)
- scalar -> wrapped in `[]any{scalar}`

### 7.3 appendToSequence

Source: `eval_helpers.go:13-25`

- `nil` -> skipped (not added)
- `*Sequence` -> recursively append each inner value
- anything else -> appended directly

---

## 8. Sort Behavior

Source: `internal/evaluator/eval_sort.go`, `eval_helpers.go:158-192`

### 8.1 compareOrder Rules

| a | b | Order |
|---|---|---|
| `nil` | `nil` | equal (0) |
| `nil` | non-nil | `nil` sorts AFTER (1) |
| non-nil | `nil` | non-nil sorts BEFORE (-1) |
| number | number | numeric comparison |
| string | string | lexicographic comparison |
| number | string | Error T2007 |
| other | other | Error T2008 |

### 8.2 Sort Stability

Go's `slices.SortStableFunc` is used -> stable sort. Equal elements preserve original order.

### 8.3 Multi-Key Sort

Sort terms are evaluated left to right. First non-zero comparison wins. Descending terms negate the comparison result.

---

## 9. Transform Behavior

Source: `internal/evaluator/eval_transform.go`

1. Input is deep-cloned first (original is never mutated)
2. Pattern is evaluated against the cloned input
3. For each matching object in the input:
   a. Update expression is evaluated -> must return object (T2011)
   b. Update fields are merged into the target
   c. Delete expression is evaluated -> must return string or array of strings (T2012)
   d. Named fields are removed from the target
4. Non-object pattern matches are validated but not mutated

---

## 10. Group-By Behavior

Source: `internal/evaluator/eval_group.go`

1. Input is coerced to array
2. For each item, key expression is evaluated (must be string -> T1003)
3. Items are grouped by key, preserving first-seen key order
4. For each group, value expression is evaluated with:
   - `$index` bound to the position of the first item in the group
   - `$key` bound to the group key
5. Duplicate keys across different pair sets -> Error D1009
6. Result is an OrderedMap with group keys in first-seen order

---

## 11. Lambda/Closure Behavior

### 11.1 Closure Capture

Lambdas capture their enclosing environment at definition time. Variables from outer scopes are accessible.

### 11.2 Zero-Param Focus Capture

Zero-parameter lambdas capture `$` (focus) at definition time. When called, the body evaluates with the definition-time focus, not the call-site focus.

### 11.3 HOF Callback Arity

Source: `functions/hof_funcs.go`

| Lambda Params | Args Passed |
|---|---|
| 0 | `[]` (empty) |
| 1 | `[value]` |
| 2 | `[value, index]` |
| 3+ | `[value, index, array]` |

Built-in functions used as HOF callbacks always receive `[value]` only.

### 11.4 Tail-Call Optimization

- Lambdas in tail position are marked as thunks at parse time
- `callFunction` implements a trampoline loop
- Max iterations = call depth limit * 10000
- Prevents stack overflow for recursive functions

---

## 12. Context Variables

| Variable | Meaning |
|---|---|
| `$` | Current focus (input to current expression) |
| `$$` | Root input (bound at top level) |
| `%` | Parent value in path context (navigates environment chain) |
| `$index` | Loop index in group-by / HOF context |
| `$key` | Group key in group-by context |
