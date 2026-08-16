# Licence for the vendored W3C QT3 expectations

The files `format-number.jsonl` and `format-number-excluded.jsonl` in this
directory are derived from `fn/format-number.xml` in the W3C QT3 test suite
(<https://github.com/w3c/qt3tests>).

Distributed under both the [W3C test suite
license](https://www.w3.org/copyright/test-suite-license-2023/) and the [W3C
3-clause BSD license](https://www.w3.org/copyright/3-clause-bsd-license-2008/).

## Which licence jsntrs is using, and why

`w3c/qt3tests` carries **no `LICENSE` file at its root** (verified 2026-08-15
against the repository contents listing). Its `w3c.json` declares:

```json
{ "group": [18797, 19552], "contacts": ["caribouW3"],
  "policy": "open", "repo-type": "tests" }
```

`repo-type: "tests"` is "Test suite work" in
<https://www.w3.org/guide/github/w3c.json.html>, so W3C's standing test-suite
licensing policy applies. That policy
(<https://www.w3.org/copyright/test-suites-licenses/>) says:

> To achieve these goals, W3C makes available test suites under two distinct
> licenses for two mutually exclusive uses:
>
> * a 3-clause BSD license for software development, bug tracking, and other
>   applications that do not require assertions of performance to the public or
>   implied claims of conformance to a W3C Specification. […]
> * a W3C test suite license for an authoritative W3C Test Suite or when claims
>   of performance with respect to a specification are required. […]
>
> The choice of license is up to the licensee for every single use of tests from
> a W3C Test Suite.

**jsntrs takes the 3-clause BSD option.** That option is the one that permits
copying and altering the tests, and this directory is an altered subset — the
expectations have been lifted out of the XQuery catalogue into a JSON Lines
record per case. The same policy page is explicit that this is the intended use
of the BSD option:

> Under the 3-clause BSD license, tests can be copied, altered, and integrated
> into software development tools, bug tracking tools, etc. This license allows
> developers, commercial vendors, and open source projects to copy tests and
> alter them as they wish to test and improve their software. However, if
> changes are made, the derivative work must not be distributed with W3C logos,
> unless W3C gives explicit permission.
>
> Note: It is explicitly understood that clause 3 of the BSD license prohibits
> the assertion of performance claims with respect to W3C Specifications by
> claiming successful passing of modified tests.

Consequences jsntrs accepts and must keep honouring:

* **No conformance claim.** jsntrs does not claim to pass the QT3 test suite,
  and no jsntrs document may use agreement with these files as an assertion of
  performance against a W3C specification. They are *evidence* used while
  deriving `$formatNumber` behaviour from XPath 3.1 F&O, nothing more. Nothing
  in `testdata/oracles/` is read by the conformance harness.
* **No W3C logos** anywhere in this repository.
* **No endorsement.** Neither the name of W3C nor the names of its contributors
  may be used to endorse or promote jsntrs (BSD clause 3, below).
* Clause 1 requires the copyright notice, the conditions and the disclaimer to
  travel with the redistribution; that is what this file is.

## Copyright notice

`w3c/qt3tests` carries no per-file or per-repository copyright notice, so the
standing W3C notice for the work applies:

> Copyright © 2010–2021 World Wide Web Consortium.
> <https://www.w3.org/copyright/>

The individual cases name their creators in the upstream `<description>` and
`<created>` metadata (David Marston, Michael Kay/Saxonica, Jim Melton,
Zhen Hua Liu, Carmelo Montanez and others); that metadata was **not** copied
into the extraction, so the upstream file remains the record of authorship.

## W3C 3-clause BSD license (2008 version), verbatim

> Redistribution and use in source and binary forms, with or without
> modification, are permitted provided that the following conditions are met:
>
> * Redistributions of works must retain the original copyright notice, this
>   list of conditions and the following disclaimer.
> * Redistributions in binary form must reproduce the original copyright notice,
>   this list of conditions and the following disclaimer in the documentation
>   and/or other materials provided with the distribution.
> * Neither the name of W3C nor the names of its contributors may be used to
>   endorse or promote products derived from this work without specific prior
>   written permission.
>
> THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
> AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
> IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
> DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE LIABLE
> FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
> DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
> SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
> CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
> OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
> OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

Source of the licence text: <https://www.w3.org/copyright/3-clause-bsd-license-2008/>
(retrieved 2026-08-15; the page states "This document is in effect since
20 August 2008. It was derived from the OSI 3-clause BSD.").

`extract-format-number.py` is jsntrs' own code and carries the repository's
licence, not W3C's.
