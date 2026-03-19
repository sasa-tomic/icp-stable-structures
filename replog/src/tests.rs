use super::*;
use ic_stable_structures::VectorMemory;

fn make_mem() -> VectorMemory {
    VectorMemory::default()
}

fn make_replog() -> RepLogMap<
    Vec<u8>,
    Vec<u8>,
    VectorMemory,
    VectorMemory,
    VectorMemory,
    VectorMemory,
    VectorMemory,
> {
    RepLogMap::init(make_mem(), make_mem(), make_mem(), make_mem(), make_mem())
}

fn key(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

fn val(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

// --- Basic CRUD ---

#[test]
fn insert_and_get() {
    let mut map = make_replog();
    assert!(map.is_empty());

    assert_eq!(map.insert(key("alice"), val("100")), None);
    assert_eq!(map.get(&key("alice")), Some(val("100")));
    assert_eq!(map.len(), 1);
}

#[test]
fn insert_overwrite_returns_old() {
    let mut map = make_replog();
    map.insert(key("alice"), val("100"));
    let old = map.insert(key("alice"), val("200"));
    assert_eq!(old, Some(val("100")));
    assert_eq!(map.get(&key("alice")), Some(val("200")));
    assert_eq!(map.len(), 1);
}

#[test]
fn remove_returns_value() {
    let mut map = make_replog();
    map.insert(key("alice"), val("100"));
    assert_eq!(map.remove(&key("alice")), Some(val("100")));
    assert_eq!(map.get(&key("alice")), None);
    assert!(map.is_empty());
}

#[test]
fn remove_nonexistent_returns_none() {
    let mut map = make_replog();
    assert_eq!(map.remove(&key("nobody")), None);
}

#[test]
fn contains_key() {
    let mut map = make_replog();
    assert!(!map.contains_key(&key("alice")));
    map.insert(key("alice"), val("100"));
    assert!(map.contains_key(&key("alice")));
    map.remove(&key("alice"));
    assert!(!map.contains_key(&key("alice")));
}

#[test]
fn first_last_key_value() {
    let mut map = make_replog();
    assert_eq!(map.first_key_value(), None);
    assert_eq!(map.last_key_value(), None);

    map.insert(key("b"), val("2"));
    map.insert(key("a"), val("1"));
    map.insert(key("c"), val("3"));

    assert_eq!(map.first_key_value(), Some((key("a"), val("1"))));
    assert_eq!(map.last_key_value(), Some((key("c"), val("3"))));
}

// --- Iteration ---

#[test]
fn iter_in_key_order() {
    let mut map = make_replog();
    map.insert(key("c"), val("3"));
    map.insert(key("a"), val("1"));
    map.insert(key("b"), val("2"));

    let entries: Vec<_> = map.iter().collect();
    assert_eq!(
        entries,
        vec![
            (key("a"), val("1")),
            (key("b"), val("2")),
            (key("c"), val("3")),
        ]
    );
}

#[test]
fn range_iteration() {
    let mut map = make_replog();
    for i in 0u8..10 {
        map.insert(vec![i], vec![i * 10]);
    }

    let entries: Vec<_> = map.range(vec![3u8]..vec![7u8]).collect();
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0], (vec![3u8], vec![30u8]));
    assert_eq!(entries[3], (vec![6u8], vec![60u8]));
}

#[test]
fn keys_and_values() {
    let mut map = make_replog();
    map.insert(key("b"), val("2"));
    map.insert(key("a"), val("1"));

    let keys: Vec<_> = map.keys().collect();
    assert_eq!(keys, vec![key("a"), key("b")]);

    let values: Vec<_> = map.values().collect();
    assert_eq!(values, vec![val("1"), val("2")]);
}

// --- Replication: current_seq ---

#[test]
fn current_seq_advances() {
    let mut map = make_replog();
    assert_eq!(map.current_seq(), 0);

    map.insert(key("a"), val("1"));
    assert_eq!(map.current_seq(), 1);

    map.insert(key("a"), val("2")); // update
    assert_eq!(map.current_seq(), 2);

    map.remove(&key("a"));
    assert_eq!(map.current_seq(), 3);
}

// --- Replication: changes_since ---

#[test]
fn changes_since_returns_all_entries() {
    let mut map = make_replog();
    map.insert(key("alice"), val("100"));
    map.insert(key("bob"), val("200"));

    let (entries, next_seq) = map.changes_since(0, 100).unwrap();
    assert_eq!(next_seq, 2);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].key, key("alice"));
    assert_eq!(entries[1].key, key("bob"));
}

