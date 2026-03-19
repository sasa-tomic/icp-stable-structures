# Spec: `ic-stable-replog` — Incremental Sync for Stable BTreeMap

## Problem

Clients outside the canister (browser apps, local SQLite, indexers) need to replicate
the contents of a stable `BTreeMap`. Today, the only option is a full scan of all
entries on every sync. At 100M+ entries with 100–500 byte keys, full scans are
prohibitively expensive.

We need an efficient way for clients to ask: *"give me everything that changed since
I last synced"*.

## Motivation

The broader goal is to enable local-first databases — SQLite, RxDB, IndexedDB — to
work as replicas of IC canister state. The canister is the single writer and source
of truth; client databases are read replicas that sync incrementally.

This crate provides the canister-side foundation. Follow-up work will build client
SDKs and Candid query interfaces on top. Design decisions here should keep that in
mind:

- **`entry_id` as stable row identity.** SQLite and RxDB both work with row IDs /
  primary keys. The `u64 entry_id` maps directly to this concept. Clients store
  `entry_id` alongside each row in their local schema, enabling efficient lookups
  when applying incremental changes.

- **Changesets, not event streams.** Local databases want row-level changesets
  they can apply as a batch/transaction: "upsert these rows, delete these rows."
  The `changes_since` API returns raw changelog entries (keys + entry IDs);
  clients apply them in order. No server-side deduplication — the client sees
  every intermediate operation, which keeps the server simple and the protocol
  transparent.

- **Pagination via cursors.** Both incremental sync and full snapshot use simple
  cursor-based pagination (seq_num for changes, last_key for snapshots). This maps
  naturally to Candid query calls with bounded response sizes.

- **Schema-agnostic.** This crate doesn't interpret keys or values — it stores
  opaque `Storable` bytes. Schema mapping (e.g., deserializing a value into SQLite
  columns) is the client SDK's responsibility.

## Chosen Approach

Decompose the data into three purpose-built structures:

```
┌─────────────────────────────────────────────────────────────────┐
│                        RepLogMap<K, V>                            │
│                                                                 │
│  (1) Key Index:    BTreeMap<K, u64>       K → entry_id          │
│  (2) Value Store:  BTreeMap<u64, V>       entry_id → value      │
│  (3) Changelog:    Log<ChangeEntry<K>>    append-only changes   │
│                                                                 │
│  Counters (in a Cell or dedicated memory):                      │
│    next_entry_id: u64                                           │
│    epoch_start:   u64    (seq_num at last changelog clear)      │
└─────────────────────────────────────────────────────────────────┘
```

The Key Index provides key-ordered traversal and lookup. The Value Store provides
O(log n) access by stable entry ID. The Changelog is append-only and records variable-size entries
(9 bytes + key size) for both upserts and deletes.

## Data Flow

### Insert(key, value)

```
entry_id = next_entry_id++
Key Index:    insert (key, entry_id)
Value Store:  insert (entry_id, value)
Changelog:    append ChangeEntry::Upsert { entry_id, key }
```

### Update(key, value)

```
entry_id = Key Index.get(key)
Value Store:  update (entry_id, value)
Changelog:    append ChangeEntry::Upsert { entry_id, key }
```

Insert and update are unified in the public API as `insert(key, value) -> Option<V>`.
If the key already exists in the Key Index, it's an update; otherwise an insert.

### Remove(key)

```
entry_id = Key Index.remove(key)
Value Store:  remove (entry_id)
Changelog:    append ChangeEntry::Delete { entry_id, key }
```

### Get(key)

```
entry_id = Key Index.get(key)
value    = Value Store.get(entry_id)
return value
```

### Clear

```
Key Index:    clear
Value Store:  clear
Changelog:    clear
epoch_start   = current_seq + 1   (invalidates all client cursors)
next_entry_id is preserved         (monotonic, never reset)
```

## Sync Protocol

### Incremental Sync

