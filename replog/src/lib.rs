mod changelog;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod proptests;

pub use changelog::{ChangeEntry, ChangeKind, Counters};

use ic_stable_structures::{BTreeMap, Cell, Log, Memory, Storable};
use std::ops::RangeBounds;

#[derive(Debug)]
pub enum RepLogError {
    ChangelogFull,
}

impl std::fmt::Display for RepLogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RepLogError::ChangelogFull => write!(f, "failed to append to changelog: memory full"),
        }
    }
}

impl std::error::Error for RepLogError {}

/// A BTreeMap with built-in change tracking for incremental sync.
///
/// Internally decomposes into:
/// - Key Index: `BTreeMap<K, u64>` mapping keys to stable entry IDs
/// - Value Store: `BTreeMap<u64, V>` mapping entry IDs to values
/// - Changelog: `Log<ChangeEntry<K>>` append-only change log
/// - Counters: `Cell<Counters>` for next_entry_id and epoch_start
///
/// Requires 5 separate `Memory` instances (typically from `MemoryManager`).
pub struct RepLogMap<K, V, M1, M2, M3, M4, M5>
where
    K: Storable + Ord + Clone,
    V: Storable,
    M1: Memory,
    M2: Memory,
    M3: Memory,
    M4: Memory,
    M5: Memory,
{
    key_index: BTreeMap<K, u64, M1>,
    value_store: BTreeMap<u64, V, M2>,
    changelog: Log<ChangeEntry<K>, M3, M4>,
    counters: Cell<Counters, M5>,
}