#[test]
fn changes_since_no_dedup() {
    let mut map = make_replog();
    map.insert(key("alice"), val("v1"));
    map.insert(key("alice"), val("v2"));
    map.insert(key("alice"), val("v3"));

    let (entries, _) = map.changes_since(0, 100).unwrap();
    // All 3 entries returned — no dedup
    assert_eq!(entries.len(), 3);
    assert!(entries.iter().all(|e| e.key == key("alice")));
}

#[test]
fn changes_since_insert_then_delete() {
    let mut map = make_replog();
    map.insert(key("alice"), val("100"));
    map.remove(&key("alice"));

    let (entries, _) = map.changes_since(0, 100).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries[0].is_upsert());
    assert!(entries[1].is_delete());
}

#[test]
fn changes_since_partial() {
    let mut map = make_replog();
    map.insert(key("a"), val("1"));
    map.insert(key("b"), val("2"));
    map.insert(key("c"), val("3"));

    // From seq 1: should return b and c
    let (entries, next_seq) = map.changes_since(1, 100).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(next_seq, 3);
    assert_eq!(entries[0].key, key("b"));
    assert_eq!(entries[1].key, key("c"));

    // Already up to date
    let (entries, next_seq) = map.changes_since(3, 100).unwrap();
    assert_eq!(entries.len(), 0);
    assert_eq!(next_seq, 3);
}

#[test]
fn changes_since_with_limit() {
    let mut map = make_replog();
    for i in 0u8..20 {
        map.insert(vec![i], vec![i]);
    }

    let (entries, next_seq) = map.changes_since(0, 5).unwrap();
    assert_eq!(entries.len(), 5);
    assert_eq!(next_seq, 5);

    // Continue from where we left off
    let (entries, next_seq) = map.changes_since(5, 5).unwrap();
    assert_eq!(entries.len(), 5);
    assert_eq!(next_seq, 10);
}

// --- Replication: snapshot ---

#[test]
fn snapshot_page_full() {
    let mut map = make_replog();
    map.insert(key("c"), val("3"));
    map.insert(key("a"), val("1"));
    map.insert(key("b"), val("2"));

    let (entries, seq) = map.snapshot_page(None, 100);
    assert_eq!(seq, 3);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].1, key("a"));
    assert_eq!(entries[1].1, key("b"));
    assert_eq!(entries[2].1, key("c"));
}

#[test]
fn snapshot_page_paginated() {
    let mut map = make_replog();
    for i in 0u8..10 {
        map.insert(vec![i], vec![i * 10]);
    }

    let (page1, _) = map.snapshot_page(None, 3);
    assert_eq!(page1.len(), 3);
    assert_eq!(page1[0].1, vec![0u8]);
    assert_eq!(page1[2].1, vec![2u8]);

    let (page2, _) = map.snapshot_page(Some(&page1[2].1), 3);
    assert_eq!(page2.len(), 3);
    assert_eq!(page2[0].1, vec![3u8]);
    assert_eq!(page2[2].1, vec![5u8]);
}

// --- Compaction ---

#[test]
fn compact_stale_client_gets_none() {
    let mut map = make_replog();
    map.insert(key("a"), val("1"));
    map.insert(key("b"), val("2"));
    assert_eq!(map.current_seq(), 2);

    map.compact();
    assert_eq!(map.current_seq(), 2);

    // Stale client gets None
    assert_eq!(map.changes_since(0, 100), None);

    // Up-to-date client still works
    let (entries, _) = map.changes_since(2, 100).unwrap();
    assert_eq!(entries.len(), 0);
}

#[test]
fn compact_then_new_changes() {
    let mut map = make_replog();
    map.insert(key("a"), val("1"));
    map.compact();

    map.insert(key("b"), val("2"));

    let (entries, next_seq) = map.changes_since(1, 100).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(next_seq, 2);
    assert_eq!(entries[0].key, key("b"));
}

// --- Clear ---

#[test]
fn clear_resets_everything() {
    let mut map = make_replog();
    map.insert(key("a"), val("1"));
    map.insert(key("b"), val("2"));
    let seq_before_clear = map.current_seq();

    map.clear();
    assert!(map.is_empty());
    assert_eq!(map.get(&key("a")), None);

    // Any client synced before clear must resync
    assert_eq!(map.changes_since(0, 100), None);
    assert_eq!(map.changes_since(seq_before_clear, 100), None);

    // After resync, client gets empty map and can resume from current_seq
    let (snapshot, seq) = map.snapshot_page(None, 100);
    assert!(snapshot.is_empty());
    let (entries, _) = map.changes_since(seq, 100).unwrap();
    assert!(entries.is_empty());

    // Entry IDs continue from where they left off (never reused)
    map.insert(key("c"), val("3"));
    let (entries, _) = map.changes_since(seq, 100).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].entry_id >= 2);
}