```
Client stores: last_seq_num (initially 0)

Client calls:  changes_since(last_seq_num, limit) -> Option<(Vec<ChangeEntry<K>>, next_seq)>

Server:
  if last_seq_num < epoch_start:
    return None                    (client must do full resync via snapshot_page)
  else:
    read changelog entries from (last_seq_num - epoch_start) to min(current, offset + limit)
    return Some((entries, next_seq))

Client:
  for each ChangeEntry:
    Upsert { entry_id, key }:
      fetch value via get(key) or get_value_by_id(entry_id)
      upsert (entry_id, key, value) into local store
    Delete { entry_id, key }:
      delete entry_id from local store
  set last_seq_num = next_seq
```

### Full Resync (Snapshot)

Used when a client has never synced, or when its cursor is behind a trimmed
changelog (epoch_start > last_seq_num).

```
Client calls:  snapshot_page(after_key: Option<K>, limit: u64)
               -> (Vec<(entry_id, K, V)>, next_seq)

Server:
  range = match after_key {
    Some(k) => Key Index.range((Excluded(k), Unbounded)),
    None    => Key Index.range(..),
  }
  for each (key, entry_id) in range.take(limit):
    value = Value Store.get(entry_id)
    emit (entry_id, key, value)
  return (entries, current_seq_num)

Client:
  insert all entries into local store
  if returned < limit: resync complete, set last_seq_num = next_seq
  else: call snapshot_page again with last key as cursor
```

### Changelog Compaction

```
RepLogMap::compact():
  epoch_start = epoch_start + changelog.len()
  changelog.clear()
```

After compaction, any client with `last_seq_num < new epoch_start` receives
`None` from `changes_since` and must do a full snapshot sync.

Compaction should only be called when all active clients have synced past
the current changelog. Tracking client cursors is the application's
responsibility.

## Types

```rust
/// A change record stored in the changelog.
enum ChangeEntry<K: Storable> {
    Upsert { entry_id: u64, key: K },
    Delete { entry_id: u64, key: K },
}
```

## Public API

```rust
impl<K, V, M1, M2, M3, M4, M5> RepLogMap<K, V, M1, M2, M3, M4, M5>
where
    K: Storable + Ord + Clone,
    V: Storable,
    M1: Memory,  // Key Index
    M2: Memory,  // Value Store
    M3: Memory,  // Changelog index (Log needs two memories)
    M4: Memory,  // Changelog data
    M5: Memory,  // Counters cell
{
    /// Initialize or load from existing memories.
    fn init(
        key_index_mem: M1,
        value_store_mem: M2,
        changelog_index_mem: M3,
        changelog_data_mem: M4,
        counters_mem: M5,
    ) -> Self;

    // --- BTreeMap-compatible API ---

    fn insert(&mut self, key: K, value: V) -> Option<V>;
    fn get(&self, key: &K) -> Option<V>;
    fn contains_key(&self, key: &K) -> bool;
    fn remove(&mut self, key: &K) -> Option<V>;
    fn try_insert(&mut self, key: K, value: V) -> Result<Option<V>, RepLogMapError>;
    fn try_remove(&mut self, key: &K) -> Result<Option<V>, RepLogMapError>;
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool;
    fn clear(&mut self);
    fn first_key_value(&self) -> Option<(K, V)>;
    fn last_key_value(&self) -> Option<(K, V)>;
    fn iter(&self) -> impl Iterator<Item = (K, V)>;
    fn range(&self, range: impl RangeBounds<K>) -> impl Iterator<Item = (K, V)>;
    fn keys(&self) -> impl Iterator<Item = K>;
    fn values(&self) -> impl Iterator<Item = V>;
    fn get_value_by_id(&self, entry_id: u64) -> Option<V>;

    // --- Replication API ---

    /// Returns the current sequence number (head of the changelog).
    fn current_seq(&self) -> u64;

    /// Returns the number of entries in the changelog.
    fn changelog_len(&self) -> u64;

    /// Returns the current epoch start sequence number.
    fn epoch_start(&self) -> u64;

    /// Returns the next entry ID that will be assigned.
    fn next_entry_id(&self) -> u64;

    /// Returns up to `limit` changelog entries starting from `since_seq`.
    /// Returns `None` if `since_seq` is behind a compacted/cleared changelog
    /// (client must do a full resync via `snapshot_page`).
    /// Returns `Some((entries, next_seq))` on success.
    fn changes_since(&self, since_seq: u64, limit: u64)
        -> Option<(Vec<ChangeEntry<K>>, u64)>;

    /// Like `changes_since`, but also fetches the current value for each entry.
    /// Upsert entries include `Some(value)`, Delete entries include `None`.
    fn changes_with_values_since(&self, since_seq: u64, limit: u64)
        -> Option<(Vec<(ChangeEntry<K>, Option<V>)>, u64)>;

    /// Returns a page of the full snapshot for initial sync or resync.
    /// Entries are ordered by key. Pass the last key received as `after_key`
    /// to paginate.
    fn snapshot_page(
        &self,
        after_key: Option<&K>,
        limit: u64,
    ) -> (Vec<(u64, K, V)>, u64);

    /// Clears the changelog. All clients with cursors behind the new
    /// epoch_start will need a full resync.
    fn compact(&mut self);
}
```

