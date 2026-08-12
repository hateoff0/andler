use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use andler_core::InstanceId;
use andler_store::{Store, StoredSnapshot};

use super::error::DaemonError;

pub const SNAPSHOTS_DIR_NAME: &str = "disk.snapshots";

pub async fn instance_dir_of(id: InstanceId) -> PathBuf {
    andler_core::paths::instances_root().join(id.to_string())
}

pub fn snapshots_dir(instance_dir: &Path) -> PathBuf {
    instance_dir.join(SNAPSHOTS_DIR_NAME)
}

/// External layer files of an instance: `disk.snapshots/*.qcow2`, excluding
/// the `.tmp-*` files a live snapshot uses before the rename pair lands.
pub async fn list_layer_files(instance_dir: &Path) -> Result<Vec<PathBuf>, DaemonError> {
    let dir = snapshots_dir(instance_dir);
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(DaemonError::Io { path: dir, source }),
    };

    let mut layers = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| DaemonError::Io {
            path: dir.clone(),
            source,
        })?
    {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".qcow2") && !name.starts_with(".tmp-") {
            layers.push(entry.path());
        }
    }
    Ok(layers)
}

/// The first `.tmp-*` overlay in the snapshots dir, if any.
pub async fn pending_overlay(instance_dir: &Path) -> Result<Option<PathBuf>, DaemonError> {
    let dir = snapshots_dir(instance_dir);
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(DaemonError::Io { path: dir, source }),
    };

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| DaemonError::Io {
            path: dir.clone(),
            source,
        })?
    {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(".tmp-") {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

/// Chain of stored records from `start_id` up to the base (each record's
/// `parent_id`), including `start_id` itself.
pub fn ancestors(records: &[StoredSnapshot], start_id: uuid::Uuid) -> Vec<&StoredSnapshot> {
    let by_id: HashMap<uuid::Uuid, &StoredSnapshot> = records.iter().map(|r| (r.id, r)).collect();
    let mut chain = Vec::new();
    let mut current = Some(start_id);
    while let Some(id) = current {
        let Some(record) = by_id.get(&id) else {
            break;
        };
        chain.push(*record);
        current = record.parent_id;
    }
    chain
}

/// Direct children (records whose `parent_id` is `parent`), regardless of
/// branch — a layer's children always share its branch by construction.
pub fn direct_children(records: &[StoredSnapshot], parent_id: uuid::Uuid) -> Vec<&StoredSnapshot> {
    records
        .iter()
        .filter(|r| r.parent_id == Some(parent_id))
        .collect()
}

/// All descendants of `start_id` (transitively), restricted to records with
/// `branch == expected` — used to prune exactly one branch's newer layers.
pub fn descendants_in_branch<'a>(
    records: &'a [StoredSnapshot],
    start_id: uuid::Uuid,
    expected_branch: Option<&str>,
) -> Vec<&'a StoredSnapshot> {
    let by_parent: HashMap<uuid::Uuid, Vec<&StoredSnapshot>> =
        records.iter().fold(HashMap::new(), |mut acc, r| {
            if let Some(parent) = r.parent_id {
                acc.entry(parent).or_default().push(r);
            }
            acc
        });

    let mut result = Vec::new();
    let mut stack: Vec<uuid::Uuid> = vec![start_id];
    while let Some(id) = stack.pop() {
        let children = by_parent.get(&id);
        if let Some(children) = children {
            for child in children {
                if child.branch.as_deref() == expected_branch {
                    result.push(*child);
                    stack.push(child.id);
                }
            }
        }
    }
    result
}

/// The top layer record of the instance's main branch (branch NULL, external
/// layer), if any. This is the parent a new snapshot's layer will back onto.
pub async fn main_chain_head(
    store: &Store,
    instance_id: InstanceId,
) -> Result<Option<StoredSnapshot>, DaemonError> {
    let records = store.load_snapshots(instance_id).await?;
    Ok(records
        .into_iter()
        .filter(|r| r.layer_path.is_some() && r.branch.is_none())
        .max_by(|a, b| a.created_at.cmp(&b.created_at)))
}

