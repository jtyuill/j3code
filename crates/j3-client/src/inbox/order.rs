//! Order keys for the Pinned and Active sections (`docs/triage.md` §5.4).
//!
//! A key is a string over `a`–`z` read as a base-26 fraction in (0, 1);
//! keys compare as strings.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

const BASE: u32 = 26;

/// A valid key is non-empty, lowercase ASCII letters only, and does not end
/// in `a` (so there is always room before it).
pub fn is_valid_order(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase()) && !key.ends_with('a')
}

/// A key strictly between `before` and `after`, either of which may be open.
/// None when a given bound is invalid or `before` is not below `after`.
pub fn order_between(before: Option<&str>, after: Option<&str>) -> Option<String> {
    if before.is_some_and(|key| !is_valid_order(key))
        || after.is_some_and(|key| !is_valid_order(key))
    {
        return None;
    }
    if let (Some(before), Some(after)) = (before, after)
        && before >= after
    {
        return None;
    }
    let mut key = Vec::new();
    between(
        before.unwrap_or("").as_bytes(),
        after.map(str::as_bytes),
        &mut key,
    );
    Some(String::from_utf8(key).expect("order keys are ASCII"))
}

fn digit(byte: u8) -> u32 {
    u32::from(byte - b'a')
}

fn letter(digit: u32) -> u8 {
    b'a' + u8::try_from(digit).expect("a base-26 digit")
}

/// Appends to `out` a key between `before` (empty = 0) and `after`
/// (`None` = open), given `before < after`.
fn between(before: &[u8], after: Option<&[u8]>, out: &mut Vec<u8>) {
    if let Some(after) = after {
        // The common prefix, where a missing digit of `before` reads as `a`.
        let shared = after
            .iter()
            .enumerate()
            .take_while(|&(i, &b)| before.get(i).copied().unwrap_or(b'a') == b)
            .count();
        if shared > 0 {
            out.extend_from_slice(&after[..shared]);
            return between(
                before.get(shared..).unwrap_or(&[]),
                Some(&after[shared..]),
                out,
            );
        }
    }
    let low = before.first().map_or(0, |&b| digit(b));
    let high = after
        .and_then(|after| after.first())
        .map_or(BASE, |&b| digit(b));
    if high - low > 1 {
        out.push(letter((low + high).div_ceil(2)));
    } else if let Some(after) = after.filter(|after| after.len() > 1) {
        out.push(after[0]);
    } else {
        out.push(letter(low));
        between(before.get(1..).unwrap_or(&[]), None, out);
    }
}

/// `count` evenly spread keys, ascending, each with room before it.
pub fn spread_orders(count: usize) -> Vec<String> {
    let slots = count as u128 + 1;
    let mut width = 2u32;
    while 26u128.pow(width) <= 2 * slots {
        width += 1;
    }
    let span = 26u128.pow(width);
    (1..slots)
        .map(|i| {
            // round(span * i / slots), halves up.
            let mut value = (2 * span * i + slots) / (2 * slots);
            if value.is_multiple_of(26) {
                value += 1;
            }
            let mut key = vec![b'a'; width as usize];
            for slot in key.iter_mut().rev() {
                *slot = b'a' + (value % 26) as u8;
                value /= 26;
            }
            String::from_utf8(key).expect("order keys are ASCII")
        })
        .collect()
}

/// One key write of a reorder plan.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OrderAssignment<K> {
    pub id: K,
    pub order: String,
}