## Memory Layout

Each RepLogMap instance requires **5 MemoryIds** from the MemoryManager:

| MemoryId | Structure | Purpose |
|---|---|---|
| N | `BTreeMap<K, u64>` | Key Index |
| N+1 | `BTreeMap<u64, V>` | Value Store |
| N+2 | `Log` index memory | Changelog index |
| N+3 | `Log` data memory | Changelog data |
| N+4 | `Cell<Counters>` | next_entry_id + epoch_start |

With 255 MemoryIds available, a canister can have up to 51 RepLogMap instances.

## Performance Characteristics

### Per-operation costs (n = total entries, tree depth d ≈ log₆(n))

| Operation | Reads | Writes | Notes |
|---|---|---|---|
| `get(key)` | 2d | 0 | Key Index lookup + Value Store lookup |
| `insert(key, value)` — new key | 2d | 2d + amortized O(1) | Key Index insert + Value Store insert + Log append |
| `insert(key, value)` — update | 2d | d + amortized O(1) | Key Index lookup + Value Store update + Log append |
| `remove(key)` | 2d | 2d + amortized O(1) | Key Index remove + Value Store remove + Log append |
| `changes_since(seq, limit)` | O(k) | 0 | k = number of changes returned; sequential log reads |
| `snapshot_page(key, limit)` | O(limit × d) | 0 | Paginated key-order scan |

Compared to a plain BTreeMap (d reads/writes per operation), this is roughly
**2× read overhead and 2–3× write overhead** per mutation. The payoff is
O(k × d) incremental sync instead of O(n × d) full scan.

### Memory overhead

- Key Index: same as a BTreeMap<K, u64> (keys + 8 bytes per entry)
- Value Store: same as a BTreeMap<u64, V> (8-byte keys + values)
- Changelog: 9 + key_size bytes per entry (both upsert and delete)
- Counters: 16 bytes (fixed)
- Total per entry: ~8 bytes overhead beyond what BTreeMap<K, V> would use
  (the u64 entry_id stored in both Key Index values and Value Store keys)

### At scale (100M entries, 500B keys)

- Key Index: ~100M × (500B key + 8B entry_id + tree overhead) ≈ 70–80 GB
- Value Store: ~100M × (8B key + value_size + tree overhead) ≈ depends on value size
- Changelog for 1M changes: ~1M × (9B + key_size) ≈ 509 MB with 500B keys
- Compare to BTreeMap<K,V>: ~100M × (500B key + value + tree overhead) ≈ 65–75 GB

The overhead vs a plain BTreeMap is approximately the Value Store's 8-byte
entry_id keys plus tree metadata: roughly 10–15% additional memory.

## Edge Cases

| Scenario | Behavior |
|---|---|
| Insert then immediately delete same key | Both ops appear in changelog. Client applies insert then delete. Net effect: no entry. |
| Update a key that was already deleted | Cannot happen — update path checks Key Index first. |
| Multiple updates to same key between syncs | Multiple changelog entries with same entry_id. All entries are returned; clients apply them in order. |
| `clear()` called | Key Index, Value Store, and Changelog cleared. `epoch_start` set to `current_seq + 1` to invalidate all client cursors. `next_entry_id` preserved (monotonic). All clients need full resync. |
| `compact()` with slow client | Slow client gets `None` from `changes_since`, does full snapshot sync. |
| Canister trap mid-operation | On IC, update calls are atomic — all stable memory changes roll back. No inconsistency. |
| Entry ID overflow | u64 counter; at 1B inserts/second, overflow takes 584 years. Not a practical concern. |
| Key exists in Key Index but not Value Store | Bug / corruption. Debug assert in `get()`. Should never happen under normal operation. |