impl<K, V, M1, M2, M3, M4, M5> RepLogMap<K, V, M1, M2, M3, M4, M5>
where
    K: Storable + Ord + Clone,
    V: Storable + Clone,
    M1: Memory,
    M2: Memory,
    M3: Memory,
    M4: Memory,
    M5: Memory,
{
    /// Initializes a RepLogMap, loading existing data from the provided memories
    /// or creating new empty structures.
    pub fn init(
        key_index_mem: M1,
        value_store_mem: M2,
        changelog_index_mem: M3,
        changelog_data_mem: M4,
        counters_mem: M5,
    ) -> Self {
        Self {
            key_index: BTreeMap::init(key_index_mem),
            value_store: BTreeMap::init(value_store_mem),
            changelog: Log::init(changelog_index_mem, changelog_data_mem),
            counters: Cell::init(counters_mem, Counters::default()),
        }
    }

    /// Inserts a key-value pair. Returns the previous value if the key existed.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        match self.key_index.get(&key) {
            Some(entry_id) => {
                let old = self.value_store.insert(entry_id, value);
                self.changelog
                    .append(&ChangeEntry { kind: ChangeKind::Upsert, entry_id, key })
                    .expect("failed to append to changelog");
                old
            }
            None => {
                let mut c = *self.counters.get();
                let entry_id = c.next_entry_id;
                c.next_entry_id += 1;
                self.counters.set(c);

                self.key_index.insert(key.clone(), entry_id);
                self.value_store.insert(entry_id, value);
                self.changelog
                    .append(&ChangeEntry { kind: ChangeKind::Upsert, entry_id, key })
                    .expect("failed to append to changelog");
                None
            }
        }
    }

    /// Returns the value associated with the given key.
    pub fn get(&self, key: &K) -> Option<V> {
        let entry_id = self.key_index.get(key)?;
        let value = self.value_store.get(&entry_id);
        debug_assert!(value.is_some(), "key_index and value_store are inconsistent for entry_id {}", entry_id);
        value
    }

    /// Returns true if the map contains the given key.
    pub fn contains_key(&self, key: &K) -> bool {
        self.key_index.contains_key(key)
    }

    /// Removes a key from the map, returning its value if present.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let entry_id = self.key_index.remove(key)?;
        let value = self.value_store.remove(&entry_id);
        self.changelog
            .append(&ChangeEntry { kind: ChangeKind::Delete, entry_id, key: key.clone() })
            .expect("failed to append to changelog");
        value
    }

    pub fn try_insert(&mut self, key: K, value: V) -> Result<Option<V>, RepLogError> {
        match self.key_index.get(&key) {
            Some(entry_id) => {
                let old = self.value_store.insert(entry_id, value);
                match self.changelog.append(&ChangeEntry { kind: ChangeKind::Upsert, entry_id, key: key.clone() }) {
                    Ok(_) => Ok(old),
                    Err(_) => {
                        match old {
                            Some(old_val) => { self.value_store.insert(entry_id, old_val); }
                            None => { self.value_store.remove(&entry_id); }
                        }
                        Err(RepLogError::ChangelogFull)
                    }
                }
            }
            None => {
                let mut c = *self.counters.get();
                let entry_id = c.next_entry_id;
                c.next_entry_id += 1;
                self.counters.set(c);

                self.key_index.insert(key.clone(), entry_id);
                self.value_store.insert(entry_id, value);
                match self.changelog.append(&ChangeEntry { kind: ChangeKind::Upsert, entry_id, key: key.clone() }) {
                    Ok(_) => Ok(None),
                    Err(_) => {
                        self.key_index.remove(&key);
                        self.value_store.remove(&entry_id);
                        c.next_entry_id -= 1;
                        self.counters.set(c);
                        Err(RepLogError::ChangelogFull)
                    }
                }
            }
        }
    }

    pub fn try_remove(&mut self, key: &K) -> Result<Option<V>, RepLogError> {
        let entry_id = match self.key_index.remove(key) {
            Some(id) => id,
            None => return Ok(None),
        };
        let value = self.value_store.remove(&entry_id);
        match self.changelog.append(&ChangeEntry { kind: ChangeKind::Delete, entry_id, key: key.clone() }) {
            Ok(_) => Ok(value),
            Err(_) => {
                self.key_index.insert(key.clone(), entry_id);
                if let Some(v) = value {
                    self.value_store.insert(entry_id, v);
                }
                Err(RepLogError::ChangelogFull)
            }
        }
    }

    /// Returns the number of entries in the map.
    pub fn len(&self) -> u64 {
        self.key_index.len()
    }

    /// Returns true if the map is empty.
    pub fn is_empty(&self) -> bool {
        self.key_index.is_empty()
    }

    /// Removes all entries and resets all state.
    /// All clients will get `None` from `changes_since` and must resync
    /// (to an empty map).
    pub fn clear(&mut self) {
        let invalidation_seq = self.current_seq() + 1;
        self.key_index.clear_new();
        self.value_store.clear_new();
        self.changelog.clear();
        self.counters.set(Counters {
            next_entry_id: self.counters.get().next_entry_id,
            epoch_start: invalidation_seq,
        });
    }

    /// Returns the first key-value pair (minimum key).
    pub fn first_key_value(&self) -> Option<(K, V)> {
        let (key, entry_id) = self.key_index.first_key_value()?;
        let value = self.value_store.get(&entry_id);
        debug_assert!(value.is_some(), "key_index and value_store are inconsistent for entry_id {}", entry_id);
        Some((key, value?))
    }

    /// Returns the last key-value pair (maximum key).
    pub fn last_key_value(&self) -> Option<(K, V)> {
        let (key, entry_id) = self.key_index.last_key_value()?;
        let value = self.value_store.get(&entry_id);
        debug_assert!(value.is_some(), "key_index and value_store are inconsistent for entry_id {}", entry_id);
        Some((key, value?))
    }

    /// Returns an iterator over all (key, value) pairs in key order.
    pub fn iter(&self) -> impl Iterator<Item = (K, V)> + '_ {
        self.key_index.iter().filter_map(|entry| {
            let key = entry.key().clone();
            let entry_id = entry.value();
            let value = self.value_store.get(&entry_id)?;
            Some((key, value))
        })
    }

    /// Returns an iterator over (key, value) pairs in the given key range.
    pub fn range(&self, range: impl RangeBounds<K>) -> impl Iterator<Item = (K, V)> + '_ {
        self.key_index.range(range).filter_map(|entry| {
            let key = entry.key().clone();
            let entry_id = entry.value();
            let value = self.value_store.get(&entry_id)?;
            Some((key, value))
        })
    }

    /// Returns an iterator over all keys in order.
    pub fn keys(&self) -> impl Iterator<Item = K> + '_ {
        self.key_index.keys()
    }

    /// Returns an iterator over all values in key order.
    pub fn values(&self) -> impl Iterator<Item = V> + '_ {
        self.key_index.iter().filter_map(|entry| {
            let entry_id = entry.value();
            self.value_store.get(&entry_id)
        })
    }

    /// Returns the value for a given entry ID, bypassing key lookup.
    pub fn get_value_by_id(&self, entry_id: u64) -> Option<V> {
        self.value_store.get(&entry_id)
    }

    /// Returns the number of entries in the changelog.
    pub fn changelog_len(&self) -> u64 {
        self.changelog.len()
    }

    /// Returns the current epoch start sequence number.
    pub fn epoch_start(&self) -> u64 {
        self.counters.get().epoch_start
    }

    /// Returns the next entry ID that will be assigned.
    pub fn next_entry_id(&self) -> u64 {
        self.counters.get().next_entry_id
    }

    // --- Replication API ---

    /// Returns the current sequence number.
    pub fn current_seq(&self) -> u64 {
        self.counters.get().epoch_start + self.changelog.len()
    }

    /// Returns up to `limit` changelog entries starting from `since_seq`.
    ///
    /// Returns `None` if `since_seq` is behind a compacted changelog
    /// (client must do a full resync via `snapshot_page`).
    ///
    /// Returns `Some((entries, next_seq))` where `next_seq` is the
    /// sequence number to pass on the next call.
    pub fn changes_since(
        &self,
        since_seq: u64,
        limit: u64,
    ) -> Option<(Vec<ChangeEntry<K>>, u64)> {
        let c = self.counters.get();

        if since_seq < c.epoch_start {
            return None;
        }

        let offset = since_seq - c.epoch_start;
        let log_len = self.changelog.len();

        if offset >= log_len {
            return Some((vec![], self.current_seq()));
        }

        let end = log_len.min(offset + limit);

        let mut entries = Vec::with_capacity((end - offset) as usize);
        for idx in offset..end {
            if let Some(entry) = self.changelog.get(idx) {
                entries.push(entry);
            }
        }

        let next_seq = since_seq + entries.len() as u64;
        Some((entries, next_seq))
    }

    pub fn changes_with_values_since(
        &self,
        since_seq: u64,
        limit: u64,
    ) -> Option<(Vec<(ChangeEntry<K>, Option<V>)>, u64)> {
        let (entries, next_seq) = self.changes_since(since_seq, limit)?;
        let with_values = entries
            .into_iter()
            .map(|entry| {
                let value = if entry.is_upsert() { self.get(&entry.key) } else { None };
                (entry, value)
            })
            .collect();
        Some((with_values, next_seq))
    }

    /// Returns a page of the full map contents for initial/re-sync.
    ///
    /// Entries are returned in key order. Pass the last key received as
    /// `after_key` to paginate. Returns `(entries, current_seq)`.
    pub fn snapshot_page(
        &self,
        after_key: Option<&K>,
        limit: u64,
    ) -> (Vec<(u64, K, V)>, u64) {
        let iter: Box<dyn Iterator<Item = _>> = match after_key {
            Some(k) => Box::new(
                self.key_index
                    .range((std::ops::Bound::Excluded(k.clone()), std::ops::Bound::Unbounded)),
            ),
            None => Box::new(self.key_index.iter()),
        };

        let mut entries = Vec::with_capacity(limit as usize);
        for lazy in iter.take(limit as usize) {
            let key = lazy.key().clone();
            let entry_id = lazy.value();
            if let Some(value) = self.value_store.get(&entry_id) {
                entries.push((entry_id, key, value));
            }
        }

        (entries, self.current_seq())
    }

    /// Clears the changelog and reclaims its memory. Clients with cursors
    /// behind the new epoch_start will get `None` from `changes_since`
    /// and must resync.
    pub fn compact(&mut self) {
        let new_epoch = self.current_seq();
        let c = self.counters.get();
        self.changelog.clear();
        self.counters.set(Counters {
            next_entry_id: c.next_entry_id,
            epoch_start: new_epoch,
        });
    }
}
