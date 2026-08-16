# rust-format-number-options

`$formatNumber`'s third argument: the options object that overrides the decimal
format. Fifty of these fifty-nine cases turn on one unsettled question, and it
is worth stating once here rather than fifty times in the cases.

## The question

XPath 3.1 F&O §4.7.1 types the decimal-format properties. Every property that
appears in a picture string is **a single character**:

| Property | F&O type |
|---|---|
| `decimal-separator`, `grouping-separator`, `exponent-separator` | A single character |
| `minus-sign`, `percent`, `per-mille`, `digit`, `pattern-separator` | A single character |
| `zero-digit` | A single character, "which must be a character in Unicode category Nd with decimal digit value 0 (zero)" |
| `infinity`, `NaN` | A string |

and it constrains them further:

> For any named or unnamed decimal format, the properties representing
> characters used in a picture string must have distinct values. These
> properties are decimal-separator, grouping-separator, exponent-separator,
> percent, per-mille, digit, and pattern-separator. Furthermore, none of these
> properties may be equal to any character in the decimal digit family.

The JSONata documentation points at exactly that section — "this argument must
be an object containing name/value pairs specified in the decimal format
section of the XPath F&O 3.1 specification" — and then, in the same page,
gives two worked examples that break its type table:

```
$formatNumber(0.14, "###pm", {"per-mille": "pm"})                 => "140pm"
$formatNumber(1234.5678, "①①.①①①e①", {"zero-digit": "⑟"})   => "①②.③④⑥e②"
```

`"pm"` is two characters. `⑟` is U+245F, Unicode category **Cn**, not Nd. Both
are pinned as conformance cases (`function-formatNumber/case007` and
`case011`), and jsntrs answers both exactly.

So JSONata is deliberately outside F&O §4.7.1's type table, and "F&O says a
single character" cannot by itself declare an answer wrong. But the
documentation gives **two points and no rule**. What a JSONata engine should do
with a two-character `pattern-separator`, an empty `zero-digit`, an
`exponent-separator` that is also a digit, or a `decimal-separator` that
collides with the grouping-separator is genuinely unspecified.

## What the cases therefore say

* **`authority`** (cases 006, 027, 035, 036, 046, 055–058) — derivable. Either
  the decimal format is well formed under §4.7.1 and ordinary F&O rules decide
  it, or it is the exact shape jsonata.org documents. The field carries the
  citation.
* **`unresolved`** (the other fifty) — the answer is a **regression pin over
  jsntrs' current behaviour and nothing else**. It records what the engine does
  today so a refactor cannot change it silently. It is not evidence that the
  behaviour is right, and it must not be cited as such.
* **`divergence`** (cases 017, 018, 021, 022, 030) — wave 6 already declined to
  follow jsonata-js here, on an F&O rule about what happens *after* the option
  is read. Those notes stand; the `unresolved` note next to them is about the
  premise, which the wave-6 work did not settle.

## Where current behaviour is hard to defend

Two families should be looked at first if this ever gets settled, because they
are not merely unspecified — they are internally inconsistent:

* **`{"zero-digit": ""}`** (cases 044, 045) makes `$formatNumber(7, "#")`
  return the **empty string**. With no zero-digit there is no decimal digit
  family, so every digit maps to nothing. A formatting function returning
  nothing at all for an ordinary number is not a defensible answer under any
  reading.
* **`{"zero-digit": "ab"}`** (cases 038, 039) reads one option two ways inside
  a single operation: the decimal digit family is built from the value's first
  character, while the zero-padding writes the value whole — hence `"ababh"`,
  which contains both. Whatever a multi-character `zero-digit` ought to mean,
  it ought to mean one thing.

Neither is invented here; both are what `crates/jsntrs/src/stdlib/format_number.rs`
documents itself as replicating from jsonata-js, whose option values are
JavaScript strings used with `indexOf`, `split` and string concatenation.

Audited under `jsntrs-qr9` (wave 8). The Decide item is in that issue: pick a
rule for option values that are not single characters, or declare the two
documented examples the whole of the contract and make everything else an
error.
