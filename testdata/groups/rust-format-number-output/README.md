# rust-format-number-output

`$formatNumber` output shapes: the analysis-phase adjustments of XPath 3.1 F&O
§4.7.4, the formatting steps of §4.7.5, grouping-separator placement on both
sides of the decimal separator, and the rounding rule.

Audited under `jsntrs-qr9` (wave 8). Every case now says what its expectation
rests on:

* **`authority`** — derived from XPath 3.1 F&O, or lifted from the W3C QT3
  suite (`../../oracles/qt3/format-number.jsonl`, cite the case id). The
  citation is in the field; check it rather than trusting the answer. A few
  cases here (026, 041, 042) cite `numberformat320`, which lives in
  `../../oracles/qt3/format-number-excluded.jsonl` rather than in the runnable
  file — the expectation it carries is sound, it is simply not one of the cases
  the extractor emits.
* **`divergence`** — jsonata-js answers differently and jsntrs declines to
  follow, with the F&O rule that says why. Wave 6 found these; wave 8 left
  them alone.
* **`unresolved`** — no authority fixes this answer. The case is a regression
  pin over jsntrs' current behaviour and nothing more, and it must not be
  read as evidence that the behaviour is right.

## The two unresolved cases

`case030` and `case036` both pass `{"exponent-separator": "."}`. The
decimal-separator is also `"."`, and F&O §4.7.1 forbids that outright:

> For any named or unnamed decimal format, the properties representing
> characters used in a picture string must have distinct values. These
> properties are decimal-separator, grouping-separator, exponent-separator,
> percent, per-mille, digit, and pattern-separator.

XSLT and XQuery reject such a decimal format before evaluation ever starts
(`XQST0098`; W3C QT3 `numberformat901err` is the same collision between
decimal-separator and grouping-separator). JSONata has no static context to
reject it in, no error code for it, and no documented behaviour — so what
`$formatNumber` should do with a picture whose decimal-separator and
exponent-separator are the same character is an open question, not a settled
one. See `../rust-format-number-options/README.md`, which is the same question
in its general form.

## The rounding cases

`case031`, `case032` and `case047` exist to pin F&O §4.7.5's two-step rounding,
which is not the same thing as a language-level `toFixed`:

> The mantissa is converted (if necessary) to an xs:decimal value […] If there
> are several such values that are numerically equal to the mantissa […] the
> one that is chosen should be one with the smallest possible number of digits
> […] This value is then rounded so that it uses no more than
> maximum-fractional-part-size digits in its fractional part […] by calling the
> function fn:round-half-to-even.

`$formatNumber(2.675, "0.00")` is the discriminating case: the shortest decimal
equal to the double is `2.675`, which round-half-to-even takes to `2.68`,
whereas rounding the double's exact binary expansion
(2.67499999999999982236…) gives `2.67`.
