//! The iteration order of a Java `HashMap<String, _>`.
//!
//! The official emulator gathers masked field names, and the fields a mask changes or returns,
//! in such maps, and the fields come out in their order (#108, `tools/oracle/field_order.py`).

use std::collections::{HashMap, HashSet};

/// Java's `String.hashCode`: base 31 over UTF-16 code units, wrapping.
fn string_hash(s: &str) -> u32 {
    s.encode_utf16().fold(0u32, |h, unit| {
        h.wrapping_mul(31).wrapping_add(u32::from(unit))
    })
}

/// `HashMap.hash`: the high half folded into the low one.
fn spread(h: u32) -> u32 {
    h ^ (h >> 16)
}

const DEFAULT_CAPACITY: u32 = 16;
/// A bucket reaching this many keys makes a table smaller than 64 double instead of turning
/// the bucket into a tree (`TREEIFY_THRESHOLD + 1`, `MIN_TREEIFY_CAPACITY`).
const CROWDED_BUCKET: usize = 9;
const MIN_TREE_CAPACITY: u32 = 64;

/// The positions in `keys` in the order a `java.util.HashMap` created empty with the default
/// capacity iterates them after receiving `keys` in turn: by bucket, and by insertion within a
/// bucket. A key seen again keeps its first place. The table doubles when it holds more than
/// 3/4 of its capacity, or when a bucket gets crowded while it is small; buckets that Java turns
/// into trees (64 buckets or more, nine keys in one) are not modelled and keep insertion order.
pub fn hash_order<S: AsRef<str>>(keys: &[S]) -> Vec<usize> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut entries: Vec<(usize, u32)> = Vec::with_capacity(keys.len());
    for (i, key) in keys.iter().enumerate() {
        let key = key.as_ref();
        if seen.insert(key) {
            entries.push((i, spread(string_hash(key))));
        }
    }

    let mut capacity = DEFAULT_CAPACITY;
    let mut buckets: HashMap<u32, usize> = HashMap::new();
    for (count, &(_, hash)) in entries.iter().enumerate() {
        let filled = buckets.entry(hash & (capacity - 1)).or_default();
        *filled += 1;
        let crowded = *filled >= CROWDED_BUCKET && capacity < MIN_TREE_CAPACITY;
        let mut grow = |capacity: &mut u32| {
            *capacity *= 2;
            buckets.clear();
            for &(_, hash) in &entries[..=count] {
                *buckets.entry(hash & (*capacity - 1)).or_default() += 1;
            }
        };
        if crowded {
            grow(&mut capacity);
        }
        if count + 1 > (capacity / 4 * 3) as usize {
            grow(&mut capacity);
        }
    }

    let mut ordered = entries;
    ordered.sort_by_key(|&(i, hash)| (hash & (capacity - 1), i));
    ordered.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order<'a>(keys: &[&'a str]) -> Vec<&'a str> {
        hash_order(keys).into_iter().map(|i| keys[i]).collect()
    }

    #[test]
    fn hashes_like_java() {
        assert_eq!(string_hash("str"), 114_225);
        assert_eq!(string_hash("zeta"), 3_735_256);
        assert_eq!(string_hash("alpha"), 92_909_918);
        // UTF-16: the second unit of a surrogate pair counts on its own.
        assert_eq!(string_hash("\u{1F600}"), 0xD83D * 31 + 0xDE00);
    }

    #[test]
    fn orders_like_the_official_emulator() {
        // Read masks recorded in crates/hidane/tests/fixtures/field_order.json.
        assert_eq!(order(&["int", "str"]), ["str", "int"]);
        assert_eq!(order(&["dbl", "int"]), ["int", "dbl"]);
        assert_eq!(order(&["a", "alpha", "zeta"]), ["zeta", "a", "alpha"]);
        // Same bucket: insertion order.
        assert_eq!(order(&["q", "a"]), ["q", "a"]);
        assert_eq!(order(&["a", "q"]), ["a", "q"]);
        // Twelve keys fit 16 buckets, the thirteenth doubles the table.
        let twelve = [
            "str", "int", "dbl", "zeta", "alpha", "a", "b", "c", "mid", "x1", "x2", "x3",
        ];
        assert_eq!(
            order(&twelve),
            [
                "str", "zeta", "a", "b", "c", "alpha", "mid", "x1", "x2", "x3", "int", "dbl"
            ]
        );
        let thirteen = [&twelve[..], &["x4"]].concat();
        assert_eq!(
            order(&thirteen),
            [
                "zeta", "a", "b", "c", "mid", "int", "dbl", "str", "alpha", "x1", "x2", "x3", "x4"
            ]
        );
        assert_eq!(
            order(&["x.y", "\u{65e5}\u{672c}", "\u{e9}t\u{e9}", "Z", "zeta"]),
            ["zeta", "x.y", "Z", "\u{65e5}\u{672c}", "\u{e9}t\u{e9}"]
        );
    }

    #[test]
    fn repeated_keys_keep_their_first_place() {
        assert_eq!(hash_order(&["b", "a", "b"]), [1, 0]);
    }

    #[test]
    fn a_crowded_bucket_doubles_a_small_table() {
        // All in one of 16 buckets, split between two of 32. Eight keep insertion order; the
        // ninth crowds the bucket, the table doubles, and the halves separate.
        let keys = [
            "k17", "k309", "k28", "k408", "k39", "k419", "k150", "k507", "k161",
        ];
        assert_eq!(order(&keys[..8]), keys[..8]);
        assert_eq!(
            order(&keys),
            [
                "k309", "k408", "k419", "k507", "k17", "k28", "k39", "k150", "k161"
            ]
        );
    }
}
