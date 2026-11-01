//! Fractional order keys for the Pinned and Active sections
//! (`packages/client-runtime/src/state/threadSort.ts:165-261`): lowercase
//! strings compared as fractions in (0, 1) over the alphabet `a`–`z`.

use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
};

const DIGITS: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";
const BASE: usize = DIGITS.len();

fn digit(byte: u8) -> usize {
    usize::from(byte - b'a')
}

/// Non-empty, lowercase letters only, and not ending in `a`, which would
/// leave no room before it (`isValidPinOrderKey`, threadSort.ts:167-175).
fn is_valid(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase()) && !key.ends_with('a')
}

/// Midpoint of two digit strings; `""` is the open bound. Requires `a < b`
/// when `b` is not open (`pinOrderMidpoint`, threadSort.ts:177-193).
fn midpoint(a: &[u8], b: &[u8]) -> Vec<u8> {
    if !b.is_empty() {
        let mut n = 0;
        while n < b.len() && a.get(n).copied().unwrap_or(DIGITS[0]) == b[n] {
            n += 1;
        }
        if n > 0 {
            let mut key = b[..n].to_vec();
            key.extend(midpoint(a.get(n..).unwrap_or_default(), &b[n..]));
            return key;
        }
    }
    let digit_a = a.first().map_or(0, |&d| digit(d));
    let digit_b = b.first().map_or(BASE, |&d| digit(d));
    if digit_b - digit_a > 1 {
        // `Math.round` of a non-negative half rounds up.
        return vec![DIGITS[(digit_a + digit_b).div_ceil(2)]];
    }
    if b.len() > 1 {
        return vec![b[0]];
    }
    let mut key = vec![DIGITS[digit_a]];
    key.extend(midpoint(a.get(1..).unwrap_or_default(), b""));
    key
}

/// A key that sorts strictly between two neighbors; `None` bounds are the
/// top of the section and the end of its keyed run. `None` when a bound is
/// corrupt or the bounds are out of order; callers then rewrite the section
/// (`pinOrderKeyBetween`, threadSort.ts:195-206).
pub fn order_key_between(before: Option<&str>, after: Option<&str>) -> Option<String> {
    let a = before.unwrap_or("");
    let b = after.unwrap_or("");
    if (!a.is_empty() && !is_valid(a)) || (!b.is_empty() && !is_valid(b)) {
        return None;
    }
    if !b.is_empty() && a >= b {
        return None;
    }
    String::from_utf8(midpoint(a.as_bytes(), b.as_bytes())).ok()
}

/// `count` ascending keys spread evenly, each with room before it
/// (`generateSpreadPinOrderKeys`, threadSort.ts:208-228).
pub fn spread_order_keys(count: usize) -> Vec<String> {
    let mut width = 2;
    let mut space = (BASE * BASE) as u128;
    while space <= (count as u128 + 1) * 2 {
        width += 1;
        space *= BASE as u128;
    }
    let step = space as f64 / (count as f64 + 1.0);
    (0..count)
        .map(|index| {
            let mut value = (step * (index as f64 + 1.0)).round() as u128;
            if value.is_multiple_of(BASE as u128) {
                value += 1;
            }
            let mut key = vec![0; width];
            for slot in key.iter_mut().rev() {
                *slot = DIGITS[(value % BASE as u128) as usize];
                value /= BASE as u128;
            }
            String::from_utf8(key).expect("keys are ASCII")
        })
        .collect()
}

/// One order key write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderAssignment<K> {
    pub id: K,
    pub order_key: String,
}

