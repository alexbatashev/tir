//! `IdMap` against a `BTreeMap` model.

use std::collections::BTreeMap;

use tir::{IdMap, ValueId};

proptest::proptest! {
    /// Whatever was inserted, removed and defaulted, the map holds what the
    /// model holds and lists it in ascending id order, across chunks.
    #[test]
    fn an_id_map_agrees_with_an_ordered_map(
        edits in proptest::collection::vec((0u32..3, 0u32..9000, 0u32..1000), 0..300),
    ) {
        let mut map: IdMap<ValueId, u32> = IdMap::new();
        let mut model: BTreeMap<u32, u32> = BTreeMap::new();
        for (kind, id, value) in edits {
            let key = ValueId::from_number(id);
            match kind {
                0 => proptest::prop_assert_eq!(map.insert(key, value), model.insert(id, value)),
                1 => proptest::prop_assert_eq!(map.remove(&key), model.remove(&id)),
                _ => *map.entry(key).or_insert(value) += 1,
            }
            if kind == 2 {
                *model.entry(id).or_insert(value) += 1;
            }
            proptest::prop_assert_eq!(map.get(&key), model.get(&id));
            proptest::prop_assert_eq!(map.len(), model.len());
        }
        let listed: Vec<(u32, u32)> = map.iter().map(|(key, value)| (key.number(), *value)).collect();
        let expected: Vec<(u32, u32)> = model.into_iter().collect();
        proptest::prop_assert_eq!(listed, expected);
    }
}