/// Resolves a layer's backing reference against the layer's own directory.
pub fn resolve_backing(layer: &Path, backing: &str) -> PathBuf {
    let backing = PathBuf::from(backing);
    if backing.is_absolute() {
        backing
    } else {
        layer
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(backing)
    }
}

/// Finds the head among `layers`: a layer no other layer references as its
/// backing. Prefers a head whose store record is on the main branch — after
/// a crash mid-restore the main branch head is the correct rebuild target
/// even when archived branches exist.
pub async fn find_head_layer(
    layers: &[PathBuf],
    records: &[StoredSnapshot],
) -> Result<PathBuf, DaemonError> {
    let mut referenced: HashSet<PathBuf> = HashSet::new();
    for layer in layers {
        let info = andler_disk::qcow2::info(layer)
            .await
            .map_err(DaemonError::Disk)?;
        if let Some(backing) = info.backing_file {
            referenced.insert(resolve_backing(layer, &backing));
        }
    }

    let mut fallback: Option<PathBuf> = None;
    for layer in layers {
        if referenced.contains(layer) {
            continue;
        }
        let is_main = records.iter().any(|r| {
            r.layer_path
                .as_deref()
                .map(|rel| layer.ends_with(rel))
                .unwrap_or(false)
                && r.branch.is_none()
        });
        if is_main {
            return Ok(layer.clone());
        }
        fallback = Some(layer.clone());
    }

    fallback.ok_or_else(|| DaemonError::Io {
        path: PathBuf::from("disk.snapshots"),
        source: std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no head layer found among snapshot layers",
        ),
    })
}

fn parse_layer_uuid(path: &Path) -> Option<uuid::Uuid> {
    let stem = path.file_stem()?.to_str()?;
    uuid::Uuid::parse_str(stem).ok()
}

