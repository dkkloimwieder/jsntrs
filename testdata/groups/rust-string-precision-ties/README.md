# rust-string-precision-ties

`$string` on a scalar number: the 15-significant-digit cast, and which way an
exact half goes.

## Why the cast exists at all

`$string` does not print the double. jsonata.org says only that a value is

> converted to a JSON string using the `JSON.stringify` function

but that sentence cannot be read as "exact ECMAScript number output", for two
reasons settled under `jsntrs-jnv` (decided 2026-08-15, written up in
`docs/spec.md` §4.2.6):

1. It names the three-parameter `JSON.stringify(value, replacer, space)`, whose
   replacer runs *before* the exact-number step. Two of the same doc list's four
   casting bullets — functions to `""`, infinity and NaN throwing — are
   unreachable without a replacer, and `prettify` occupies the third slot.
2. jsonata.org's own published outputs are rounded. `$sqrt(2)` is documented as
   `1.414213562373` — byte-exact `Math.sqrt(2).toPrecision(13)` — where exact
   ECMAScript `ToString` gives `1.4142135623730951`. `$power(2, 0.5)` and the
   three `$random()` samples on the same page are all 13 significant digits too.

So the documentation is silent on the **count** but not on the **kind**: the
only affirmative evidence of intent it offers is evidence *for* rounding.
jsntrs uses 15 because that is the live reference count (jsonata-js moved 13 →
15 in 1.5.4 and never updated the docs). **The count is a recorded deviation,
not a derived answer** — `docs/spec.md` §4.2.6 states it, and states the price:
`$string` is not round-trip lossless, `$number($string(0.4308013916015625))`
does not recover its argument.

## The ties

Cases 000, 001, 004 and 005 pin the direction, which is *not* `$round`'s
half-to-even. `Number(n.toPrecision(15))` strips the sign first and then, per
ECMAScript `Number.prototype.toPrecision`,

> If there are two such sets of exponent and intSignificand, pick the exponent
> and intSignificand for which intSignificand × 10^(exponent − precisionCount +
> 1) is larger.

— so an exact half goes **away from zero**: `499747614544282.5` → `…283` and
`-499747614544282.5` → `-…283`. Cases 002 and 003 bracket it from either side.

## The boundary

Cases 017 and 018 apply no `$string` at all and pin the raw values exactly. The
cast belongs to `$string`/`&` and to nothing else: `write_json` stays the exact
round-tripping layer, which is what CLAUDE.md invariant 5 is about. Together
with cases 000–016 they stop the rounding migrating into the wrong layer in
either direction.

Audited under `jsntrs-qr9` (wave 8). No expectation changed; every case now
carries the citation chain above so the next reader can see that "matches
jsonata-js" was never the reason.
