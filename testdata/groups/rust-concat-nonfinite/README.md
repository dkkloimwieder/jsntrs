# rust-concat-nonfinite

`$string` and `&` when a non-finite number reaches them.

## The rule is documented; the code is not

jsonata.org lists four casting rules for `$string`, and one of them is a throw:

> Numeric infinity and NaN throw an error because they cannot be represented as
> a JSON number

`&` inherits it by delegation — jsonata.org, Other Operators:

> If either or both of the operands are not strings, then they are first cast to
> string using the rules of the `$string` function.

That is unusually strong documentation for JSONata: an affirmative statement
that a builtin throws on a *value* rather than a type. It is what every case in
this group rests on.

What the documentation does **not** give is a code, because it publishes no
error-code page at all — see `docs/behaviors.md` §2.0, which lists the six codes
that appear anywhere on the site and why only `S0217` is a language rule. So the
`D3001` / `D1001` split here is jsntrs' own spelling of a documented throw:

| shape | code |
|---|---|
| bare `Infinity`, `-Infinity`, `NaN` reaching `$string` or `&` | `D3001` |
| array or object *containing* a non-finite number | `D1001` |

The split is inherited vocabulary, recorded in `docs/behaviors.md` §1.3 together
with one deliberate deviation — jsonata-js renders a composite member that is
`NaN` as `null` instead of throwing, because its `isNumeric` returns false
before the `!isFinite` guard is reached; jsntrs raises `D1001` for either.
**This group deliberately does not exercise that case**, so nothing here pins
the deviation.

## Why the shapes vary

The fixtures reach the same two code paths through as many routes as the
language offers — literal division, a JSON `1e400` that overflows on parse, both
operand positions of `&`, a chained `&`, `$string` with and without `prettify`,
inside a path step, inside `$map` and `$each` lambdas — because the guard lives
in one place (`Value::stringify` / `stringify_into`) and the point is that no
route can bypass it. `1e400` is used rather than a bare `Infinity` because the
fixture files must stay valid JSON.

Audited under `jsntrs-qr9` (wave 8). No expectation changed; every case now
carries the two documentation quotes and the note that the code is not one of
them.