#[test]
fn clear_preserves_entry_id_monotonicity() {
    let mut map = make_replog();
    map.insert(key("a"), val("1"));
    map.insert(key("b"), val("2"));
    map.insert(key("c"), val("3"));
    map.clear();
    let seq_after_clear = map.current_seq();

    map.insert(key("x"), val("99"));
    let (entries, _) = map.changes_since(seq_after_clear, 100).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].entry_id, 3);
}

#[test]
fn observability_accessors() {
    let mut map = make_replog();
    assert_eq!(map.changelog_len(), 0);
    assert_eq!(map.epoch_start(), 0);
    assert_eq!(map.next_entry_id(), 0);

    map.insert(key("a"), val("1"));
    map.insert(key("b"), val("2"));
    assert_eq!(map.changelog_len(), 2);
    assert_eq!(map.epoch_start(), 0);
    assert_eq!(map.next_entry_id(), 2);

    map.compact();
    assert_eq!(map.changelog_len(), 0);
    assert_eq!(map.epoch_start(), 2);
    assert_eq!(map.next_entry_id(), 2);
}

// --- Edge cases ---

#[test]
fn reinsert_after_delete() {
    let mut map = make_replog();
    map.insert(key("a"), val("1")); // entry_id 0
    map.remove(&key("a"));
    map.insert(key("a"), val("2")); // entry_id 1

    let (entries, _) = map.changes_since(0, 100).unwrap();
    assert_eq!(entries.len(), 3);
    assert!(entries[0].is_upsert());
    assert_eq!(entries[0].entry_id, 0);
    assert!(entries[1].is_delete());
    assert_eq!(entries[1].entry_id, 0);
    assert!(entries[2].is_upsert());
    assert_eq!(entries[2].entry_id, 1);
}

#[test]
fn many_entries() {
    let mut map = make_replog();
    let n = 500u64;

    for i in 0..n {
        map.insert(i.to_le_bytes().to_vec(), i.to_le_bytes().to_vec());
    }
    assert_eq!(map.len(), n);
    assert_eq!(map.current_seq(), n);

    // Snapshot returns everything
    let (entries, _) = map.snapshot_page(None, n + 1);
    assert_eq!(entries.len(), n as usize);

    // Changes since 0 returns everything
    let (entries, _) = map.changes_since(0, n + 1).unwrap();
    assert_eq!(entries.len(), n as usize);

    // Update half
    for i in 0..n / 2 {
        map.insert(i.to_le_bytes().to_vec(), (i * 100).to_le_bytes().to_vec());
    }

    // Changes since n returns only updates
    let (entries, _) = map.changes_since(n, n + 1).unwrap();
    assert_eq!(entries.len(), (n / 2) as usize);
}

// --- try_insert / try_remove ---

#[test]
fn try_insert_works() {
    let mut map = make_replog();
    assert_eq!(map.try_insert(key("a"), val("1")).unwrap(), None);
    assert_eq!(map.get(&key("a")), Some(val("1")));

    let old = map.try_insert(key("a"), val("2")).unwrap();
    assert_eq!(old, Some(val("1")));
    assert_eq!(map.get(&key("a")), Some(val("2")));
}

#[test]
fn try_remove_works() {
    let mut map = make_replog();
    assert_eq!(map.try_remove(&key("a")).unwrap(), None);

    map.insert(key("a"), val("1"));
    assert_eq!(map.try_remove(&key("a")).unwrap(), Some(val("1")));
    assert_eq!(map.get(&key("a")), None);
}

// --- changes_with_values_since ---

#[test]
fn changes_with_values() {
    let mut map = make_replog();
    map.insert(key("a"), val("1"));
    map.insert(key("b"), val("2"));
    map.remove(&key("a"));

    let (entries, next_seq) = map.changes_with_values_since(0, 100).unwrap();
    assert_eq!(next_seq, 3);
    assert_eq!(entries.len(), 3);

    assert!(entries[0].0.is_upsert());
    assert_eq!(entries[0].1, None);

    assert!(entries[1].0.is_upsert());
    assert_eq!(entries[1].1, Some(val("2")));

    assert!(entries[2].0.is_delete());
    assert_eq!(entries[2].1, None);
}
