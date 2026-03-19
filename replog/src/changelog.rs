use ic_stable_structures::Storable;
use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Upsert = 0,
    Delete = 1,
}

/// A change record stored in the append-only changelog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEntry<K> {
    pub kind: ChangeKind,
    pub entry_id: u64,
    pub key: K,
}

impl<K> ChangeEntry<K> {
    pub fn is_upsert(&self) -> bool {
        self.kind == ChangeKind::Upsert
    }

    pub fn is_delete(&self) -> bool {
        self.kind == ChangeKind::Delete
    }
}

impl<K: Storable + Clone> Storable for ChangeEntry<K> {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        let key_bytes = self.key.to_bytes();
        let mut buf = Vec::with_capacity(1 + 8 + key_bytes.len());
        buf.push(self.kind as u8);
        buf.extend_from_slice(&self.entry_id.to_le_bytes());
        buf.extend_from_slice(&key_bytes);
        Cow::Owned(buf)
    }

    fn into_bytes(self) -> Vec<u8> {
        self.to_bytes().into_owned()
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        let kind = match bytes[0] {
            0 => ChangeKind::Upsert,
            1 => ChangeKind::Delete,
            t => panic!("invalid ChangeEntry tag: {}", t),
        };
        Self {
            kind,
            entry_id: u64::from_le_bytes(bytes[1..9].try_into().unwrap()),
            key: K::from_bytes(Cow::Borrowed(&bytes[9..])),
        }
    }

    const BOUND: ic_stable_structures::storable::Bound =
        ic_stable_structures::storable::Bound::Unbounded;
}

/// Counters persisted in a Cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counters {
    pub next_entry_id: u64,
    pub epoch_start: u64,
}

impl Default for Counters {
    fn default() -> Self {
        Self {
            next_entry_id: 0,
            epoch_start: 0,
        }
    }
}

impl Storable for Counters {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        let mut buf = [0u8; 16];
        buf[0..8].copy_from_slice(&self.next_entry_id.to_le_bytes());
        buf[8..16].copy_from_slice(&self.epoch_start.to_le_bytes());
        Cow::Owned(buf.to_vec())
    }

    fn into_bytes(self) -> Vec<u8> {
        self.to_bytes().into_owned()
    }

    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        Self {
            next_entry_id: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            epoch_start: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
        }
    }

    const BOUND: ic_stable_structures::storable::Bound =
        ic_stable_structures::storable::Bound::Bounded {
            max_size: 16,
            is_fixed_size: true,
        };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_entry_roundtrip_upsert() {
        let entry: ChangeEntry<Vec<u8>> = ChangeEntry {
            kind: ChangeKind::Upsert,
            entry_id: 42,
            key: vec![1, 2, 3, 4, 5],
        };
        let bytes = entry.to_bytes();
        let decoded = ChangeEntry::from_bytes(bytes);
        assert_eq!(decoded, entry);
    }

    #[test]
    fn change_entry_roundtrip_delete() {
        let entry: ChangeEntry<Vec<u8>> = ChangeEntry {
            kind: ChangeKind::Delete,
            entry_id: 7,
            key: vec![10, 20],
        };
        let bytes = entry.to_bytes();
        let decoded = ChangeEntry::from_bytes(bytes);
        assert_eq!(decoded, entry);
    }

    #[test]
    fn counters_roundtrip() {
        let c = Counters {
            next_entry_id: 1_000_000,
            epoch_start: 500,
        };
        let bytes = c.to_bytes();
        assert_eq!(bytes.len(), 16);
        let decoded = Counters::from_bytes(bytes);
        assert_eq!(decoded, c);
    }
}
