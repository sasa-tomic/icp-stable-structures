# RepLog Integration Guide

## Section A: Migrating an Existing BTreeMap to RepLogMap

Concrete example: migrating the `ACCOUNTS` map (`Principal → UserAccount`) in `cashier_backend`.

### 1. Add the dependency

In the workspace `Cargo.toml`:

```toml
ic-stable-replog = "0.1.0"
```

In the package `Cargo.toml` (`cashier_backend/Cargo.toml`):

```toml
ic-stable-replog = { workspace = true }
```

### 2. Allocate MemoryIds

RepLogMap needs **5** memory IDs. Add them after the last existing ID in `lib.rs`:

```rust
// Existing (last is 10)
pub const DELEGATIONS_MEMORY_ID: MemoryId = MemoryId::new(10);

// Replog: ACCOUNTS (5 memories)
pub const ACCOUNTS_REPLOG_KEY_INDEX_ID: MemoryId = MemoryId::new(11);
pub const ACCOUNTS_REPLOG_VALUE_STORE_ID: MemoryId = MemoryId::new(12);
pub const ACCOUNTS_REPLOG_CHANGELOG_INDEX_ID: MemoryId = MemoryId::new(13);
pub const ACCOUNTS_REPLOG_CHANGELOG_DATA_ID: MemoryId = MemoryId::new(14);
pub const ACCOUNTS_REPLOG_COUNTERS_ID: MemoryId = MemoryId::new(15);
```

Keep `ACCOUNTS_MEMORY_ID = 1` — it stays until migration completes.

### 3. Set up RepLogMap in thread_local

Replace the ACCOUNTS thread_local in `user_accounts.rs`:

```rust
use std::cell::RefCell;

use candid::Principal;
use ic_stable_structures::StableBTreeMap;
use ic_stable_replog::RepLogMap;

use crate::{Memory, MEMORY_MANAGER};
use crate::{
    ACCOUNTS_MEMORY_ID,
    ACCOUNTS_REPLOG_KEY_INDEX_ID,
    ACCOUNTS_REPLOG_VALUE_STORE_ID,
    ACCOUNTS_REPLOG_CHANGELOG_INDEX_ID,
    ACCOUNTS_REPLOG_CHANGELOG_DATA_ID,
    ACCOUNTS_REPLOG_COUNTERS_ID,
};

type AccountsRepLog = RepLogMap<Principal, UserAccount, Memory, Memory, Memory, Memory, Memory>;

thread_local! {
    // Old map — kept for migration reads, then unused
    static ACCOUNTS_LEGACY: RefCell<StableBTreeMap<Principal, UserAccount, Memory>> =
        RefCell::new(StableBTreeMap::init(
            MEMORY_MANAGER.with(|m| m.borrow().get(ACCOUNTS_MEMORY_ID))
        ));

    // New RepLogMap
    static ACCOUNTS: RefCell<AccountsRepLog> = RefCell::new(
        RepLogMap::init(
            MEMORY_MANAGER.with(|m| m.borrow().get(ACCOUNTS_REPLOG_KEY_INDEX_ID)),
            MEMORY_MANAGER.with(|m| m.borrow().get(ACCOUNTS_REPLOG_VALUE_STORE_ID)),
            MEMORY_MANAGER.with(|m| m.borrow().get(ACCOUNTS_REPLOG_CHANGELOG_INDEX_ID)),
            MEMORY_MANAGER.with(|m| m.borrow().get(ACCOUNTS_REPLOG_CHANGELOG_DATA_ID)),
            MEMORY_MANAGER.with(|m| m.borrow().get(ACCOUNTS_REPLOG_COUNTERS_ID)),
        )
    );
}
```

Update `with_accounts` and `with_accounts_mut` to use the RepLogMap:

```rust
pub fn with_accounts<R>(f: impl FnOnce(&AccountsRepLog) -> R) -> R {
    ACCOUNTS.with(|a| f(&a.borrow()))
}

pub(crate) fn with_accounts_mut<R>(f: impl FnOnce(&mut AccountsRepLog) -> R) -> R {
    ACCOUNTS.with(|a| f(&mut a.borrow_mut()))
}
```

