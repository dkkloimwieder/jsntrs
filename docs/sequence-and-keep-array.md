# Sequences and keep-array, derived from the language documentation

Status: **specification derivation**, wave 6 track V. Every rule below is
derived from <https://docs.jsonata.org> and quotes the sentence it rests on.
Where the documentation does not settle a question this file says so instead
of guessing — an unanswered question is recorded in *Open questions*, not
resolved by looking at an implementation.

This file is authority-ranked *above* `docs/spec.md`, which distils the Go
implementation and therefore describes an implementation. It is not a
description of what jsntrs currently does; several rules below are ones
jsntrs does **not** yet obey, and those are named explicitly.

Throughout, "the reference" means jsonata-js 2.2.2, cited only as evidence.

---

## 0. The sentences everything else is derived from

**S1 — what a path expression produces.**

> JSONata has been designed foremost as a query language, whereby a path
> expression can select zero, one or more than one values from the JSON
> document. These values, each of which can be of any of the types listed
> above, are returned as a *result sequence*.
>
> — [/processing](https://docs.jsonata.org/processing) § Sequences

**S2 — the empty sequence.**

> An **empty sequence** is a sequence with no values and is considered to be
> 'nothing' or 'no match'. It won't appear in the output of any expression.
>
> — [/processing](https://docs.jsonata.org/processing) § Sequences

**S3 — the singleton sequence.**

> A **singleton sequence** is a sequence containing a single value. It is
> considered equivalent to that value itself, and the output from any
> expression, or sub-expression will be that value without any surrounding
> structure.
>
> — [/processing](https://docs.jsonata.org/processing) § Sequences

**S4 — the many-valued sequence.**

> A sequence containing more than one value is represented in the output as a
> JSON array.
>
> — [/processing](https://docs.jsonata.org/processing) § Sequences

**S5 — matched and constructed arrays are not sequences.**

> Note that if an expression matches an array from the input JSON, or a JSON
> array is explicitly constructed in the query using the array constructor,
> then this remains an array of values rather than a sequence of values and
> will not be subject to the sequence flattening rules.
>
> — [/processing](https://docs.jsonata.org/processing) § Sequences

**S6 — sequences never nest.**

> If a sequence contains one or more (sub-)sequences, then the values from the
> sub-sequence are pulled up to the level of the outer sequence. A result
> sequence will never contain child sequences (they are flattened).
>
> — [/processing](https://docs.jsonata.org/processing) § Sequences

**S7 — the map operator.**

> For each value in the LHS array in turn:
> - The value is known as the *context* and is used as the basis for any
>   relative path expression on the RHS. It is also accessible in the RHS
>   expression using the `$` symbol.
> - The RHS expression is evaluated to produce a value or array of values (or
>   nothing). These values are appended to a combined array of results for the
>   operator as a whole.
>
> The combined result of the operator is returned.
>
> — [/path-operators](https://docs.jsonata.org/path-operators) § `.` (Map)

**S8 — the stages of a path expression.**

> **Map** — Evaluates the RHS expression in the context of each item in the
> input sequence. Flattens results into result sequence.
> **Filter** — Filter results from previous stage by applying predicate
> expression between brackets to each item.
> **Sort** — Sorts (re-orders) the input sequence according to the criteria in
> parentheses.
> **Index** — Binds a named variable to the current context position (zero
> offset) in the sequence.
> **Join** — Binds a named variable to the current context item in the
> sequence. Can only be used directly following a map stage.
> **Reduce** — Group and aggregate the input sequence to a single result
> object as defined by the name/value expressions. Can only appear as the
> final stage in a path expression.
>
> — [/processing](https://docs.jsonata.org/processing) § Path processing
>   stages

**S9 — singleton array and value equivalence.**

> Within a JSONata expression or subexpression, any value (which is not itself
> an array) and an array containing just that value are deemed to be
> equivalent.
>
> — [/predicate](https://docs.jsonata.org/predicate) § Singleton array and
>   value equivalence

**S10 — what `[]` is for, and where it may go.**

> When processing the return value of a JSONata expression, it might be
> desirable to have the results in a consistent format regardless of how many
> values were matched. […] This is done by adding empty square brackets `[]`
> to a step within the location path.
>
> Note that the `[]` can be placed either side of the predicates and on any
> step in the path expression.
>
> — [/predicate](https://docs.jsonata.org/predicate) § Singleton array and
>   value equivalence

with these four worked examples, all on the same document:

| expression | documented result |
|---|---|
| `Address[].City` | `[ "Winchester" ]` |
| `Phone[0][].number` | `[ "0203 544 1234" ]` |
| `Phone[][type='home'].number` | `[ "0203 544 1234" ]` |
| `Phone[type='office'].number[]` | `[ "01962 001234", "01962 001235" ]` |

**S11 — the array constructor.**

> At any point in a location path where a field reference is expected, a pair
> of square brackets `[]` can be inserted to specify that the results of the
> expression within those brackets should be contained within a new array in
> the output.
>
> — [/construction](https://docs.jsonata.org/construction) § Array
>   constructors

**S12 — the wildcard produces a sequence.**

> This wildcard selects the values of all the properties of the context
> object. It can be used in a path expression in place of a property name, but
> it cannot be combined with other characters like a glob pattern. The order
> of these values in the result sequence is implementation dependent.
>
> — [/path-operators](https://docs.jsonata.org/path-operators) § `*`
>   (Wildcard)

**S13 — array navigation.**

> Indexes are zero offset […] If the number is not an integer, then it is
> rounded *down* to an integer. […] Negative indexes count from the end of the
> array […] If an index is specified that exceeds the size of the array, then
> nothing is selected. If no index is specified for an array (i.e. no square
> brackets after the field reference), then the whole array is selected. […]
> Despite the structure of the nested array, the resultant selection is
> flattened into a single flat array.
>
> — [/simple](https://docs.jsonata.org/simple) § Navigating JSON Arrays

---

## 1. What a path step contributes to the result sequence

**R1.** A step is evaluated once per value in the sequence that reaches it
(S7, S8-Map). Each evaluation contributes to *one* combined sequence for the
step as a whole; the step does not produce one sequence per context.

**R2.** What one evaluation contributes is *values*, plural or zero — "The RHS
expression is evaluated to produce a value or array of values (or nothing).
These values are appended…" (S7). So:

- a result that is **nothing** contributes nothing;
- a result that is **one value** contributes that value;
- a result that is a **sequence** contributes its members, never itself (S6);
- a result that is an **array of values** contributes its members — this is
  the "flattened into a single flat array" of S13 and the "Flattens results
  into result sequence" of S8-Map.

**R3.** R2's last clause and S5 are in tension, and the documentation resolves
it only for the one-context case. S5 says a matched input array "remains an
array of values rather than a sequence of values and will not be subject to
the sequence flattening rules". Read together with S3, the consistent reading
— and the one both engines implement — is:

> A matched array is a **value**, not a sequence. When it is the sole
> contribution to a step's sequence, S3 makes the step's result that array
> itself, intact, including when it is empty. When several contexts each
> contribute, S7's "these values are appended" applies and the arrays'
> members are combined.

This is the reading that makes `a` on `{"a": []}` answer `[]` rather than
nothing, which both engines do. **See Open question Q1**: the documentation
does not say which of S5 and S7 wins when *several* contexts each contribute
an empty array, and the two engines disagree there.

**R4 — the wildcard is a sequence, not a match.** S12 says the wildcard's
values land "in the result sequence". A wildcard step is therefore governed by
S2/S3/S4 in full: no properties (or only empty-array properties, once R2 has
appended their zero members) means an **empty sequence**, which is nothing;
one value means that value, unwrapped. This is *not* the S5 exemption — the
wildcard matches an object's properties, not an array.

---

## 2. When the result collapses

**R5.** The collapse in S2/S3/S4 is not a serialisation step applied once at
the API boundary. S3 says "the output from any expression, **or
sub-expression** will be that value without any surrounding structure". A
sub-expression is a filter's result, a sort stage's result, a group-by pair's
value, a function argument, an operand of `=`. Each of those sees the
collapsed value.

Consequences that are not obvious:

- `[1,2,3][[0]]` is `1`, not `[1]`, and therefore `[1,2,3][[0]] = 1` is true.
  (Fixed in jsntrs-bmw.)
- A sort stage whose input sequence has one member yields that member (R8).
- A group-by pair whose value sequence is empty has *nothing* to insert, and
  S2 says nothing "won't appear in the output of any expression".

**R6 — collapse does not reach inside a matched or constructed array.** S5
exempts them; S3 speaks of *sequences*. So a sequence whose single member is
an array collapses to that array, and the array keeps its own length:
`[[1,2],[3,4]][[0]]` is `[1,2]`, and a following step or index sees a
two-element array — `[[1,2],[3,4]][[0]][0]` is `1` by S13, not `[1,2]`.

**R7 — S9 is a rule about *inputs*, not about outputs.** "any value (which is
not itself an array) and an array containing just that value are deemed to be
equivalent" licenses an operator to accept `1` where it expects `[1]`. It does
not license an implementation to *return* `[1]` where the sequence rules say
`1`; S3 is explicit that the output carries no surrounding structure. The two
sentences meet in the filter operator: a subscript applies to a single value
by treating it as the one-item sequence it is equivalent to, so `1[[0]]` is
`1` and `1[[1]]` is nothing. (Fixed in jsntrs-bmw.)

**R8 — a sort stage re-orders and does nothing else.** S8-Sort: "Sorts
(re-orders) the input sequence according to the criteria in parentheses." A
re-ordering is a permutation: it changes neither the membership of the
sequence, nor its emptiness, nor its singleton-ness, nor any flag on it.
Therefore, for any operand `X`:

- `X^(k)` has exactly as many members as `X`, so S2/S3/S4 give it the same
  shape `X` would have had. `a^(b)` on `{"a":[{"b":1}]}` is `{"b":1}`, and
  `$count(a^($))` on `{"a":[[3,1,2]]}` is `3` (one member, itself a
  three-element array — R6).
- `X[]^(k)` and `X^(k)[]` are the same expression as far as the flag is
  concerned (S10: "the `[]` can be placed either side of the predicates and on
  any step"), and both keep the wrap.
- The rule cannot depend on whether the sort key mentions `%`. There is one
  Sort stage in S8, not two.

This is the rule the parked issues jsntrs-by0 and jsntrs-09h need. jsntrs
obeys the second bullet for a sort key that does not mention `%` (fixed by
jsntrs-p0v.19: `a[]^(b)` and `a^(b)[]` on `{"a":{"b":1}}` are both
`[{"b":1}]`) and violates the first and the third. **jsntrs does not obey
R8 as a whole.**

---

## 3. Keep-array (`[]` on a step)

**R9 — one flag per path, applied once.** S10 puts `[]` "on any step in the
path expression", and its four worked examples put it on early steps and
observe a *single* level of array at the end (`Address[].City` →
`[ "Winchester" ]`, not `[[ "Winchester" ]]`). So `[]` is not a per-step
wrapper: it is one flag belonging to the path expression, which any step may
set, and which is honoured exactly once — when the path's final sequence is
collapsed.

**R10 — `[]` suppresses the singleton unwrap of S3. That is all it does.**
Its stated purpose is "to have the results in a consistent format regardless
of how many values were matched" (S10), and every documented example is a
match that produced one value and is shown as an array of one. It changes
neither which values were selected nor how many.

**R11 — `[]` on an empty sequence is still nothing.** S2 is unconditional:
an empty sequence "won't appear in the output of any expression". A flag that
suppresses an unwrap has nothing to unwrap when there are zero values, and S2
admits no exception for a flagged sequence. Therefore:

- a **missing name** (`c` where the context has no `c`) is nothing, and `c[]`
  is nothing — *not* `[]`;
- a **filter that rejects everything** (`a[$>5][]` on `{"a":[1,2]}`) is
  nothing;
- an **out-of-range index** (`a[0][]` on `{"a":[]}`) is nothing;
- a **wildcard with no values** (`a.*[]` on `{"a":{"c":[]}}`, by R4) is
  nothing;
- a group-by pair whose value is any of the above **drops the pair**, because
  nothing "won't appear in the output".

**R12 — `[]` on an *empty matched array* is that array.** This is not R11's
case and the distinction is the whole point of S5: `a` on `{"a": []}` matched
an array from the input, so by R3 the step's result is the array `[]` — a
defined value, not an empty sequence. `[]` then has a value to keep and
leaves it an array: `a[]` on `{"a":[]}` is `[]`. Both engines agree.

> **The distinction to hold on to.** "No values were selected" and "one value
> was selected and it happens to be an empty array" are different results that
> print the same way in a debugger. R11 applies to the first, R12 to the
> second. The test is not "is the result empty" but "was anything selected".

**R13 — `[]` and the array constructor `[expr]` are different operators.**
S11's brackets "contain the results of the expression within those brackets
within a new array"; S10's empty brackets suppress an unwrap. `[c]` where `c`
is missing is `[]` (a constructed array with no members — S5 makes it a value,
so it survives); `c[]` where `c` is missing is nothing (R11). An
implementation that routes one through the other will get exactly one of the
two right.

---

## 4. What this rule says about jsntrs today

Ordered by how load-bearing the divergence is. "spec" is the rule above.

| shape | spec | jsntrs | reference | issue |
|---|---|---|---|---|
| `a^(b)` on `{"a":[{"b":1}]}` | `{"b":1}` (R8+R5) | `[{"b":1}]` | `{"b":1}` | jsntrs-by0 |
| `$count(a^($))` on `{"a":[[3,1,2]]}` | `3` (R8+R6) | `1` | `3` | jsntrs-by0 |
| `x.a^(b)` on `{"x":{"a":[{"b":1}]}}` | `{"b":1}` (R8+R5) | `[{"b":1}]` | `{"b":1}` | jsntrs-by0 |
| `a[]^(%.k)` on `{"a":{"b":1},"k":2}` | `[{"b":1}]` (R8) | `{"b":1}` | `[{"b":1}]` | jsntrs-09h |
| `a{'k': c[]}` on `{"a":[{"b":1}]}` | `{}` (R11) | `{"k":[]}` | `{}` | jsntrs-a1e |
| `a{'k': *[]}` on `{"a":[{"c":[]}]}` | `{}` (R4+R11) | `{"k":[]}` | `{"k":[]}` | jsntrs-a1e |
| `a.*` on `{"a":{"c":[1]}}` | `1` (R4) | `1` | `[1]` | — |
| `a.*[]` on `{"a":{"c":[]}}` | nothing (R4+R11) | nothing | `[]` | — |
| `[1,2,3][[0]]` | `1` (R5) | `1` *(fixed)* | `1` | jsntrs-bmw |
| `[[1,2],[3,4]][[0]][0]` | `1` (R6) | `1` | `[1,2]` | — |
| `[1,2,3][-1.9]` | `2` (S13) | `2` *(fixed)* | `2` | jsntrs-2u4 |

The two rows where jsntrs already matches the spec and the reference does not
are worth stating plainly, because the previous wave treated the reference as
the target and parked a correct fix over them:

- **`a.*` on `{"a":{"c":[1]}}`.** S12 says the wildcard's values go into a
  *result sequence*; a one-value result sequence is that value (S3). jsntrs
  answers `1`. The reference answers `[1]` because its `evaluateWildcard`
  returns a plain JS array rather than a sequence, so neither S2 nor S3 ever
  fires on it.
- **`a.*[]` on `{"a":{"c":[]}}`.** Same cause: R4 makes this an empty
  sequence, R11 makes it nothing. jsntrs answers nothing. The reference
  answers `[]`.

**This unblocks jsntrs-a1e.** Its parking note reads: "a wildcard over an
object whose only value is an empty array yields an EMPTY SEQUENCE, and
keep-array on an empty sequence is `[]`, a defined value, so the pair stays."
The first half is right (R4) and the second half is wrong (R11): S2 is
unconditional, and even the reference sets its own `keepSingleton` flag
*before* testing for emptiness, so the flag never has anything to wrap. The
six rows that were read as regressions — `a{'k': *[]}` and `a{'k': *[][]}` on
three documents — are the fix being correct and the 1ac8814 baseline copying
the reference's wildcard artifact. a1e's main change (drop the pair) is
correct as written; what needs re-deriving is the *baseline*, not the change.

---

## 5. Open questions — the documentation does not answer these

**Q1. Several contexts each contributing an empty matched array.**
`a.b` on `{"a":[{"b":[]},{"b":[]}]}` is `[]` in jsntrs and nothing in the
reference. S5 says a matched array is not subject to the flattening rules;
S7 says the map appends the *values* produced by each context. With one
context the two agree (`a.b` on `{"a":{"b":[]}}` is `[]` in both). With
several they do not, and no sentence in the documentation ranks them. Do not
change either engine's behaviour on the strength of the other.

**Q2. A sort over an empty matched array.**
`a^($)` on `{"a":[]}` is `[]` in jsntrs and nothing in the reference. Under R3
the operand is the value `[]`; under S8-Sort the operand is "the input
sequence", which would be empty and therefore nothing. The documentation uses
both vocabularies without saying which applies to a matched array. This is
jsntrs-by0 item 4 and it should **not** be folded into the R8 fix — R8 is
about permutation preserving shape, and Q2 is about what shape the operand had
to begin with.

**Q3. `[]` on a sequence whose single member is an array.**
`[[1,2],[3,4]][[0]][]` is `[[1,2]]` in both engines today. R10 says `[]`
suppresses the unwrap, which gives `[[1,2]]`; R6 says the collapsed value is
the array `[1,2]`, and S10's purpose ("a consistent format regardless of how
many values were matched") is already satisfied by an array. Both readings are
defensible and the documented examples never select an array-valued single
match. jsntrs preserves `[[1,2]]`; leave it there until the question is
settled.

**Q4. Which error code an unrepresentable number raises.**
The documentation site has no error-code page (its navigation lists no such
page, and the numeric-operators page names no code). "It is an error if either
operand is not a number" is the whole of the guidance. Codes in jsntrs are
therefore internal consistency, not conformance.

**Q5. A bare `%` as an entire predicate.**
`a.b[%]` is `[1,2,3]` in jsntrs (the parent is the `a` object, which is
truthy) and nothing in the reference. The reference's own AST shows why —
it assigns the slot level 1 where `%.n` gets level 0, and never marks the
step `tuple: true`, so the slot is never filled — and [/path-operators]
§ `%` says an unresolvable parent is a *static error S0217*, which the
reference does not raise either. jsntrs's answer follows the definition of
`%`; the case is pinned in `rust-parent-in-predicate` with a divergence note.

**Q6. Where a filter stage applies when the path carries a `%`.**
`a.b[$exists(%)][0]` on `{"a":[{"b":[1,2]},{"b":[3,4]}]}` is `[1,3]` in
jsntrs and `1` in the reference, while `a.b[true][0]` on the same document is
`[1,3]` in both. The reference switches a filter from per-context to
whole-stream purely because an unrelated sibling predicate mentions `%`. S8
describes one Filter stage, and the documented behaviour of `Account.Order.
Product[0]` is per-context, so jsntrs's self-consistency looks right — but the
documentation never states it, so this stays a question.
