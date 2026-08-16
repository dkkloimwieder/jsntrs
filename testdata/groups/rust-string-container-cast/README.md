# rust-string-container-cast

`$string` on an array or object: the same 15-significant-digit cast the scalar
arm applies, reaching every member of the tree.

Read `../rust-string-precision-ties/README.md` first — it states where the cast
comes from and what is and is not documented about it. This group is about
where the cast *reaches*.

## What the cases separate

* **The cast is a value transform over the whole tree, not a print-time
  rewrite** (cases 000–005, 008–012, 024, 025). `JSON.stringify(arg, replacer,
  space)` runs the replacer at every node and then serializes exactly; jsntrs
  does the same in two steps (`Value::string_cast`, then the ordinary JSON
  writer), which is why `$string([[[0.4308013916015625]]])` rounds three levels
  down.
* **Integers are exempt** (cases 006, 007, 013). jsonata-js's replacer is
  guarded by `!Number.isInteger(val)` and jsntrs' `string_cast_number` by
  `n.fract() != 0.0`, so `9007199254740994` and `123456789012345678901` print
  their exact double digits. These pin the boundary of the cast rather than the
  cast.
* **The other three documented casting bullets** (cases 014, 015, 016) —
  functions to `""`, and everything else through `JSON.stringify`. Case 014 is
  the interesting one: a function *member* becomes `""`, which bare
  `JSON.stringify` would render as `null`, so it is direct evidence that the
  documented sentence means the replacer form.
* **`prettify`** (cases 017–021). jsonata.org documents the shape ("One line per
  field and lines will be indented based on the field depth") but not the
  indent **width**; two spaces is `JSON.stringify`'s `space` argument as
  jsonata-js passes it. Cases 020 and 021 hold no value the cast touches, so
  they pin the layout alone.
* **`&` inherits all of it** (cases 022, 023), because jsonata.org defines the
  operator by delegation: "If either or both of the operands are not strings,
  then they are first cast to string using the rules of the `$string`
  function."
* **The layer boundary** (cases 026–029). No `$string`, so nothing rounds:
  `[1234567890123456.7]` keeps `1234567890123456.8` and `[0.1 + 0.2]` keeps
  `0.30000000000000004`. Paired with case 000 and case 010 they assert that
  exactly one of the two number-output layers rounds.

Audited under `jsntrs-qr9` (wave 8). No expectation changed; each case now
carries its citation.