/// The key writes for moving `moved` within a section.
///
/// `order` is the section's visible rows after the move; `keys` holds the
/// current key of every keyed row of the section, including rows hidden from
/// view (filtered out, collapsed). Hidden rows' keys are reserved and never
/// reused.
///
/// When each neighbour of `moved` is absent or keyed, `moved` alone gets a
/// key between them (stepping past reserved keys). Otherwise the visible rows
/// are respread and only the keys that change are written.
pub fn plan_reorder<K: Eq + Hash + Clone>(
    order: &[K],
    keys: &HashMap<K, String>,
    moved: &K,
) -> Vec<OrderAssignment<K>> {
    let Some(position) = order.iter().position(|id| id == moved) else {
        return Vec::new();
    };
    let visible: HashSet<&K> = order.iter().collect();
    let reserved: HashSet<&str> = keys
        .iter()
        .filter(|(id, _)| !visible.contains(id))
        .map(|(_, key)| key.as_str())
        .collect();

    // A neighbour's key: Ok(None) when there is no neighbour, Err when it is keyless.
    let neighbour = |index: Option<usize>| match index.and_then(|index| order.get(index)) {
        None => Ok(None),
        Some(id) => keys.get(id).map(|key| Some(key.as_str())).ok_or(()),
    };
    if let (Ok(before), Ok(after)) = (
        neighbour(position.checked_sub(1)),
        neighbour(Some(position + 1)),
    ) {
        let mut key = order_between(before, after);
        while let Some(taken) = key.as_deref().filter(|key| reserved.contains(key)) {
            key = order_between(Some(taken), after);
        }
        if let Some(key) = key {
            if keys.get(moved) == Some(&key) {
                return Vec::new();
            }
            return vec![OrderAssignment {
                id: moved.clone(),
                order: key,
            }];
        }
    }

    spread_orders(order.len() + reserved.len())
        .into_iter()
        .filter(|key| !reserved.contains(key.as_str()))
        .zip(order)
        .filter(|(key, id)| keys.get(*id) != Some(key))
        .map(|(order, id)| OrderAssignment {
            id: id.clone(),
            order,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyed(pairs: &[(u32, &str)]) -> HashMap<u32, String> {
        pairs
            .iter()
            .map(|&(id, key)| (id, key.to_owned()))
            .collect()
    }

    fn apply(keys: &mut HashMap<u32, String>, plan: Vec<OrderAssignment<u32>>) {
        for assignment in plan {
            keys.insert(assignment.id, assignment.order);
        }
    }

    fn sorted_by_key(keys: &HashMap<u32, String>, ids: &[u32]) -> Vec<u32> {
        let mut ids = ids.to_vec();
        ids.sort_by(|a, b| keys[a].cmp(&keys[b]));
        ids
    }

    #[test]
    fn valid_keys() {
        assert!(is_valid_order("n"));
        assert!(is_valid_order("ab"));
        assert!(!is_valid_order(""));
        assert!(!is_valid_order("a"));
        assert!(!is_valid_order("ba"));
        assert!(!is_valid_order("Bc"));
        assert!(!is_valid_order("b1"));
    }

    #[test]
    fn keys_between_bounds() {
        assert_eq!(order_between(None, None).as_deref(), Some("n"));
        assert_eq!(order_between(Some("y"), None).as_deref(), Some("z"));
        assert_eq!(order_between(Some("z"), None).as_deref(), Some("zn"));
        assert_eq!(order_between(None, Some("b")).as_deref(), Some("an"));
        assert_eq!(order_between(None, Some("ab")).as_deref(), Some("aan"));
        assert_eq!(order_between(Some("b"), Some("d")).as_deref(), Some("c"));
        assert_eq!(order_between(Some("b"), Some("c")).as_deref(), Some("bn"));
        assert_eq!(order_between(Some("b"), Some("cd")).as_deref(), Some("c"));
        assert_eq!(order_between(Some("bz"), Some("c")).as_deref(), Some("bzn"));
    }

    #[test]
    fn returns_none_for_corrupt_or_out_of_order_bounds() {
        assert_eq!(order_between(Some(""), None), None);
        assert_eq!(order_between(Some("ba"), None), None);
        assert_eq!(order_between(None, Some("B")), None);
        assert_eq!(order_between(Some("c"), Some("c")), None);
        assert_eq!(order_between(Some("d"), Some("c")), None);
    }

    #[test]
    fn stays_strictly_ordered_under_repeated_top_insertion() {
        let mut first = order_between(None, None).unwrap();
        for _ in 0..500 {
            let key = order_between(None, Some(&first)).unwrap();
            assert!(is_valid_order(&key));
            assert!(key < first, "{key} < {first}");
            first = key;
        }
    }

    #[test]
    fn stays_strictly_ordered_under_repeated_middle_insertion() {
        let low = "c".to_owned();
        let mut high = "d".to_owned();
        let mut previous_low = low.clone();
        for i in 0..500 {
            let key = order_between(Some(&previous_low), Some(&high)).unwrap();
            assert!(is_valid_order(&key));
            assert!(
                previous_low < key && key < high,
                "{previous_low} < {key} < {high}"
            );
            if i % 2 == 0 {
                high = key
            } else {
                previous_low = key
            }
        }
    }

    #[test]
    fn spread_keys_are_unique_and_insertable() {
        for count in [0, 1, 2, 25, 26, 337, 338, 1200] {
            let keys = spread_orders(count);
            assert_eq!(keys.len(), count);
            let mut previous: Option<&str> = None;
            for key in &keys {
                assert!(is_valid_order(key), "{key}");
                assert!(previous.is_none_or(|previous| previous < key.as_str()));
                assert!(
                    order_between(previous, Some(key)).is_some(),
                    "room before {key}"
                );
                previous = Some(key);
            }
        }
        assert_eq!(spread_orders(1)[0].len(), 2);
    }

    #[test]
    fn moves_between_keyed_neighbours_write_one_key() {
        let keys = keyed(&[(1, "b"), (2, "d"), (3, "f")]);
        let plan = plan_reorder(&[1, 3, 2], &keys, &3);
        assert_eq!(
            plan,
            vec![OrderAssignment {
                id: 3,
                order: "c".to_owned()
            }]
        );
        let top = plan_reorder(&[3, 1, 2], &keys, &3);
        assert_eq!(
            top,
            vec![OrderAssignment {
                id: 3,
                order: "an".to_owned()
            }]
        );
    }

    #[test]
    fn keeps_hidden_slots_available_between_visible_neighbors() {
        // Row 9 is hidden at "c", exactly the midpoint of the visible neighbours.
        let keys = keyed(&[(1, "b"), (2, "d"), (3, "x"), (9, "c")]);
        let plan = plan_reorder(&[1, 3, 2], &keys, &3);
        assert_eq!(plan.len(), 1);
        let key = &plan[0].order;
        assert_ne!(key, "c");
        assert!("b" < key.as_str() && key.as_str() < "d");
    }

    #[test]
    fn materializes_keyless_rows_without_overwriting_hidden_slots() {
        let spread = spread_orders(5);
        // Hidden rows hold the 2nd and 4th spread keys of a five-row spread.
        let keys: HashMap<u32, String> = [(8, spread[1].clone()), (9, spread[3].clone())].into();
        let order = [1, 2, 3];
        let plan = plan_reorder(&order, &keys, &2);
        assert_eq!(plan.len(), 3);
        let mut all = keys.clone();
        apply(&mut all, plan);
        let hidden: HashSet<&String> = [&keys[&8], &keys[&9]].into();
        for id in order {
            assert!(!hidden.contains(&all[&id]), "row {id} reused a hidden key");
        }
        assert_eq!(sorted_by_key(&all, &order), order);
    }

    #[test]
    fn rebalancing_writes_only_changed_keys() {
        let spread = spread_orders(3);
        let keys: HashMap<u32, String> = [(1, spread[0].clone()), (3, spread[2].clone())].into();
        // Row 2 is keyless, so the move respreads, and rows 1 and 3 keep theirs.
        let plan = plan_reorder(&[1, 2, 3], &keys, &1);
        assert_eq!(
            plan,
            vec![OrderAssignment {
                id: 2,
                order: spread[1].clone()
            }]
        );
    }

    #[test]
    fn materializes_a_large_active_list_in_the_requested_order() {
        let order: Vec<u32> = (0..1200).rev().collect();
        let mut keys = HashMap::new();
        let plan = plan_reorder(&order, &keys, &600);
        assert_eq!(plan.len(), 1200);
        apply(&mut keys, plan);
        assert_eq!(sorted_by_key(&keys, &order), order);
    }

    #[test]
    fn a_row_outside_the_order_plans_nothing() {
        assert!(plan_reorder(&[1, 2], &HashMap::new(), &3).is_empty());
    }
}