Callers using `.get(&key)`, `.insert(k, v)`, `.remove(&k)`, `.iter()`, `.len()` work unchanged — RepLogMap has the same signatures.

### 4. Data migration

Add to `lib.rs` `run_migrations()`:

```rust
fn run_migrations() {
    audit_log::run_migrations();
    migrate_accounts_to_replog();
}
```

The migration function (in `user_accounts.rs` or a dedicated `migrations.rs`):

```rust
use crate::meta_kv;

const ACCOUNTS_REPLOG_MIGRATION_KEY: &str = "migration:accounts_replog";

pub fn migrate_accounts_to_replog() {
    if meta_kv::get(ACCOUNTS_REPLOG_MIGRATION_KEY).is_some() {
        return; // already migrated
    }

    let mut count = 0u64;

    // Read from legacy, write to RepLogMap
    ACCOUNTS_LEGACY.with(|legacy| {
        let legacy = legacy.borrow();
        ACCOUNTS.with(|replog| {
            let mut replog = replog.borrow_mut();
            for (principal, account) in legacy.iter() {
                replog.insert(principal, account);
                count += 1;
            }
        });
    });

    meta_kv::set(ACCOUNTS_REPLOG_MIGRATION_KEY, "done");
    ic_cdk::println!("Migration: copied {} accounts to replog", count);
}
```

If the account count is large (>10K), batch the migration across multiple IC messages using timers, similar to the existing `schedule_flush_batch` pattern.

### 5. Candid query endpoints for sync

```rust
use candid::{CandidType, Deserialize, Principal};
use ic_stable_replog::ChangeEntry;

#[derive(CandidType, Deserialize)]
struct RepLogChangesResponse {
    entries: Vec<AccountChange>,
    next_seq: u64,
}

#[derive(CandidType, Deserialize)]
enum AccountChange {
    Upsert { entry_id: u64, key: Principal },
    Delete { entry_id: u64, key: Principal },
}

impl From<ChangeEntry<Principal>> for AccountChange {
    fn from(e: ChangeEntry<Principal>) -> Self {
        match e {
            ChangeEntry::Upsert { entry_id, key } => AccountChange::Upsert { entry_id, key },
            ChangeEntry::Delete { entry_id, key } => AccountChange::Delete { entry_id, key },
        }
    }
}

#[derive(CandidType, Deserialize)]
struct RepLogSnapshotResponse {
    entries: Vec<(u64, Principal, UserAccount)>,
    current_seq: u64,
}

#[derive(CandidType, Deserialize)]
struct RepLogValueResponse {
    values: Vec<(u64, UserAccount)>,
}

/// Get current sequence number.
#[ic_cdk::query]
fn accounts_replog_seq() -> u64 {
    with_accounts(|m| m.current_seq())
}

/// Get changelog entries since `since_seq`. Returns None if resync required.
#[ic_cdk::query]
fn accounts_replog_changes(since_seq: u64, limit: u64) -> Option<RepLogChangesResponse> {
    with_accounts(|m| {
        m.changes_since(since_seq, limit).map(|(entries, next_seq)| {
            RepLogChangesResponse {
                entries: entries.into_iter().map(Into::into).collect(),
                next_seq,
            }
        })
    })
}

/// Get a page of all entries for full sync. Pass last key to paginate.
#[ic_cdk::query]
fn accounts_replog_snapshot(after_key: Option<Principal>, limit: u64) -> RepLogSnapshotResponse {
    with_accounts(|m| {
        let (entries, current_seq) = m.snapshot_page(after_key.as_ref(), limit);
        RepLogSnapshotResponse {
            entries,
            current_seq,
        }
    })
}

/// Fetch values by entry_id (for resolving Upsert changes).
#[ic_cdk::query]
fn accounts_replog_get_values(entry_ids: Vec<u64>) -> RepLogValueResponse {
    with_accounts(|m| {
        let values = entry_ids
            .into_iter()
            .filter_map(|id| m.get_value_by_id(id).map(|v| (id, v)))
            .collect();
        RepLogValueResponse { values }
    })
}
```

