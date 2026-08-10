# andler-store

Snapshot metadata storage using SQLite (`rusqlite` with `bundled` feature — statically linked `libsqlite3`, no system library dependency).

Instance configs are **not** stored here: they live in per-instance `instance.toml` files (the daemon's registry), and instance lifecycle state is not persisted at all — a daemon restart recreates it via the toml scan and backend adoption. This store holds only `snapshots` metadata.

## Schema

```sql
CREATE TABLE IF NOT EXISTS snapshots (
    id          TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    tag         TEXT NOT NULL,
    description TEXT,
    created_at  TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_snapshots_instance_tag
    ON snapshots(instance_id, tag);
```

`created_at` is stored as an RFC-3339 string. There is no `instances` table and no FK enforcement (some distro sqlite builds default `foreign_keys` to ON; the store pins it off and never relies on cascades).

## Legacy migration

Databases from before the file-based registry (user_version 0/1, carrying an `instances(id, config_json, state_json)` table) are migrated in-place on daemon startup:

1. `load_legacy_instances()` reads every stored config JSON.
2. The daemon writes each config as `instance.toml` in the instance's directory — refusing to overwrite a file that already exists with different content (that would silently pick one of two sources of truth).
3. `finalize_config_migration()` drops the `instances` table and bumps `user_version` to 2; from then on `load_legacy_instances()` returns nothing and the migration is a no-op.

## Public API

### `Store`

| Method | Signature | Description |
|--------|-----------|-------------|
| `open` | `async fn(path: impl AsRef<Path>) -> Result<Self, StoreError>` | Open or create sqlite DB at path, apply schema |
| `open_in_memory` | `async fn() -> Result<Self, StoreError>` | In-memory DB for tests only |
| `schema_version` | `async fn(&self) -> Result<i64, StoreError>` | `PRAGMA user_version` |
| `load_legacy_instances` | `async fn(&self) -> Result<Vec<InstanceConfig>, StoreError>` | Configs from the pre-file-registry `instances` table; empty when already migrated |
| `finalize_config_migration` | `async fn(&self) -> Result<(), StoreError>` | Drop `instances`, set user_version 2 |
| `save_snapshot` | `async fn(&self, snapshot: &StoredSnapshot) -> Result<(), StoreError>` | Save snapshot metadata |
| `load_snapshots` | `async fn(&self, instance_id: InstanceId) -> Result<Vec<StoredSnapshot>, StoreError>` | All snapshots for an instance, ordered by `created_at` |
| `get_snapshot` | `async fn(&self, instance_id: InstanceId, tag: &str) -> Result<Option<StoredSnapshot>, StoreError>` | One snapshot by tag |
| `delete_snapshot` | `async fn(&self, instance_id: InstanceId, tag: &str) -> Result<(), StoreError>` | Idempotent delete by tag |

### Types

**`StoredSnapshot`**: `id: Uuid` + `instance_id: InstanceId` + `tag: String` + `description: Option<String>` + `created_at: String`.

**`StoreError`**: `NotFound(InstanceId)`, `Sqlite(rusqlite::Error)`, `Serde(serde_json::Error)`, `TaskJoin(tokio::task::JoinError)`.

## Concurrency

Single `rusqlite::Connection` under `Arc<Mutex<...>>`. Each call goes into `tokio::task::spawn_blocking`. At current operation frequency (snapshot metadata writes, not hundreds per second), a dedicated writer thread or WAL mode doesn't justify the complexity.

**Poisoned mutex recovery**: All `.lock()` calls use `.unwrap_or_else(|e| e.into_inner())` to recover the guard even if the mutex is poisoned (previous holder panicked). Safe because the only thing behind the Mutex is a `rusqlite::Connection` — SQLite's own transaction/statement state is safe to continue using after a panic in user code.

## Important

This crate does NOT contain business logic for state transitions — only persistence of what's already decided in `andler-core`. Validation and FSM transitions stay in `andler-core`/`andler-daemon`. `Store` knows nothing about `HypervisorBackend` / backend handles — that's intentionally outside its responsibility (process handles don't survive daemon restart anyway, so there's no point persisting them).

## Tests

~20 tests in `store::tests` using `Store::open_in_memory()`:

- Fresh store starts at the current schema
- Legacy-database migration: configs extracted, `instances` dropped, idempotent re-open
- Corrupt legacy config fails migration loudly
- Snapshot CRUD: round-trip, unique tag per instance, get/delete, different InstanceIds for same tag

No `qemu-img` / `/dev/kvm` / network required — only sqlite `:memory:`, so no `#[ignore]` flags. Runs in regular unit-test target.
