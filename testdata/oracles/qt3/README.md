# W3C QT3 `fn:format-number` expectations

Executable evidence for XPath 3.1 F&O §4.7.3–§4.7.5. The QT3 suite is the W3C
XQuery/XPath working groups' own test suite for the F&O specification, so for
`$formatNumber` it **outranks jsonata-js**: where jsonata-js and QT3 disagree,
QT3 is telling you what the picture-string rules mean and jsonata-js is telling
you what one implementation happens to print.

Nothing here is read by `cargo test`. `tests/conformance.rs` walks
`testdata/groups/` only. These files are why a conformance case says what it
says, not a gate of their own — see `../README.md`.

## Provenance

| | |
|---|---|
| Upstream file | <https://raw.githubusercontent.com/w3c/qt3tests/master/fn/format-number.xml> |
| Repository | <https://github.com/w3c/qt3tests> |
| Fetched | 2026-08-15 |
| Repository HEAD at fetch | `201a6e466940cdfc727f4babfedcde5332b9f578` (2026-05-14) |
| Last commit touching the file | `fa2788dc0db2569ccb7e4fdbb4ca8369d00c7320` (2021-09-30) |
| SHA-256 of the fetched XML | `1875ca56ebcb01da7f0f494ce5e5e0f9cc112126efd49afa88aea13071a10287` |
| Bytes | 132,529 |
| Test cases in the file | 269 |

Licence and the notice it requires: **[`LICENSE.md`](LICENSE.md)** — read it
before adding anything here or citing these files anywhere. jsntrs redistributes
this subset under the W3C 3-clause BSD test-suite licence and therefore makes
**no claim of conformance to any W3C specification**.

Regenerate with:

```sh
curl -sSL -o format-number.xml \
  https://raw.githubusercontent.com/w3c/qt3tests/master/fn/format-number.xml
python3 extract-format-number.py format-number.xml /tmp/fn.json \
  format-number-excluded.jsonl
mv /tmp/fn.jsonl format-number.jsonl
```

## `format-number.jsonl` — 253 single-call cases

One JSON object per line, natural-sorted by case id. Fields:

| Field | Meaning |
|---|---|
| `qt3` | upstream `test-case/@name`; cite this id in a conformance case |
| `value` | the first argument **verbatim** as XQuery source, not converted |
| `picture` | the second argument, string-literal escapes already resolved |
| `decimal_format_name` | the third argument, when the case passes one |
| `decimal_format` | the decimal-format properties in force, merged from the case's `<environment><decimal-format>` and from any `declare [default] decimal-format` prolog |
| `decimal_format_undeclared` | the named format is not declared in this case (these are namespace-resolution tests) |
| `expect` | `{"string": …}`, `{"error": code}`, or `{"any_of": [ … ]}` |
| `dependency` | upstream `<dependency>` elements (spec version, optional features) |
| `query` | the whole upstream query, whitespace-collapsed |

A "single call" is a query that reduces to exactly one `format-number(…)` after
the `declare … decimal-format` prolog is lifted out, `let $x := … return $x` is
unwrapped and the `fn:` prefix is dropped. Sixteen cases do not reduce that way
and live in `format-number-excluded.jsonl` with an `excluded` reason and their
untouched query and expectation, so no upstream case is silently dropped.
`numberformat320`, cited by several conformance cases, is one of them: it is a
`||` concatenation of three calls.

## Reading these against JSONata

The mapping is close but not exact, and the differences are where mistakes get
made:

* **The options object is the decimal format.** JSONata's `$formatNumber`
  third argument is an options object with the same property names as XSLT's
  `xsl:decimal-format`; XPath's third argument is instead the *name* of a
  statically declared format. So `decimal_format` here is what a JSONata
  options object would carry, and `decimal_format_name` /
  `decimal_format_undeclared` mark cases testing XQuery name resolution, which
  JSONata has no analogue for (`FODF1280`, `XQST0097/0098/0114`, `XPST0003`
  are static XQuery errors, not picture-string errors).
* **`value` is XQuery source, deliberately.** `xs:decimal('…')` has unbounded
  precision; JSONata numbers are IEEE doubles. A case whose `value` is an
  `xs:decimal` with more than 17 significant digits cannot be reproduced —
  that is the single-`f64` numeric model, `docs/spec.md` §1.4 and issue
  `jsntrs-c64`, not a `$formatNumber` defect. `numberformat119` and
  `numberformat120` (18 significant digits) are the standing examples.
* **Error codes differ by design.** F&O raises `FODF1310` for a malformed
  picture; JSONata splits the same condition across `D3080`–`D3093`. Compare
  the *classification* (does the picture raise at all?), not the code.
* **`xs:double` vs `xs:decimal` inputs matter.** F&O §4.7.2 says of `$value`:
  "Note that if an `xs:decimal` is supplied, it is not automatically promoted to
  an `xs:double`, as such promotion can involve a loss of precision." Several
  cases exist only to pin that boundary, and JSONata sits permanently on the
  `xs:double` side of it.
