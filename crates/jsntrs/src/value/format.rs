//! Number formatting matching JavaScript's `Number.toString()` behavior.
//!
//! Go equivalent: `FormatFloat` in `eval_helpers.go`, `FormatNumber` in `eval_helpers.go`.
//! Uses `ryu-js` crate for exact ECMAScript formatting.

/// Render one number the way `$string` (and `&`) must render it.
///
/// This is the *scalar* arm of the `$string` cast, and it is deliberately the
/// same two steps the container arm takes ([`Value::string_cast`] followed by
/// the JSON writer): snap the value to 15 significant digits with
/// [`string_cast_number`], then render that double exactly, with `ryu-js`.
/// The rounding is the cast; the rendering is ECMAScript
/// `Number.prototype.toString`, which `ryu-js` implements.
///
/// [`Value::string_cast`]: crate::Value::string_cast
///
/// Doing anything else here splits the engine against itself. Until
/// jsntrs-nyn this function ran a port of Go's `FormatFloat(n, 'g', 15, 64)`
/// pipeline whose scientific threshold (`exp < -1`) let every `|n| < 0.1`
/// escape the cast, so `$string(0.04308013916015625)` answered
/// `"0.04308013916015625"` while `$string([0.04308013916015625])` answered
/// `"[0.0430801391601563]"` — one number, one function, two answers. No
/// conformance case covered the band, which is how the split survived.
///
/// What the cast *is* is settled separately (jsntrs-jnv): the JSONata
/// documentation never states a digit count, but its own worked examples are
/// rounded, and the sentence delegating to `JSON.stringify` names a function
/// whose replacer runs before the exact-number step. 15 digits is the live
/// reference count, recorded as a deviation in `docs/spec.md`.
///
/// Non-finite input returns `"null"`. `$string` rejects those with `D1001`
/// long before this, but the error payloads that report them come through
/// here and pin that spelling.
pub fn format_float(n: f64) -> String {
    if !n.is_finite() {
        return "null".into();
    }

    // Integral values below 1e15 print as their plain digits: the cast passes
    // integers through untouched, so this is what both steps below would
    // produce anyway, without their intermediate work. Integers dominate the
    // string-concat and $string hot paths, and this also settles negative
    // zero — `-0.0 as i64` is 0, and JavaScript prints `String(-0)` and
    // `JSON.stringify(-0)` both as "0" (jsntrs-p0v.5).
    if n.abs() < 1e15 && n.fract() == 0.0 {
        return format!("{}", n as i64);
    }

    let mut buf = ryu_js::Buffer::new();
    buf.format_finite(string_cast_number(n)).to_owned()
}

/// Round `n` to 15 significant decimal digits and return the nearest double
/// — ECMAScript `Number(n.toPrecision(15))`, the cast jsonata-js's `$string`
/// applies to a non-integral number before serializing it.
///
/// The digits come from [`g15_significand`], so ties round away from zero;
/// re-parsing a ≤15-digit decimal lands on the same double `Number(…)` would.
fn round_to_15_significant(n: f64) -> f64 {
    let (digits, exp) = g15_significand(n.abs());
    let sign = if n.is_sign_negative() { "-" } else { "" };
    let (lead, rest) = digits.split_at(1);
    format!("{sign}{lead}.{rest}e{exp}").parse().unwrap_or(n)
}

/// The `$string` replacer applied to one number, as a number.
///
/// jsonata-js serializes a container with
/// `JSON.stringify(arg, replacer, space)` where the replacer is
/// `val.toPrecision && !Number.isInteger(val) ? Number(val.toPrecision(15))
/// : val` (jsonata 2.2.2, `string()` in `src/functions.js`). So an integer
/// keeps its exact digits and everything else is snapped to the nearest
/// double with 15 significant digits *before* serialization — the numbers
/// themselves change, and the JSON writer that follows is still the exact
/// round-tripping one.
///
/// Non-finite input is returned unchanged; `$string` rejects it with `D1001`
/// before reaching here.
pub(crate) fn string_cast_number(n: f64) -> f64 {
    if !n.is_finite() || n.fract() == 0.0 {
        n
    } else {
        round_to_15_significant(n)
    }
}

