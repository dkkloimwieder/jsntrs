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
| `NaN` | float64 | `"null"` | |
| `Inf` | float64 | `"null"` | |
| `true` | bool | `"true"` | |
| `false` | bool | `"false"` | |
| `Null` | null | `"null"` | JSON marshaled |
| `[1,2]` | []any | `"[1,2]"` | JSON marshaled (no HTML escape) |
| `{"a":1}` | OrderedMap | `{"a":1}` | JSON marshaled, preserves key order |

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

Numbers have a **single** representation, `Value::Number(f64)` (`crates/jsntrs/src/value.rs:82`). JSON input is converted to f64 at parse time (`Value::from_json`, `value.rs:464-467`), so integers beyond 2^53 are not preserved verbatim -- extra digits are lost on ingest. Output goes through `ryu-js` (`value.rs:521-528`) and `&`/`$string` coercion through `format_float` (`value.rs:399-401`).

*Go reference:* the Go engine kept a second numeric type, `json.Number`, whose `FormatNumber` returned plain integers and decimals **verbatim** (converting to float64 only when the raw string contained `e` or `E`) to preserve precision beyond 2^53. That path was deliberately not ported.

---

## 2. Error Code Catalog

### 2.1 Syntax/Lexer Errors (S0xxx)

| Code | Trigger | Message Pattern |
|---|---|---|
| S0101 | Unterminated string literal | String literal must be terminated |
| S0102 | Invalid number literal | Number out of range |
| S0103 | Invalid escape sequence in string | Unsupported escape sequence |
| S0104 | Invalid unicode escape `\uXXXX` | Invalid unicode codepoint |
| S0105 | Unterminated backtick-quoted name | Quoted name must be terminated |
| S0106 | Unclosed block comment `/*` | Block comment must be terminated |
| S0201 | Unexpected token in expression | Syntax error: unexpected token |
| S0202 | Expected token not found | Expected `X` got `Y` |
| S0203 | Expected token type | Expected `X` before end of expression |
| S0206 | Unmatched bracket/paren | Unmatched `X` |
| S0211 | Invalid regex grouping | Regex error |
| S0213 | Invalid step in path (numeric literal) | Invalid step |
| S0217 | `%` operator outside valid path context | Parent operator not valid |
| S0301 | Empty regex pattern | Empty regex |
| S0302 | Invalid regex (bad flag or syntax) | Invalid regex flag / Invalid regex |
| S0401 | Content-type on non-array/function type | Signature error |
| S0402 | Malformed function signature | Signature parse error |

### 2.2 Type Errors (T0xxx, T1xxx, T2xxx)

| Code | Trigger | Message Pattern |
|---|---|---|
| T0410 | Argument type mismatch / wrong arity | Argument `N` of function must be... |
| T0412 | Array content-type violation | Array element type mismatch |
| T1003 | Key expression evaluates to non-string | Key must evaluate to string |
| T1005 | Function invoked without `$` prefix | Attempted to invoke unquoted name |
| T1006 | Attempted to invoke undefined/non-function | Attempted to invoke non-function |
| T1007 | Partial application of non-$ function | Cannot partially apply |
| T1008 | Partial application of non-function | Cannot partially apply non-function |
| T2001 | Arithmetic operand is not a number | Left/right operand must be number |
| T2002 | Arithmetic operand is null | Left/right operand must be number (null) |
| T2003 | Range left side not integer | Left side of range must be integer |
| T2004 | Range right side not integer | Right side of range must be integer |
| T2006 | `~>` right side not a function | Right side of chain must be function |
| T2007 | Cannot compare string and number | Cannot compare string and number |
| T2008 | Cannot compare incompatible types | Cannot compare types |
| T2009 | Comparison operands must be same type | Operands must be both numbers or strings |
| T2010 | Comparison operands must be numbers or strings | Operands must be numbers or strings |
| T2011 | Transform update clause must return object | Transform update must be object |
| T2012 | Transform delete clause must return string/array of strings | Transform delete must be strings |

### 2.3 Domain/Runtime Errors (D1xxx, D2xxx, D3xxx)

| Code | Trigger | Message Pattern |
|---|---|---|
| D1001 | Number out of range (Inf/NaN from arithmetic) | Number out of range |
| D1002 | Invalid regex pattern / unary minus on non-number | Invalid regex / Cannot negate |
| D1009 | Duplicate key in object/group construction | Duplicate key |
| D2014 | Range exceeds 10M elements | Range too large |
| D3001 | Modulo by zero | Modulo by zero |
| D3010 | `$replace` empty pattern; invalid regex argument to `$contains`/`$split`; malformed `$base64decode` input | Pattern cannot be empty / invalid regex / bad base64 |
| D3030 | `$number` cannot cast value | Cannot cast to number |
| D3060 | `$sqrt` of negative number | Negative square root |
| D3061 | `$power` result non-finite | Power result non-finite |
| D3070 | Sort type mismatch | Cannot sort mixed types |
| D3121 | `$eval` maximum nesting exceeded | Maximum nesting depth exceeded |

### 2.4 Stack Overflow

| Code | Trigger | Message Pattern |
|---|---|---|
| U1001 | Recursive call depth exceeds 100 | Stack overflow |

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