**Note:** `get_value_by_id` reads from the internal value store by entry ID. If this method is not yet exposed on `RepLogMap`, add it:

```rust
pub fn get_value_by_id(&self, entry_id: u64) -> Option<V> {
    self.value_store.get(&entry_id)
}
```

### 6. Client sync protocol

```
// Pseudocode — runs on the client (browser, agent, CLI)

state = {
    seq: 0,               // last known sequence number
    entries: Map(),        // entry_id -> { key, value }
    needs_full_sync: true, // start with full sync
}

function sync():
    if state.needs_full_sync:
        do_full_sync()
    else:
        do_incremental_sync()

function do_full_sync():
    state.entries.clear()
    last_key = null

    loop:
        resp = canister.accounts_replog_snapshot(last_key, 500)

        for (entry_id, key, value) in resp.entries:
            state.entries[entry_id] = { key, value }

        if resp.entries.is_empty():
            break

        last_key = resp.entries.last().key

    state.seq = resp.current_seq
    state.needs_full_sync = false

function do_incremental_sync():
    loop:
        resp = canister.accounts_replog_changes(state.seq, 500)

        if resp is None:
            // Changelog compacted — must resync
            state.needs_full_sync = true
            do_full_sync()
            return

        // Collect entry_ids that need value fetches
        upsert_ids = []
        for change in resp.entries:
            match change:
                Upsert { entry_id, key }:
                    upsert_ids.push(entry_id)
                    state.entries[entry_id] = { key, value: null }
                Delete { entry_id, key }:
                    state.entries.remove(entry_id)

        // Batch-fetch values for upserts
        if upsert_ids.not_empty():
            values_resp = canister.accounts_replog_get_values(upsert_ids)
            for (entry_id, value) in values_resp.values:
                state.entries[entry_id].value = value

        state.seq = resp.next_seq

        if resp.entries.is_empty():
            break  // caught up

// Poll loop
every 5 seconds:
    sync()
```

---

## Section B: Using RepLogMap in a New Project (Greenfield)

### 1. Cargo.toml

```toml
[package]
name = "my_backend"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
candid = "0.10"
ic-cdk = "0.19"
ic-stable-structures = "0.7.2"
ic-stable-replog = "0.1.0"
```

### 2. MemoryManager setup

```rust
use std::cell::RefCell;
use candid::{CandidType, Deserialize, Principal};
use ic_stable_structures::memory_manager::{MemoryId, MemoryManager, VirtualMemory};
use ic_stable_structures::{DefaultMemoryImpl, Storable, storable::Bound};
use ic_stable_replog::RepLogMap;
use std::borrow::Cow;

type Memory = VirtualMemory<DefaultMemoryImpl>;

// 5 memories per RepLogMap
const ITEMS_KEY_INDEX: MemoryId = MemoryId::new(0);
const ITEMS_VALUE_STORE: MemoryId = MemoryId::new(1);
const ITEMS_CHANGELOG_INDEX: MemoryId = MemoryId::new(2);
const ITEMS_CHANGELOG_DATA: MemoryId = MemoryId::new(3);
const ITEMS_COUNTERS: MemoryId = MemoryId::new(4);

#[derive(CandidType, Deserialize, Clone)]
struct Item {
    name: String,
    data: Vec<u8>,
}

impl Storable for Item {
    fn to_bytes(&self) -> Cow<'_, [u8]> {
        Cow::Owned(candid::encode_one(self).unwrap())
    }
    fn from_bytes(bytes: Cow<[u8]>) -> Self {
        candid::decode_one(&bytes).unwrap()
    }
    const BOUND: Bound = Bound::Unbounded;
}

type ItemMap = RepLogMap<Principal, Item, Memory, Memory, Memory, Memory, Memory>;

thread_local! {
    static MEMORY_MANAGER: RefCell<MemoryManager<DefaultMemoryImpl>> =
        RefCell::new(MemoryManager::init(DefaultMemoryImpl::default()));

    static ITEMS: RefCell<ItemMap> = RefCell::new(
        RepLogMap::init(
            MEMORY_MANAGER.with(|m| m.borrow().get(ITEMS_KEY_INDEX)),
            MEMORY_MANAGER.with(|m| m.borrow().get(ITEMS_VALUE_STORE)),
            MEMORY_MANAGER.with(|m| m.borrow().get(ITEMS_CHANGELOG_INDEX)),
            MEMORY_MANAGER.with(|m| m.borrow().get(ITEMS_CHANGELOG_DATA)),
            MEMORY_MANAGER.with(|m| m.borrow().get(ITEMS_COUNTERS)),
        )
    );
}
```