/// Powers of five up to the largest one that can still leave a 16-digit
/// product (5^23 = 1.19e16 already has 17 digits on its own).
const POW5: [u128; 23] = {
    let mut table = [1u128; 23];
    let mut i = 1;
    while i < 23 {
        table[i] = table[i - 1] * 5;
        i += 1;
    }
    table
};

/// The exact decimal significand of `abs` when rounding it to 15 significant
/// digits is an exact tie, paired with the power of ten it sits over.
///
/// Every finite double is `m × 2^e` with `m` odd. For `e < 0` — a
/// non-integral value — the exact decimal expansion is `M / 10^k` with
/// `k = -e` and `M = m·5^k`. `M` is an odd multiple of five, so its last
/// digit is always 5 and its first is non-zero: the significant digits of
/// the value are *exactly* the digits of `M`, and rounding to 15 significant
/// digits is an exact tie precisely when `M` has 16 of them. For `e ≥ 0` the
/// value is the integer `m·2^e`, which ends in an even digit unless `e == 0`,
/// so only a bare odd mantissa can tie there.
///
/// Returns `(M, k)`: the value is `M × 10^-k`, so its decimal exponent (as in
/// `d1.d2…d16 × 10^exp`) is `15 - k`. `abs` must be finite and non-zero.
fn exact_15_digit_tie(abs: f64) -> Option<(u128, u32)> {
    let bits = abs.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    // Subnormals carry no implicit leading bit and share the smallest exponent.
    let (mut m, mut e) = if biased == 0 {
        (frac, -1074i32)
    } else {
        (frac | (1u64 << 52), biased - 1075)
    };
    if m == 0 {
        return None;
    }
    let shift = m.trailing_zeros();
    m >>= shift;
    e += shift as i32;

    let (significand, k) = if e >= 0 {
        if e > 0 {
            return None;
        }
        (u128::from(m), 0u32)
    } else {
        let k = e.unsigned_abs();
        // 5^23 already exceeds 10^16, so no larger k can leave 16 digits.
        if k as usize >= POW5.len() {
            return None;
        }
        (u128::from(m) * POW5[k as usize], k)
    };

    if significand % 10 == 5
        && (1_000_000_000_000_000..10_000_000_000_000_000).contains(&significand)
    {
        Some((significand, k))
    } else {
        None
    }
}

