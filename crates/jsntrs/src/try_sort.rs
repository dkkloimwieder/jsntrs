//! Fallible stable sort for comparators that can raise JSONata errors.
//!
//! `slice::sort_by` panics ("comparison function does not correctly
//! implement a total order") when a comparator turns inconsistent — which
//! the error-latching pattern (return `Equal` after an error) and the
//! JSONata `$sort` comparator protocol (truthy → `Less`, falsy → `Equal`,
//! never `Greater`) both can do. Go's `sort.SliceStable` tolerates such
//! comparators; this bottom-up merge sort restores that tolerance and
//! propagates the first comparator error immediately instead of latching.

use std::cmp::Ordering;

/// Stable-sort `items`, propagating the first comparator error.
///
/// Equal elements keep their original relative order. Comparators that
/// are not a total order produce an unspecified (but panic-free) order,
/// matching Go's `sort.SliceStable`.
///
/// # Errors
/// Returns the first error the comparator raises; `items` is consumed
/// either way.
pub(crate) fn try_sort_by<T, E>(
    items: Vec<T>,
    mut cmp: impl FnMut(&T, &T) -> Result<Ordering, E>,
) -> Result<Vec<T>, E> {
    let n = items.len();
    if n < 2 {
        return Ok(items);
    }

    // Sort a permutation of indices, then apply it. u32 is plenty: Value
    // arrays are bounded far below 4B elements (e.g. $append's 10M cap).
    let mut idx: Vec<u32> = (0..n as u32).collect();
    let mut buf: Vec<u32> = vec![0; n];

    let mut width = 1usize;
    while width < n {
        let mut start = 0usize;
        while start < n {
            let mid = usize::min(start + width, n);
            let end = usize::min(start + 2 * width, n);
            let (mut i, mut j, mut k) = (start, mid, start);
            while i < mid && j < end {
                let (a, b) = (idx[i] as usize, idx[j] as usize);
                // Take from the right run only when its head is strictly
                // Less than the left head (Go: `less(j, i)`). Keying on
                // the Less signal — not Greater — matters because the
                // JSONata comparator protocol never returns Greater; ties
                // take from the left run, which is what makes this stable.
                if cmp(&items[b], &items[a])? == Ordering::Less {
                    buf[k] = idx[j];
                    j += 1;
                } else {
                    buf[k] = idx[i];
                    i += 1;
                }
                k += 1;
            }
            buf[k..k + (mid - i)].copy_from_slice(&idx[i..mid]);
            let k = k + (mid - i);
            buf[k..k + (end - j)].copy_from_slice(&idx[j..end]);
            start = end;
        }
        std::mem::swap(&mut idx, &mut buf);
        width *= 2;
    }

    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    Ok(idx
        .into_iter()
        .map(|i| match slots[i as usize].take() {
            Some(item) => item,
            None => unreachable!("sorted index permutation visits each slot exactly once"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_like_std_for_consistent_comparators() {
        for n in [0usize, 1, 2, 3, 7, 50, 257] {
            // Deterministic scrambled input.
            let items: Vec<u64> = (0..n as u64).map(|i| (i * 48271) % 101).collect();
            let mut expected = items.clone();
            expected.sort_unstable();
            let got = try_sort_by(items, |a, b| Ok::<_, ()>(a.cmp(b))).unwrap();
            assert_eq!(got, expected, "n={n}");
        }
    }

    #[test]
    fn stable_for_equal_keys() {
        // Sort by the key only; the payload records original order.
        let items: Vec<(u8, usize)> = (0..100).map(|i| ((i % 3) as u8, i)).collect();
        let got = try_sort_by(items, |a, b| Ok::<_, ()>(a.0.cmp(&b.0))).unwrap();
        for w in got.windows(2) {
            assert!(w[0].0 < w[1].0 || (w[0].0 == w[1].0 && w[0].1 < w[1].1));
        }
    }

    #[test]
    fn propagates_first_error() {
        let items: Vec<i32> = (0..64).rev().collect();
        let mut calls = 0;
        let err = try_sort_by(items, |a, b| {
            calls += 1;
            if calls == 10 {
                Err("boom")
            } else {
                Ok(a.cmp(b))
            }
        })
        .unwrap_err();
        assert_eq!(err, "boom");
    }

    #[test]
    fn never_greater_comparator_is_panic_free() {
        // The JSONata comparator protocol maps falsy to Equal and truthy
        // to Less — never Greater. std sort may detect this as a total-
        // order violation and panic; try_sort_by must not.
        let items: Vec<i32> = (0..200).map(|i| (i * 7) % 13).collect();
        let got = try_sort_by(items, |_, _| Ok::<_, ()>(Ordering::Less)).unwrap();
        assert_eq!(got.len(), 200);
    }
}
