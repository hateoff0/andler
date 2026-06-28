# andler-store

Persistent instance state storage using SQLite (`rusqlite` with `bundled` feature — statically linked `libsqlite3`, no system library dependency).

## Schema

Two tables with JSON columns:

```sql
CREATE TABLE IF NOT EXISTS instances (
    id          TEXT PRIMARY KEY,
    config_json TEXT NOT NULL,
    state_json  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS snapshots (
    id          TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    tag         TEXT NOT NULL,
    description TEXT,
    created_at  TEXT NOT NULL,
    FOREIGN KEY (instance_id) REFERENCES instances(id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_snapshots_instance_tag
    ON snapshots(instance_id, tag);
```

`InstanceConfig` and `InstanceState` are stored as JSON blobs, not as individual typed columns. The only access patterns today are "everything by `InstanceId`" and "everything for restore on startup" — there's no filtering by individual config fields.

`ON DELETE CASCADE` ensures snapshots are cleaned up when their parent instance is deleted.

## Public API

### `Store`

| Method | Signature | Description |
|--------|-----------|-------------|
| `open` | `async fn(path: impl AsRef<Path>) -> Result<Self, StoreError>` | Open or create sqlite DB at path, apply schema |
| `open_in_memory` | `async fn() -> Result<Self, StoreError>` | In-memory DB for tests only |
| `save_instance` | `async fn(&self, cfg: &InstanceConfig, state: &InstanceState) -> Result<(), StoreError>` | INSERT OR REPLACE — same semantics as `Daemon::create_instance` |
| `save_state` | `async fn(&self, id: InstanceId, state: &InstanceState) -> Result<(), StoreError>` | Update only `state_json`, preserve `config_json`. Returns `NotFound` if no row. |
| `load_instance` | `async fn(&self, id: InstanceId) -> Result<StoredInstance, StoreError>` | Load one instance (config + state) |
| `load_all` | `async fn(&self) -> Result<Vec<StoredInstance>, StoreError>` | Load all instances (for startup recovery) |
| `delete_instance` | `async fn(&self, id: InstanceId) -> Result<(), StoreError>` | Idempotent — no error if missing. Cascades to snapshots. |
| `save_snapshot` | `async fn(&self, snapshot: &StoredSnapshot) -> Result<(), StoreError>` | Save snapshot metadata |
| `load_snapshots` | `async fn(&self, instance_id: InstanceId) -> Result<Vec<StoredSnapshot>, StoreError>` | All snapshots for an instance, ordered by `created_at` |
| `get_snapshot` | `async fn(&self, instance_id: InstanceId, tag: &str) -> Result<Option<StoredSnapshot>, StoreError>` | One snapshot by tag |
| `delete_snapshot` | `async fn(&self, instance_id: InstanceId, tag: &str) -> Result<(), StoreError>` | Idempotent delete by tag |

### Types

**`StoredInstance`**: `config: InstanceConfig` + `state: InstanceState`.

**`StoredSnapshot`**: `id: Uuid` + `instance_id: InstanceId` + `tag: String` + `description: Option<String>` + `created_at: String`.

**`StoreError`**: `NotFound(InstanceId)`, `Sqlite(rusqlite::Error)`, `Serde(serde_json::Error)`, `TaskJoin(tokio::task::JoinError)`.

## Concurrency

Single `rusqlite::Connection` under `Arc<Mutex<...>>`. Each call goes into `tokio::task::spawn_blocking`. At current operation frequency (FSM transitions per individual instance, not hundreds per second), a dedicated writer thread or WAL mode doesn't justify the complexity.

## Important

This crate does NOT contain business logic for state transitions — only persistence of what's already decided in `andler-core`. Validation and FSM transitions stay in `andler-core`/`andler-daemon`. `Store` knows nothing about `HypervisorBackend` / backend handles — that's intentionally outside its responsibility (process handles don't survive daemon restart anyway, so there's no point persisting them).

## Tests

19 tests in `store::tests` using `Store::open_in_memory()`:

- Round-trip config + state (including `InstanceState::Error { message }`)
- Overwrite existing ID
- Partial state update (`save_state` preserves config)
- `NotFound` for missing records
- `load_all` on empty and non-empty stores
- `delete_instance` idempotency
- Snapshot CRUD: round-trip, unique tag per instance, cascade delete, get/delete, different InstanceIds for same tag
- `parse_instance_id` from string

No `qemu-img` / `/dev/kvm` / network required — only sqlite `:memory:`, so no `#[ignore]` flags. Runs in regular unit-test target.