## Crate Structure

```
replog/
├── Cargo.toml          # depends on ic-stable-structures = { path = ".." }
├── src/
│   ├── lib.rs          # RepLogMap struct and public API
│   ├── changelog.rs    # ChangeEntry type, Counters, Storable impls
│   └── tests.rs        # unit and integration tests
```

Workspace integration in root `Cargo.toml`:
```toml
[workspace]
members = ["benchmarks", "replog"]
```

## Known Limitations

1. ~~**Log memory reclamation.**~~ Resolved.
   [PR #414](https://github.com/dfinity/stable-structures/pull/414) added
   `Log::clear()` upstream. `compact()` now calls `self.changelog.clear()`
   directly, reclaiming physical memory. No workarounds needed.

2. **`changes_since` returns keys but not values.** By design — keeping values
   out of the changelog keeps it compact. Use `changes_with_values_since` to
   get entries paired with their current values, or call `get(key)` /
   `get_value_by_id(entry_id)` for each Upsert entry in a custom query method.

3. **Snapshot pagination is non-transactional.** Mutations between `snapshot_page`
   calls during a full resync can cause the client to see a mix of old and new
   states. Clients should use this pattern:

   ```
   first_seq = None
   loop:
     (entries, seq) = snapshot_page(after_key, limit)
     if first_seq is None: first_seq = seq
     apply entries locally
     if entries.len() < limit: break
     after_key = last entry's key

   // Catch mutations that happened during pagination
   catch_up(first_seq)
   ```

   The building blocks (`snapshot_page` + `changes_since`) support this; the
   ordering logic is the client's responsibility.

## Non-Goals

- **Client-side implementation**: this spec covers only the canister-side data
  structure. Client SDKs (JS, Rust, etc.) are out of scope.
- **Conflict resolution**: this is a single-writer (canister) to many-readers
  (clients) replication. No write conflicts to resolve.
- **Partial key sync**: clients sync entire entries, not partial field updates.
- **Real-time streaming**: sync is pull-based (client polls), not push-based.

## Resolved Questions

1. **Should `changes_since` deduplicate server-side or let clients deduplicate?**
   **Neither — no dedup.** `changes_since` returns raw changelog entries in
   order. Clients apply them sequentially, which naturally converges to the
   correct state. This keeps the server simple and avoids the heap cost of
   building a deduplicated map on every sync call.

2. **Should `ChangeEntry` variants include the key?**
   **Yes, both do.** Both `Upsert` and `Delete` carry the key. This lets
   clients apply changes without needing to maintain a separate
   `entry_id → key` mapping, simplifying client logic at the cost of
   slightly larger changelog entries.

3. **Should the Changelog use `Log` or `BTreeMap<u64, ChangeEntry>`?**
   **Start with `Log`.** Append + sequential read is the primary access pattern.
   `BTreeMap` allows finer-grained compaction but adds overhead. Can be swapped
   later without changing the public API.

4. **Counter storage: 5th MemoryId or packed into reserved header bytes?**
   **5th MemoryId with `Cell<Counters>`.** Clarity over cleverness.

## Open Questions

1. **Candid interface design.** The canister-facing query API (wrapping
   `changes_since` and `snapshot_page` as Candid query methods) is out of scope
   for this crate but will be needed by client SDKs. Should this crate provide
   suggested Candid type definitions or leave that entirely to consumers?

2. **Bounded response sizes.** IC query calls have response size limits (~2MB
   effective for certified queries, ~3.2MB for uncertified). The `limit`
   parameter on `changes_since` and `snapshot_page` lets callers control this,
   but should the crate enforce a maximum to prevent OOM on the canister side?