/// `abs` rounded to 15 significant decimal digits, ties **away from zero**.
///
/// That is ECMAScript's `Number.prototype.toPrecision` rule: it strips the
/// sign first, then picks, among two equally close candidates, "the `e` and
/// `n` for which `n × 10^(e-p+1)` is larger". Rust's `{:.14e}` is correctly
/// rounded but breaks ties to *even*, and by the time its output is in hand
/// the 16th digit that would identify the tie is gone — so the tie case is
/// recomputed from the exact significand rather than patched (jsntrs-1uk).
///
/// Returns exactly 15 digits and the decimal exponent `exp`: the value is
/// `d1.d2…d15 × 10^exp`. `abs` must be finite, positive and non-zero.
fn g15_significand(abs: f64) -> (String, i32) {
    let sci = format!("{abs:.14e}");
    let (mantissa_str, exp_str) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp_str.parse().unwrap_or(0);
    let digits: String = mantissa_str.replace('.', "");

    let Some((significand, k)) = exact_15_digit_tie(abs) else {
        return (digits, exp);
    };

    // Exact tie: drop the trailing 5 and round the magnitude up.
    let rounded = significand / 10 + 1;
    let exp = 15 - k as i32;
    if rounded >= 1_000_000_000_000_000 {
        // 999…9|5 carried into a 16th digit.
        ("100000000000000".to_owned(), exp + 1)
    } else {
        (rounded.to_string(), exp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_integers() {
        assert_eq!(format_float(0.0), "0");
        assert_eq!(format_float(1.0), "1");
        assert_eq!(format_float(42.0), "42");
        assert_eq!(format_float(-1.0), "-1");
    }

    /// The integer fast path must be indistinguishable from the 'g'15
    /// pipeline, including at its boundaries.
    #[test]
    fn format_integer_fast_path_matches_g15_pipeline() {
        assert_eq!(format_float(123_456.0), "123456");
        assert_eq!(format_float(999_999_999_999_999.0), "999999999999999");
        assert_eq!(format_float(-999_999_999_999_999.0), "-999999999999999");
        // 1e15 is just past the fast path; the 'g'15 pipeline goes
        // scientific and step 3 converts back to full decimal — same text
        // the fast path would have produced, via the slow route.
        assert_eq!(format_float(1e15), "1000000000000000");
    }

    /// jsonata-js-verified (2026-08-14): `$string(-0)` is "0", matching
    /// `String(-0)` and the `ryu-js` JSON layer. The pre-jsntrs-p0v.5
    /// output was Go's "-0", which made `$string(0 * -1)` disagree with the
    /// serialized form of the same value.
    #[test]
    fn negative_zero_prints_unsigned() {
        assert_eq!(format_float(-0.0), "0");
        assert_eq!(format_float(0.0), "0");
        // The way `0 * -1` reaches the formatter from an expression.
        assert_eq!(format_float(f64::from(0) * f64::from(-1)), "0");
    }

    #[test]
    fn format_decimals() {
        assert_eq!(format_float(0.5), "0.5");
        assert_eq!(format_float(3.25), "3.25");
    }

    #[test]
    fn format_nan_inf_as_null() {
        assert_eq!(format_float(f64::NAN), "null");
        assert_eq!(format_float(f64::INFINITY), "null");
        assert_eq!(format_float(f64::NEG_INFINITY), "null");
    }

    #[test]
    fn format_scientific_large() {
        // 1e21 and above should use scientific notation
        assert_eq!(format_float(1e21), "1e+21");
        assert_eq!(format_float(1e25), "1e+25");
    }

    /// jsonata-js 2.2.2-verified (2026-08-15, jsntrs-p0v.24): a non-integral
    /// number at or above 1e15 is cast through
    /// `Number(val.toPrecision(15))` before it is serialized, so the digits
    /// past the 15th become zeros. Printing the shortest round-tripping
    /// digits instead — what the `'g'15`-then-`'f',-1` pipeline does — leaked
    /// them: `$string(1234567890123456.7)` came out "1234567890123456.8".
    #[test]
    fn format_rounds_non_integers_at_or_above_1e15() {
        assert_eq!(format_float(1_234_567_890_123_456.7), "1234567890123460");
        assert_eq!(format_float(-1_234_567_890_123_456.7), "-1234567890123460");
        assert_eq!(format_float(1.234_567_890_123_456_7e15), "1234567890123460");
        // Every ulp in the band, written as a sum because the eighths and
        // quarters need 16+ digits to spell out literally: 1/8 below 2^50,
        // then 1/4, then 1/2 up to 2^52. All three additions are exact.
        assert_eq!(format_float(1e15 + 0.125), "1000000000000000");
        assert_eq!(format_float(1e15 + 0.5), "1000000000000000");
        assert_eq!(
            format_float(1_234_567_890_123_456.0 + 0.75),
            "1234567890123460"
        );
        assert_eq!(
            format_float(2_251_799_813_685_248.0 + 0.5),
            "2251799813685250"
        );
        // Just below the band the 'g'15 pipeline already rounded, and
        // 999999999999999.9 rounds up into 16 digits.
        assert_eq!(format_float(999_999_999_999_999.9), "1000000000000000");
    }

    /// jsonata-js 2.2.2-verified (2026-08-15, jsntrs-p0v.24): the same
    /// replacer leaves *integers* alone (`!Number.isInteger(val)` guards the
    /// cast), so they keep their exact ECMAScript digits however long those
    /// run. This is what function-string/case030 pins, and the rounding
    /// branch must not reach them.
    #[test]
    fn format_keeps_large_integers_exact() {
        assert_eq!(format_float(5_890_840_712_243_076.0), "5890840712243076");
        assert_eq!(
            format_float(1.234_567_890_123_456_8e20),
            "123456789012345680000"
        );
        assert_eq!(
            format_float(-1.234_567_890_123_456_8e20),
            "-123456789012345680000"
        );
        // 2^53 and its neighbours: 16 and 17 significant digits, all kept.
        assert_eq!(format_float(9_007_199_254_740_992.0), "9007199254740992");
        assert_eq!(format_float(9_007_199_254_740_994.0), "9007199254740994");
        assert_eq!(format_float(1_000_000_000_000_001.0), "1000000000000001");
        assert_eq!(format_float(1.234_567_890_123_455e16), "12345678901234550");
        // Past 1e21 the exact digits ride the exponential form, and f64::MAX
        // keeps all 17 of them.
        assert_eq!(
            format_float(1.234_567_890_123_456_7e21),
            "1.2345678901234568e+21"
        );
        assert_eq!(format_float(f64::MAX), "1.7976931348623157e+308");
        assert_eq!(format_float(f64::MIN), "-1.7976931348623157e+308");
    }

    #[test]
    fn format_scientific_small() {
        // Below 5e-7 should use scientific notation
        assert_eq!(format_float(1e-7), "1e-7");
        assert_eq!(format_float(5e-8), "5e-8");
    }

    #[test]
    fn format_decimal_range() {
        // Between 5e-7 and 1e21 should use decimal
        assert_eq!(format_float(0.000_001), "0.000001");
        assert_eq!(
            format_float(999_999_999_999_999_900_000.0),
            "999999999999999900000"
        );
    }

    /// Below 0.1 the cast applies like everywhere else (jsntrs-nyn).
    ///
    /// This test used to assert the opposite — that `[5e-7, 1e-6)` kept
    /// shortest round-trip digits, `$string(7/9000000)` being
    /// `"0.0000007777777777777778"`. That came from the Go pipeline's `'g'`
    /// threshold (`exp < -1`), which sent everything under 0.1 down a branch
    /// that never rounded, and it left the scalar path disagreeing with
    /// jsonata-js, with ECMAScript, *and* with jsntrs's own container path all
    /// at once. It was the only thing pinning the band, and it was wrong.
    ///
    /// Every expectation below is copied from a jsonata 2.2.2 sweep
    /// (2026-08-15, `require.cache` cleared per case).
    #[test]
    fn format_applies_the_cast_below_one_tenth() {
        for (n, expected) in [
            // The [5e-7, 1e-6) band the old test froze.
            (7.0_f64 / 9_000_000.0, "7.77777777777778e-7"),
            (0.000_000_5, "5e-7"),
            (0.000_000_999_999_999_999_999_7, "0.000001"),
            (0.000_001_234_567_890_123_456_7, "0.00000123456789012346"),
            (0.000_000_49, "4.9e-7"),
            // …and the rest of the decade range that escaped with it.
            (0.012_345_678_901_234_567, "0.0123456789012346"),
            (0.043_080_139_160_156_25, "0.0430801391601563"),
            (0.004_308_013_916_015_625, "0.00430801391601562"),
            (1.234_567_890_123_456_7e-10, "1.23456789012346e-10"),
            (3.980_498_358_697_587e-249, "3.98049835869759e-249"),
            (f64::from_bits(1), "5e-324"),
            (0.099_999_999_999_999_99, "0.1"),
            (1.0_f64 / 3.0, "0.333333333333333"),
            // The float-noise case the cast exists to absorb.
            (0.1 + 0.2, "0.3"),
        ] {
            assert_eq!(format_float(n), expected, "for {n:e}");
            assert_eq!(format_float(-n), format!("-{expected}"), "for -{n:e}");
        }
    }

    /// One number must render the same whether or not it sits in a container.
    ///
    /// The scalar arm ([`format_float`]) and the container arm
    /// (`Value::string_cast` plus the JSON writer) are separate code paths
    /// that have to agree; jsntrs-nyn is what happens when they stop. The
    /// values are the ones that used to disagree.
    #[test]
    fn scalar_and_container_arms_agree() {
        for n in [
            7.0_f64 / 9_000_000.0,
            0.012_345_678_901_234_567,
            0.043_080_139_160_156_25,
            0.004_308_013_916_015_625,
            1.234_567_890_123_456_7e-10,
            0.1 + 0.2,
            1.0 / 3.0,
            0.5,
            1.5,
            42.0,
            -0.000_000_777_777_777_777_777_8,
        ] {
            // What the container arm hands the JSON writer, rendered the way
            // the JSON writer renders it.
            let mut buf = ryu_js::Buffer::new();
            let via_container = buf.format_finite(string_cast_number(n)).to_owned();
            assert_eq!(format_float(n), via_container, "for {n:e}");
        }
    }

    #[test]
    fn format_matches_go_reference() {
        // case001: $string(22/7) — 15 significant digits
        assert_eq!(format_float(22.0_f64 / 7.0), "3.14285714285714");
        // case008: sum results
        assert_eq!(format_float(90.57), "90.57");
        assert_eq!(format_float(245.79), "245.79");
        // case018: 78.8 / 2
        assert_eq!(format_float(39.4), "39.4");
    }

    /// jsonata-js 2.2.2-verified (2026-08-15, jsntrs-1uk): a value whose exact
    /// decimal expansion is 16 significant digits ending in 5 is an exact tie
    /// for the `toPrecision(15)` cast, and ECMAScript breaks it *away from
    /// zero* — not to even, which is what Rust's `{:.14e}` does and what this
    /// used to inherit. One representative per decade of the band
    /// `format_float` prints positionally, each output copied from a sweep of
    /// the reference engine.
    #[test]
    fn format_ties_round_away_from_zero() {
        for (n, expected) in [
            (499_747_614_544_282.5, "499747614544283"),
            (100_000_000_000_000.5, "100000000000001"),
            (85_162_478_855_207.25, "85162478855207.3"),
            (5_500_000_000_000.125, "5500000000000.13"),
            (100_000_000_000.062_5, "100000000000.063"),
            (19_708_353_146.281_25, "19708353146.2813"),
            (4_307_710_549.890_625, "4307710549.89063"),
            (417_611_838.007_812_5, "417611838.007813"),
            (38_761_082.347_656_25, "38761082.3476563"),
            (5_500_000.001_953_125, "5500000.00195313"),
            (468_358.176_757_812_5, "468358.176757813"),
            (13_139.115_722_656_25, "13139.1157226563"),
            (2_878.845_947_265_625, "2878.84594726563"),
            (300.208_129_882_812_5, "300.208129882813"),
            (28.434_875_488_281_25, "28.4348754882813"),
            (1.000_030_517_578_125, "1.00003051757813"),
            (0.430_801_391_601_562_5, "0.430801391601563"),
            (0.999_984_741_210_937_5, "0.999984741210938"),
        ] {
            assert_eq!(format_float(n), expected, "for {n}");
            assert_eq!(format_float(-n), format!("-{expected}"), "for -{n}");
        }
        // The tie that carries out of the 15th digit into a 16th.
        assert_eq!(format_float(999_999_999_999_999.5), "1000000000000000");
        assert_eq!(format_float(-999_999_999_999_999.5), "-1000000000000000");
    }

    /// The doubles on either side of a tie are not ties, and must keep the
    /// plain nearest rounding. jsonata-js 2.2.2-verified (2026-08-15).
    #[test]
    fn format_near_ties_keep_nearest_rounding() {
        for (n, expected) in [
            (499_747_614_544_282.4, "499747614544282"),
            (499_747_614_544_282.44, "499747614544282"),
            (499_747_614_544_282.56, "499747614544283"),
            (499_747_614_544_282.6, "499747614544283"),
            (0.430_801_391_601_562_4, "0.430801391601562"),
            (0.430_801_391_601_562_44, "0.430801391601562"),
            (0.430_801_391_601_562_56, "0.430801391601563"),
            (28.434_875_488_281_243, "28.4348754882812"),
            (28.434_875_488_281_257, "28.4348754882813"),
            (999_999_999_999_999.4, "999999999999999"),
            (999_999_999_999_999.6, "1000000000000000"),
        ] {
            assert_eq!(format_float(n), expected, "for {n}");
        }
    }

    /// Exhaustive over the *shape* of the bug rather than a sample: every
    /// decade of the positional band gets a spread of exact ties, built from
    /// odd mantissas so the construction is guaranteed exact, and each is
    /// checked against the away-from-zero digits computed independently in
    /// `u128` integer arithmetic. `n = m / 2^k` with `m` odd has the exact
    /// decimal expansion `m·5^k / 10^k`, so the tie is real by construction.
    #[test]
    fn format_ties_match_exact_integer_rounding_across_the_band() {
        let mut checked = 0u32;
        for k in 1..=16u32 {
            let pow5 = 5u128.pow(k);
            let lo = 10u128.pow(15).div_ceil(pow5);
            let hi = 10u128.pow(16) / pow5;
            // Even step, odd start: every `m` visited stays odd, which is
            // what makes `m·5^k` end in 5.
            let step = (((hi - lo) / 61) & !1u128).max(2);
            let mut m = lo | 1;
            while m < hi {
                let significand = m * pow5;
                assert!((10u128.pow(15)..10u128.pow(16)).contains(&significand));
                // m < 2^53 and 2^k is exact, so the double is exactly m/2^k.
                let n = m as f64 / (1u64 << k) as f64;
                assert_eq!(
                    exact_15_digit_tie(n),
                    Some((significand, k)),
                    "tie not detected for {n}"
                );

                // Away from zero: drop the trailing 5, add one, then place
                // the decimal point k-1 digits from the right.
                let rounded = (significand / 10 + 1).to_string();
                let frac = (k - 1) as usize;
                let expected = if rounded.len() > frac {
                    let (int_part, frac_part) = rounded.split_at(rounded.len() - frac);
                    if frac_part.is_empty() {
                        int_part.to_owned()
                    } else {
                        format!("{int_part}.{frac_part}")
                            .trim_end_matches('0')
                            .trim_end_matches('.')
                            .to_owned()
                    }
                } else {
                    format!("0.{}{}", "0".repeat(frac - rounded.len()), rounded)
                        .trim_end_matches('0')
                        .to_owned()
                };

                assert_eq!(format_float(n), expected, "for {n} (k={k}, m={m})");
                assert_eq!(format_float(-n), format!("-{expected}"), "for -{n}");
                checked += 1;
                m += step;
            }
        }
        assert!(checked > 900, "band coverage too thin: {checked}");
    }

    /// The tie detector must not fire on values that are not exact ties:
    /// integers with an even trailing digit, expansions shorter or longer
    /// than 16 significant digits, and neighbours of real ties.
    #[test]
    fn exact_tie_detection_rejects_non_ties() {
        for n in [
            0.5,
            0.25,
            1.0,
            42.0,
            1e15,
            5_890_840_712_243_076.0,
            1_234_567_890_123_456.7,
            499_747_614_544_282.4,
            499_747_614_544_282.6,
            0.1,
            22.0_f64 / 7.0,
            f64::MIN_POSITIVE,
        ] {
            assert_eq!(exact_15_digit_tie(n.abs()), None, "false tie for {n}");
        }
        // 4997476145442825 is a 16-digit odd *integer* ending in 5: a real
        // tie, which the detector reports even though `format_float` never
        // 15-rounds an integer (jsonata-js leaves integers uncast).
        assert_eq!(
            exact_15_digit_tie(4_997_476_145_442_825.0),
            Some((4_997_476_145_442_825, 0))
        );
        assert_eq!(format_float(4_997_476_145_442_825.0), "4997476145442825");
    }
}
