use std::path::Path;

use andler_core::DaemonEvent;

/// events.jsonl audit log, one JSON line per event, appended by the
/// operation that owns the event (supervisor for lifecycle, daemon for
/// config operations). Rotation is logrotate-style by size: when the active
/// file reaches `MAX_BYTES` it is shifted to `.1` and older files to `.2`/
/// `.3`; older ones are dropped, keeping `KEEP` files total.
const MAX_BYTES: u64 = 1024 * 1024;
const KEEP: usize = 4;

pub(crate) async fn append_event(instance_dir: &Path, event: &DaemonEvent) {
    let line = match event.to_jsonl_line() {
        Ok(line) => line,
        Err(err) => {
            tracing::error!(
                instance_dir = %instance_dir.display(),
                error = %err,
                "failed to serialize audit event"
            );
            return;
        }
    };

    let path = instance_dir.join("events.jsonl");
    rotate_if_needed(&path).await;

    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).append(true);
    let mut file = match options.open(&path).await {
        Ok(file) => file,
        Err(err) => {
            tracing::error!(
                path = %path.display(),
                error = %err,
                "failed to open audit log for append"
            );
            return;
        }
    };
    use tokio::io::AsyncWriteExt;
    if let Err(err) = file.write_all(format!("{line}\n").as_bytes()).await {
        tracing::error!(
            path = %path.display(),
            error = %err,
            "failed to append audit event"
        );
    }
}

async fn rotate_if_needed(path: &Path) {
    let size = match tokio::fs::metadata(path).await {
        Ok(meta) => meta.len(),
        Err(_) => return,
    };
    if size < MAX_BYTES {
        return;
    }

    for i in (1..KEEP - 1).rev() {
        let from = rotated_path(path, i);
        let to = rotated_path(path, i + 1);
        match tokio::fs::rename(&from, &to).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(
                    from = %from.display(),
                    to = %to.display(),
                    error = %err,
                    "audit log rotation shift failed"
                );
            }
        }
    }
    match tokio::fs::rename(path, &rotated_path(path, 1)).await {
        Ok(()) => {}
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "audit log rotation failed"
            );
        }
    }
}

fn rotated_path(path: &Path, index: usize) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{index}"));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use andler_core::{EventKind, InstanceId, InstanceState};

    fn sample_event(id: InstanceId) -> DaemonEvent {
        DaemonEvent {
            ts_ms: 1_700_000_000_000,
            instance_id: Some(id),
            kind: EventKind::Lifecycle {
                from: InstanceState::Created,
                to: InstanceState::Running,
                reason: None,
            },
        }
    }

    #[tokio::test]
    async fn append_creates_file_with_one_line_per_event() {
        let dir = std::env::temp_dir().join(format!("andler-audit-test-{}", InstanceId::new()));
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let id = InstanceId::new();
        append_event(&dir, &sample_event(id)).await;
        append_event(&dir, &sample_event(id)).await;

        let content = tokio::fs::read_to_string(dir.join("events.jsonl"))
            .await
            .unwrap();
        assert_eq!(content.lines().count(), 2);

        let parsed: DaemonEvent = serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(parsed.instance_id, Some(id));
        assert!(matches!(parsed.kind, EventKind::Lifecycle { .. }));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn rotation_shifts_files_and_keeps_bounded() {
        let dir = std::env::temp_dir().join(format!("andler-audit-test-{}", InstanceId::new()));
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let id = InstanceId::new();
        let path = dir.join("events.jsonl");

        // A file over the rotation threshold triggers rotation on the next append.
        tokio::fs::write(&path, "x".repeat((MAX_BYTES + 100) as usize))
            .await
            .unwrap();
        append_event(&dir, &sample_event(id)).await;

        assert!(tokio::fs::metadata(dir.join("events.jsonl.1"))
            .await
            .is_ok());
        assert!(tokio::fs::metadata(&path).await.is_ok());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
