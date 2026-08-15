# Oracles

Recorded answers from another engine, kept as *evidence* for work on a
specific builtin. They are **not** part of the conformance suite: the
harness walks `testdata/groups/` only, and nothing here is asserted by
`cargo test`.

Read them with the project's standing rule in mind — jsntrs targets the
JSONata specification (and, for the picture-string builtins, XPath 3.1
F&O), not any particular implementation. Where an oracle disagrees with
the spec, the spec wins and the oracle is the thing that is wrong.

| File | What | Recorded from |
|---|---|---|
| `formatnumber-pictures.tsv` | 1,600 `$formatNumber` picture/option expressions with the answer another engine gives | jsonata-js 2.2.2 |

`formatnumber-pictures.tsv` replaces an earlier copy of the same corpus
that had been recorded against jsonata-js ≤ 2.1.0: 189 of its 1,600 lines
carried a raw JavaScript `TypeError` that 2.1.0 threw and 2.2.2 does not,
which silently misdirected `$formatNumber` work until it was caught in
August 2026. Re-record rather than trust a stale copy, and say which
version produced it.

Format: tab-separated, `expression <TAB> answer`, where an answer is
either a JSON literal or `ERR <code> <message>`.