/// Reconciles the on-disk chain against the store after a daemon restart.
///
/// `active_disk` is the instance's configured disk path (it can live
/// outside the registry directory when the user's TOML points elsewhere).
/// `files=false` (a QEMU process was adopted and is running) restricts the
/// pass to metadata: entries whose layer file vanished are removed, layers
/// in the chain without an entry get one. `files=true` additionally repairs
/// the disk: an orphaned `.tmp-*` overlay becomes the active disk (crash
/// between the rename pair), or the active disk is rebuilt on top of the
/// chain head (crash mid-restore); `.tmp-*` leftovers are removed.
pub async fn reconcile_chain(
    store: &Store,
    instance_id: InstanceId,
    instance_dir: &Path,
    active_disk: &Path,
    files: bool,
) -> Result<(), DaemonError> {
    let disk_path = active_disk.to_path_buf();
    let snap_dir = snapshots_dir(instance_dir);

    if !disk_path.exists() {
        if !files {
            tracing::warn!(
                instance_id = %instance_id,
                "active disk missing while a QEMU process is attached; chain not repaired"
            );
        } else if let Some(tmp) = pending_overlay(instance_dir).await? {
            tokio::fs::rename(&tmp, &disk_path)
                .await
                .map_err(|source| DaemonError::Io {
                    path: tmp.clone(),
                    source,
                })?;
            tracing::warn!(
                instance_id = %instance_id,
                tmp = %tmp.display(),
                "recovered active disk from an uncommitted snapshot overlay"
            );
        } else {
            let layers = list_layer_files(instance_dir).await?;
            if layers.is_empty() {
                tracing::warn!(
                    instance_id = %instance_id,
                    "active disk missing and no snapshot layers to rebuild from"
                );
                return Ok(());
            }
            let records = store.load_snapshots(instance_id).await?;
            let head = find_head_layer(&layers, &records).await?;
            let size = andler_disk::qcow2::virtual_size_bytes(&head)
                .await
                .map_err(DaemonError::Disk)?;
            andler_disk::qcow2::create_overlay(&disk_path, &head, size)
                .await
                .map_err(DaemonError::Disk)?;
            tracing::warn!(
                instance_id = %instance_id,
                head = %head.display(),
                "rebuilt active disk on top of chain head after an interrupted restore"
            );
        }
    }

    if files {
        if let Ok(mut entries) = tokio::fs::read_dir(&snap_dir).await {
            while let Some(entry) =
                entries
                    .next_entry()
                    .await
                    .map_err(|source| DaemonError::Io {
                        path: snap_dir.clone(),
                        source,
                    })?
            {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with(".tmp-") {
                    let _ = tokio::fs::remove_file(entry.path()).await;
                }
            }
        }
    }

    let chain = match andler_disk::qcow2::chain_from_head(&disk_path).await {
        Ok(chain) => chain,
        Err(err) => {
            tracing::warn!(
                instance_id = %instance_id,
                error = %err,
                "cannot inspect disk chain; metadata reconciliation skipped"
            );
            return Ok(());
        }
    };

    let mut records = store.load_snapshots(instance_id).await?;

    let mut by_path: HashMap<PathBuf, StoredSnapshot> = HashMap::new();
    for record in &records {
        if let Some(rel) = &record.layer_path {
            by_path.insert(instance_dir.join(rel), record.clone());
        }
    }

    for (path, record) in &by_path {
        if !path.exists() {
            tracing::warn!(
                instance_id = %instance_id,
                path = %path.display(),
                "snapshot layer file missing; dropping its metadata entry"
            );
            store.delete_snapshot_by_id(instance_id, record.id).await?;
        }
    }
    records = store.load_snapshots(instance_id).await?;
    by_path.clear();
    for record in &records {
        if let Some(rel) = &record.layer_path {
            by_path.insert(instance_dir.join(rel), record.clone());
        }
    }

    let mut prev_id: Option<uuid::Uuid> = None;
    for path in &chain {
        if path == &disk_path {
            continue;
        }
        if !path.starts_with(&snap_dir) {
            prev_id = None;
            continue;
        }
        if let Some(record) = by_path.get(path) {
            prev_id = Some(record.id);
            continue;
        }
        let Some(id) = parse_layer_uuid(path) else {
            prev_id = None;
            continue;
        };
        let rel = path.strip_prefix(instance_dir).unwrap_or(path);
        let record = StoredSnapshot {
            id,
            instance_id,
            tag: format!("recovered-{}", &id.to_string()[..8]),
            description: Some("recovered by daemon chain reconciliation".to_string()),
            created_at: chrono::Utc::now().to_rfc3339(),
            layer_path: Some(rel.to_string_lossy().into_owned()),
            parent_id: prev_id,
            branch: None,
        };
        store.save_snapshot(&record).await?;
        prev_id = Some(id);
        tracing::warn!(
            instance_id = %instance_id,
            path = %path.display(),
            "snapshot layer found on disk without metadata; recovered entry created"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempRoot(PathBuf);

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn record(id: u128, parent: Option<u128>, branch: Option<&str>) -> StoredSnapshot {
        StoredSnapshot {
            id: uuid::Uuid::from_u128(id),
            instance_id: InstanceId::new(),
            tag: format!("s-{id}"),
            description: None,
            created_at: "2026-01-01T00:00:00+00:00".to_string(),
            layer_path: Some(format!("disk.snapshots/{id}.qcow2")),
            parent_id: parent.map(uuid::Uuid::from_u128),
            branch: branch.map(str::to_string),
        }
    }

    #[test]
    fn ancestors_walk_parent_ids_to_base() {
        let records = vec![
            record(1, None, None),
            record(2, Some(1), None),
            record(3, Some(2), None),
        ];
        let chain = ancestors(&records, uuid::Uuid::from_u128(3));
        let ids: Vec<u128> = chain.iter().map(|r| r.id.as_u128()).collect();
        assert_eq!(ids, vec![3, 2, 1]);
    }

    #[test]
    fn ancestors_stop_at_missing_parent() {
        let records = vec![record(2, Some(1), None), record(3, Some(2), None)];
        let chain = ancestors(&records, uuid::Uuid::from_u128(3));
        let ids: Vec<u128> = chain.iter().map(|r| r.id.as_u128()).collect();
        assert_eq!(ids, vec![3, 2]);
    }

    #[test]
    fn descendants_respect_branch() {
        let records = vec![
            record(1, None, None),
            record(2, Some(1), None),
            record(3, Some(2), None),
            record(4, Some(2), Some("branch-x")),
            record(5, Some(4), Some("branch-x")),
        ];
        let main = descendants_in_branch(&records, uuid::Uuid::from_u128(1), None);
        let mut ids: Vec<u128> = main.iter().map(|r| r.id.as_u128()).collect();
        ids.sort();
        assert_eq!(ids, vec![2, 3]);

        // branch-x diverges at layer 2; walk from 2 keeps only that branch
        let branch = descendants_in_branch(&records, uuid::Uuid::from_u128(2), Some("branch-x"));
        let mut ids: Vec<u128> = branch.iter().map(|r| r.id.as_u128()).collect();
        ids.sort();
        assert_eq!(ids, vec![4, 5]);

        // walk from 1 in branch-x never crosses main layer 2
        let branch_from_base =
            descendants_in_branch(&records, uuid::Uuid::from_u128(1), Some("branch-x"));
        assert!(branch_from_base.is_empty());
    }

    #[test]
    fn direct_children_lists_immediate_layers() {
        let records = vec![
            record(1, None, None),
            record(2, Some(1), None),
            record(3, Some(2), None),
        ];
        let children = direct_children(&records, uuid::Uuid::from_u128(1));
        let ids: Vec<u128> = children.iter().map(|r| r.id.as_u128()).collect();
        assert_eq!(ids, vec![2]);
    }

    #[test]
    fn resolve_backing_joins_relative_against_layer_dir() {
        assert_eq!(
            resolve_backing(Path::new("/i/disk.snapshots/a.qcow2"), "b.qcow2"),
            PathBuf::from("/i/disk.snapshots/b.qcow2")
        );
        assert_eq!(
            resolve_backing(Path::new("/i/disk.snapshots/a.qcow2"), "/i/disk.qcow2"),
            PathBuf::from("/i/disk.qcow2")
        );
    }

    async fn tmp_instance_dir() -> (TempRoot, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "andler-diskchain-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&root)
            .await
            .expect("creating test temp dir must succeed");
        let instance_dir = root.join(InstanceId::new().to_string());
        tokio::fs::create_dir_all(instance_dir.join(SNAPSHOTS_DIR_NAME))
            .await
            .expect("creating snapshot dir must succeed");
        (TempRoot(root), instance_dir)
    }

    #[tokio::test]
    #[ignore = "requires qemu-img, see docker/e2e/README.md integration-test target"]
    async fn find_head_layer_prefers_main_branch_and_falls_back() {
        let (_root, instance_dir) = tmp_instance_dir().await;
        let snap_dir = instance_dir.join(SNAPSHOTS_DIR_NAME);

        let a = snap_dir.join("aaaa.qcow2");
        let b = snap_dir.join("bbbb.qcow2");
        andler_disk::qcow2::create(&a, 8 * 1024 * 1024)
            .await
            .expect("creating a must succeed");
        andler_disk::qcow2::create(&b, 8 * 1024 * 1024)
            .await
            .expect("creating b must succeed");

        // B has a main-branch record: the head must be B even though A is
        // listed first (main-branch preference over file order).
        let record_b = StoredSnapshot {
            id: uuid::Uuid::from_u128(2),
            instance_id: InstanceId::new(),
            tag: "b".to_string(),
            description: None,
            created_at: "2026-01-01T00:00:00+00:00".to_string(),
            layer_path: Some("disk.snapshots/bbbb.qcow2".to_string()),
            parent_id: None,
            branch: None,
        };
        let head = find_head_layer(&[a.clone(), b.clone()], &[record_b])
            .await
            .expect("head must be found");
        assert_eq!(head, b);

        // Neither layer has a record (double crash mid-branching): fall back
        // to the last unreferenced layer instead of failing.
        let head = find_head_layer(&[a.clone(), b.clone()], &[])
            .await
            .expect("head must fall back to an unreferenced layer");
        assert_eq!(head, b);
    }

    #[tokio::test]
    #[ignore = "requires qemu-img, see docker/e2e/README.md integration-test target"]
    async fn reconcile_promotes_tmp_overlay_to_active_disk() {
        let (_dir, instance_dir) = tmp_instance_dir().await;
        let store = andler_store::Store::open_in_memory()
            .await
            .expect("in-memory store must open");
        let instance_id = InstanceId::new();
        let snap_dir = instance_dir.join(SNAPSHOTS_DIR_NAME);
        let tmp = snap_dir.join(".tmp-deadbeef.qcow2");
        tokio::fs::write(&tmp, b"not a real qcow2; rename is all that matters")
            .await
            .expect("writing tmp overlay must succeed");

        reconcile_chain(
            &store,
            instance_id,
            &instance_dir,
            &instance_dir.join("disk.qcow2"),
            true,
        )
        .await
        .expect("reconciliation must not fail");

        assert!(instance_dir.join("disk.qcow2").exists());
        assert!(!tmp.exists());
    }

    #[tokio::test]
    #[ignore = "requires qemu-img, see docker/e2e/README.md integration-test target"]
    async fn reconcile_rebuilds_active_disk_after_interrupted_restore() {
        let (_dir, instance_dir) = tmp_instance_dir().await;
        let store = andler_store::Store::open_in_memory()
            .await
            .expect("in-memory store must open");
        let instance_id = InstanceId::new();
        let snap_dir = instance_dir.join(SNAPSHOTS_DIR_NAME);

        let base = instance_dir.join("base.qcow2");
        andler_disk::qcow2::create(&base, 8 * 1024 * 1024)
            .await
            .expect("creating base must succeed");
        let layer_id = uuid::Uuid::new_v4();
        let layer = snap_dir.join(format!("{layer_id}.qcow2"));
        andler_disk::qcow2::create_overlay(&layer, &base, 8 * 1024 * 1024)
            .await
            .expect("creating layer must succeed");

        store
            .save_snapshot(&StoredSnapshot {
                id: layer_id,
                instance_id,
                tag: "s-1".to_string(),
                description: None,
                created_at: "2026-01-01T00:00:00+00:00".to_string(),
                layer_path: Some(
                    layer
                        .strip_prefix(&instance_dir)
                        .expect("layer must live inside instance dir")
                        .to_string_lossy()
                        .into_owned(),
                ),
                parent_id: None,
                branch: None,
            })
            .await
            .expect("saving layer record must succeed");

        // active disk vanished (interrupted restore): rebuild on chain head
        reconcile_chain(
            &store,
            instance_id,
            &instance_dir,
            &instance_dir.join("disk.qcow2"),
            true,
        )
        .await
        .expect("reconciliation must not fail");

        let active = instance_dir.join("disk.qcow2");
        assert!(active.exists());
        let info = andler_disk::qcow2::info(&active)
            .await
            .expect("active disk must be a readable qcow2");
        assert_eq!(
            info.backing_file.as_deref(),
            Some(layer.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    #[ignore = "requires qemu-img, see docker/e2e/README.md integration-test target"]
    async fn reconcile_recovers_unregistered_layers_as_entries() {
        let (_dir, instance_dir) = tmp_instance_dir().await;
        let store = andler_store::Store::open_in_memory()
            .await
            .expect("in-memory store must open");
        let instance_id = InstanceId::new();
        let snap_dir = instance_dir.join(SNAPSHOTS_DIR_NAME);

        let base = instance_dir.join("base.qcow2");
        andler_disk::qcow2::create(&base, 8 * 1024 * 1024)
            .await
            .expect("creating base must succeed");
        let layer_id = uuid::Uuid::new_v4();
        let layer = snap_dir.join(format!("{layer_id}.qcow2"));
        andler_disk::qcow2::create_overlay(&layer, &base, 8 * 1024 * 1024)
            .await
            .expect("creating layer must succeed");
        let active = instance_dir.join("disk.qcow2");
        andler_disk::qcow2::create_overlay(&active, &layer, 8 * 1024 * 1024)
            .await
            .expect("creating active disk must succeed");

        reconcile_chain(
            &store,
            instance_id,
            &instance_dir,
            &instance_dir.join("disk.qcow2"),
            true,
        )
        .await
        .expect("reconciliation must not fail");

        let records = store
            .load_snapshots(instance_id)
            .await
            .expect("loading records must succeed");
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].tag,
            format!("recovered-{}", &layer_id.to_string()[..8])
        );
        assert_eq!(
            records[0].layer_path.as_deref(),
            Some(
                layer
                    .strip_prefix(&instance_dir)
                    .expect("layer must live inside instance dir")
                    .to_string_lossy()
                    .as_ref()
            )
        );
        assert_eq!(records[0].parent_id, None);
    }

    #[tokio::test]
    #[ignore = "requires qemu-img, see docker/e2e/README.md integration-test target"]
    async fn reconcile_drops_entries_for_missing_layer_files() {
        let (_dir, instance_dir) = tmp_instance_dir().await;
        let store = andler_store::Store::open_in_memory()
            .await
            .expect("in-memory store must open");
        let instance_id = InstanceId::new();

        let base = instance_dir.join("base.qcow2");
        andler_disk::qcow2::create(&base, 8 * 1024 * 1024)
            .await
            .expect("creating base must succeed");
        let active = instance_dir.join("disk.qcow2");
        andler_disk::qcow2::create_overlay(&active, &base, 8 * 1024 * 1024)
            .await
            .expect("creating active disk must succeed");

        // entry whose layer file is gone (deleted behind the daemon's back)
        let gone_id = uuid::Uuid::new_v4();
        store
            .save_snapshot(&StoredSnapshot {
                id: gone_id,
                instance_id,
                tag: "s-gone".to_string(),
                description: None,
                created_at: "2026-01-01T00:00:00+00:00".to_string(),
                layer_path: Some(format!("disk.snapshots/{gone_id}.qcow2")),
                parent_id: None,
                branch: None,
            })
            .await
            .expect("saving record must succeed");

        reconcile_chain(
            &store,
            instance_id,
            &instance_dir,
            &instance_dir.join("disk.qcow2"),
            true,
        )
        .await
        .expect("reconciliation must not fail");

        let records = store
            .load_snapshots(instance_id)
            .await
            .expect("loading records must succeed");
        assert!(records.is_empty());
    }

    #[tokio::test]
    #[ignore = "requires qemu-img, see docker/e2e/README.md integration-test target"]
    async fn reconcile_without_files_leaves_tmp_overlay_alone() {
        let (_dir, instance_dir) = tmp_instance_dir().await;
        let store = andler_store::Store::open_in_memory()
            .await
            .expect("in-memory store must open");
        let instance_id = InstanceId::new();
        let snap_dir = instance_dir.join(SNAPSHOTS_DIR_NAME);
        let tmp = snap_dir.join(".tmp-deadbeef.qcow2");
        tokio::fs::write(&tmp, b"overlay of a live VM; must not be moved")
            .await
            .expect("writing tmp overlay must succeed");

        reconcile_chain(
            &store,
            instance_id,
            &instance_dir,
            &instance_dir.join("disk.qcow2"),
            false,
        )
        .await
        .expect("metadata-only reconciliation must not fail");

        assert!(tmp.exists());
        assert!(!instance_dir.join("disk.qcow2").exists());
    }
}