### 3. Basic usage

```rust
#[ic_cdk::update]
fn set_item(key: Principal, item: Item) {
    ITEMS.with(|m| m.borrow_mut().insert(key, item));
}

#[ic_cdk::query]
fn get_item(key: Principal) -> Option<Item> {
    ITEMS.with(|m| m.borrow().get(&key))
}

#[ic_cdk::update]
fn remove_item(key: Principal) -> Option<Item> {
    ITEMS.with(|m| m.borrow_mut().remove(&key))
}
```

### 4. Sync endpoints

```rust
use ic_stable_replog::ChangeEntry;

#[derive(CandidType, Deserialize)]
struct RepLogChangesResponse {
    entries: Vec<ItemChange>,
    next_seq: u64,
}

#[derive(CandidType, Deserialize)]
enum ItemChange {
    Upsert { entry_id: u64, key: Principal },
    Delete { entry_id: u64, key: Principal },
}

impl From<ChangeEntry<Principal>> for ItemChange {
    fn from(e: ChangeEntry<Principal>) -> Self {
        match e {
            ChangeEntry::Upsert { entry_id, key } => ItemChange::Upsert { entry_id, key },
            ChangeEntry::Delete { entry_id, key } => ItemChange::Delete { entry_id, key },
        }
    }
}

#[derive(CandidType, Deserialize)]
struct RepLogSnapshotResponse {
    entries: Vec<(u64, Principal, Item)>,
    current_seq: u64,
}

#[ic_cdk::query]
fn replog_seq() -> u64 {
    ITEMS.with(|m| m.borrow().current_seq())
}

#[ic_cdk::query]
fn replog_changes(since_seq: u64, limit: u64) -> Option<RepLogChangesResponse> {
    ITEMS.with(|m| {
        m.borrow().changes_since(since_seq, limit).map(|(entries, next_seq)| {
            RepLogChangesResponse {
                entries: entries.into_iter().map(Into::into).collect(),
                next_seq,
            }
        })
    })
}

#[ic_cdk::query]
fn replog_snapshot(after_key: Option<Principal>, limit: u64) -> RepLogSnapshotResponse {
    ITEMS.with(|m| {
        let (entries, current_seq) = m.borrow().snapshot_page(after_key.as_ref(), limit);
        RepLogSnapshotResponse { entries, current_seq }
    })
}
```

### 5. Client sync protocol

Same protocol as Section A:

```
state = { seq: 0, entries: Map(), needs_full_sync: true }

function sync():
    if state.needs_full_sync:
        state.entries.clear()
        last_key = null
        loop:
            resp = canister.sync_snapshot(last_key, 500)
            for (entry_id, key, value) in resp.entries:
                state.entries[entry_id] = { key, value }
            if resp.entries.is_empty(): break
            last_key = resp.entries.last().key
        state.seq = resp.current_seq
        state.needs_full_sync = false
    else:
        loop:
            resp = canister.replog_changes(state.seq, 500)
            if resp is None:
                state.needs_full_sync = true
                return sync()  // resync
            for change in resp.entries:
                match change:
                    Upsert { entry_id, key }:
                        value = canister.get_item(key)
                        state.entries[entry_id] = { key, value }
                    Delete { entry_id, key }:
                        state.entries.remove(entry_id)
            state.seq = resp.next_seq
            if resp.entries.is_empty(): break

every 5 seconds: sync()
```

In the greenfield case, `snapshot_page` already returns full `(entry_id, key, value)` tuples, so the full-sync path needs no extra fetches. For incremental sync, you either add a `get_value_by_id` method to RepLogMap (see Section A note) or fetch by key via your existing query endpoint.
