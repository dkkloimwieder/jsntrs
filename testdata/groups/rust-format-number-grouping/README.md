# rust-format-number-grouping

Integer-part grouping in `$formatNumber`: which grouping positions a picture
records, and whether they get extrapolated.

The whole group turns on one paragraph of XPath 3.1 F&O §4.7.4:

> The grouping is defined to be regular if the following conditions apply:
>
> * There is a[t] least one grouping-separator in the integer part of the
>   sub-picture.
> * There is a positive integer G (the grouping size) such that the position of
>   every grouping-separator in the integer part of the sub-picture is a
>   positive integer multiple of G.
> * Every position in the integer part of the sub-picture that is a positive
>   integer multiple of G is occupied by a grouping-separator.
>
> If the grouping is regular, then the integer-part-grouping-positions sequence
> contains all integer multiples of G as far as necessary to accommodate the
> largest possible number.

The third condition is the one implementations get wrong: it is not enough for
a G to divide every separator position, every multiple of G inside the integer
part must *also* carry a separator. `"#,##,##"` is regular at G=2 and extends
to a number longer than the picture; `"####,##,##"` records {2, 4} but is not
regular, because position 6 is empty — so its separators stay exactly where the
picture put them.

Cases 000–010 cover both branches, five regular and six irregular; each
`authority` field names the recorded positions and says which branch applies
and why. Cases 011–015 take their expectations from the W3C QT3 suite, which pins both
branches independently: `numberformat310/312/318/319` are in
`../../oracles/qt3/format-number.jsonl`, and `numberformat320` is in
`../../oracles/qt3/format-number-excluded.jsonl` — it is one of the sixteen
cases the extractor holds back (see that oracle's README for why), so it is
cited as evidence rather than run.

Audited under `jsntrs-qr9` (wave 8): every case in this group is derivable from
F&O or QT3, none of them rests on what an implementation happened to print.
jsonata-js disagrees with three of them (000-series case003, and QT3's
`numberformat310` and `numberformat319`) because it extrapolates from the
greatest common divisor without checking the third condition.