/// The key writes that put `moved` where it sits in `ordered` (the visible
/// rows, already rearranged). `keys` holds every keyed row's key, hidden
/// rows' too, so their slots stay reserved; keyless rows are absent. One
/// midpoint write when the neighbors allow it, else fresh spread keys for
/// every visible row whose key changes (`planPinnedReorder`,
/// threadSort.ts:230-261). Used for both Pinned and Active.
pub fn plan_reorder<K: Eq + Hash + Clone>(
    ordered: &[K],
    keys: &HashMap<K, String>,
    moved: &K,
) -> Vec<OrderAssignment<K>> {
    let visible: HashSet<&K> = ordered.iter().collect();
    let reserved: HashSet<&str> = keys
        .iter()
        .filter(|(id, _)| !visible.contains(id))
        .map(|(_, key)| key.as_str())
        .collect();
    let Some(index) = ordered.iter().position(|id| id == moved) else {
        return Vec::new();
    };
    let before_id = index.checked_sub(1).map(|i| &ordered[i]);
    let after_id = ordered.get(index + 1);
    let before_key = before_id.and_then(|id| keys.get(id)).map(String::as_str);
    let after_key = after_id.and_then(|id| keys.get(id)).map(String::as_str);
    let before_usable = before_id.is_none() || before_key.is_some();
    let after_usable = after_id.is_none() || after_key.is_some();
    if before_usable && after_usable {
        let mut key = order_key_between(before_key, after_key);
        while let Some(candidate) = key.as_deref().filter(|key| reserved.contains(key)) {
            key = order_key_between(Some(candidate), after_key);
        }
        if let Some(order_key) = key {
            return vec![OrderAssignment {
                id: moved.clone(),
                order_key,
            }];
        }
    }
    let spread: Vec<String> = spread_order_keys(ordered.len() + reserved.len())
        .into_iter()
        .filter(|key| !reserved.contains(key.as_str()))
        .take(ordered.len())
        .collect();
    ordered
        .iter()
        .zip(spread)
        .filter(|(id, key)| keys.get(*id) != Some(key))
        .map(|(id, order_key)| OrderAssignment {
            id: id.clone(),
            order_key,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- pinOrderKeyBetween (Sidebar.logic.test.ts:873-904) ---

    #[test]
    fn stays_strictly_ordered_under_repeated_top_insertion() {
        let mut head: Option<String> = None;
        let mut keys = HashSet::new();
        for _ in 0..100 {
            let key = order_key_between(None, head.as_deref()).expect("a key");
            if let Some(head) = &head {
                assert!(key < *head);
            }
            keys.insert(key.clone());
            head = Some(key);
        }
        assert_eq!(keys.len(), 100);
    }

    #[test]
    fn stays_strictly_ordered_under_repeated_middle_insertion() {
        let mut low = order_key_between(None, None).unwrap();
        let mut high = order_key_between(Some(&low), None).unwrap();
        for i in 0..100 {
            let key = order_key_between(Some(&low), Some(&high)).unwrap();
            assert!(low < key && key < high);
            if i % 2 == 0 {
                low = key;
            } else {
                high = key;
            }
        }
    }

    #[test]
    fn returns_none_for_corrupt_or_out_of_order_bounds() {
        assert_eq!(order_key_between(Some("z"), Some("a")), None);
        assert_eq!(order_key_between(Some("A!"), None), None);
        assert_eq!(order_key_between(None, Some("ma")), None);
        assert_eq!(order_key_between(Some("m"), Some("m")), None);
    }

    // --- planPinnedReorder with hidden rows (threadSort.test.ts:177-213) ---

    fn keys(entries: &[(&'static str, &str)]) -> HashMap<&'static str, String> {
        entries
            .iter()
            .map(|(id, key)| (*id, (*key).to_owned()))
            .collect()
    }

    #[test]
    fn keeps_hidden_slots_available_between_visible_neighbors() {
        let midpoint = order_key_between(Some("f"), Some("t")).unwrap();
        let keys = keys(&[
            ("a", "f"),
            ("b", "t"),
            ("moved", "z"),
            ("snoozed", &midpoint),
        ]);
        let assignments = plan_reorder(&["a", "moved", "b"], &keys, &"moved");
        assert_eq!(assignments.len(), 1);
        let key = &assignments[0].order_key;
        assert!(key.as_str() > "f" && key.as_str() < "t");
        assert_ne!(*key, midpoint);
        assert_eq!(assignments[0].id, "moved");
    }

    #[test]
    fn materializes_keyless_rows_without_overwriting_hidden_slots() {
        let reserved = spread_order_keys(6);
        let ids = [
            "hidden-0", "hidden-1", "hidden-2", "hidden-3", "hidden-4", "hidden-5",
        ];
        let keys: HashMap<&str, String> = ids.into_iter().zip(reserved.iter().cloned()).collect();
        let assignments = plan_reorder(&["c", "a", "b"], &keys, &"c");
        assert_eq!(
            assignments.iter().map(|a| a.id).collect::<Vec<_>>(),
            ["c", "a", "b"]
        );
        let written: Vec<&String> = assignments.iter().map(|a| &a.order_key).collect();
        let mut sorted = written.clone();
        sorted.sort();
        assert_eq!(written, sorted);
        assert_eq!(written.iter().collect::<HashSet<_>>().len(), 3);
        assert!(written.iter().all(|key| !reserved.contains(key)));
    }

    // --- generateSpreadPinOrderKeys (threadSort.test.ts:215-234) ---

    #[test]
    fn spread_keys_are_unique_and_insertable() {
        for count in [0, 1, 650, 675, 676, 1_001, 2_000] {
            let keys = spread_order_keys(count);
            assert_eq!(keys.len(), count);
            assert_eq!(keys.iter().collect::<HashSet<_>>().len(), count);
            let mut sorted = keys.clone();
            sorted.sort();
            assert_eq!(sorted, keys);
            for (index, after) in keys.iter().enumerate() {
                let before = index.checked_sub(1).map(|i| keys[i].as_str());
                assert!(after.bytes().all(|b| b.is_ascii_lowercase()) && !after.ends_with('a'));
                let between = order_key_between(before, Some(after)).expect("room before");
                assert!(between < *after);
                if let Some(before) = before {
                    assert!(between.as_str() > before);
                }
            }
        }
    }
}
