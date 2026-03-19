use super::*;
use ic_stable_structures::VectorMemory;
use proptest::collection::vec as prop_vec;
use proptest::prelude::*;
use std::collections::BTreeMap as StdBTreeMap;

#[derive(Debug, Clone)]
enum Op {
    Insert(u64, u64),
    Remove(u64),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u64..100, any::<u64>()).prop_map(|(k, v)| Op::Insert(k, v)),
        (0u64..100).prop_map(Op::Remove),
    ]
}

type TestMap = RepLogMap<u64, u64, VectorMemory, VectorMemory, VectorMemory, VectorMemory, VectorMemory>;

fn make_test_map() -> TestMap {
    RepLogMap::init(
        VectorMemory::default(),
        VectorMemory::default(),
        VectorMemory::default(),
        VectorMemory::default(),
        VectorMemory::default(),
    )
}

fn apply_ops(map: &mut TestMap, ops: &[Op]) -> StdBTreeMap<u64, u64> {
    let mut expected = StdBTreeMap::new();
    for op in ops {
        match op {
            Op::Insert(k, v) => {
                map.insert(*k, *v);
                expected.insert(*k, *v);
            }
            Op::Remove(k) => {
                map.remove(k);
                expected.remove(k);
            }
        }
    }
    expected
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn key_index_and_map_consistent(ops in prop_vec(op_strategy(), 0..200)) {
        let mut map = make_test_map();
        let expected = apply_ops(&mut map, &ops);

        prop_assert_eq!(map.len(), expected.len() as u64);

        for (k, v) in &expected {
            prop_assert_eq!(map.get(k), Some(*v));
        }

        for k in 0u64..100 {
            if !expected.contains_key(&k) {
                prop_assert_eq!(map.get(&k), None);
            }
        }
    }

    #[test]
    fn changes_since_replayed_reproduces_map(ops in prop_vec(op_strategy(), 0..200)) {
        let mut map = make_test_map();
        let expected = apply_ops(&mut map, &ops);

        let (changes, _) = map.changes_since(0, u64::MAX).unwrap();

        let mut replay: StdBTreeMap<u64, u64> = StdBTreeMap::new();
        for change in &changes {
            if change.is_upsert() {
                if let Some(v) = map.get(&change.key) {
                    replay.insert(change.key, v);
                }
            } else {
                replay.remove(&change.key);
            }
        }

        let original: StdBTreeMap<u64, u64> = map.iter().collect();
        prop_assert_eq!(&replay, &original);
        prop_assert_eq!(&replay, &expected);
    }

    #[test]
    fn compact_resync_equivalence(ops in prop_vec(op_strategy(), 1..200)) {
        let mut map = make_test_map();
        apply_ops(&mut map, &ops);

        let had_changes = map.current_seq() > 0;
        map.compact();

        if had_changes {
            prop_assert_eq!(map.changes_since(0, 1), None);
        }

        let mut snapshot_entries = Vec::new();
        let mut after_key: Option<u64> = None;
        loop {
            let (page, _) = map.snapshot_page(after_key.as_ref(), 10);
            if page.is_empty() {
                break;
            }
            after_key = Some(page.last().unwrap().1);
            for (_, k, v) in page {
                snapshot_entries.push((k, v));
            }
        }

        let original: Vec<(u64, u64)> = map.iter().collect();
        prop_assert_eq!(snapshot_entries, original);
    }
}
