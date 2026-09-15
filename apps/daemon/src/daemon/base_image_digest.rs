use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::DaemonError;

/// What a base image's digest was computed for: the pair that changes when the
/// file is replaced, which is exactly what the pin check is about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Entry {
    size: u64,
    /// Nanoseconds since the epoch; `0` when the platform cannot say.
    mtime_ns: u128,
    sha256: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Memo {
    #[serde(default)]
    images: HashMap<PathBuf, Entry>,
}

/// The sha256 of a base image, remembered per `(path, size, mtime)`.
///
/// Why: the pin derive hashes the whole image — 2.5 GB for a published GApps
/// build — on *every* create. A release build does that in about a second, a
/// debug build (the one a developer runs) at 48 MB/s, so fifty seconds. The
/// memo makes every create after the first a stat plus a compare, and keeps
/// the property the pin is about: replacing the file changes its size or its
/// mtime, which misses the memo and hashes again.
///
/// Not a security boundary — an actor who can rewrite the image can rewrite
/// the memo — but the same provenance check without the repeated read. A memo
/// that cannot be read or written is not an error either: the digest is
/// computed and the failure is logged.
pub fn sha256(path: &Path) -> Result<String, DaemonError> {
    sha256_with_memo(path, &andler_core::paths::base_image_digests_path())
}

/// The memo file is a parameter so the tests can hand each case its own: the
/// production path is one file under `ANDLER_HOME`, and tests sharing it would
/// race each other's read-modify-write (and write into the developer's own
/// cache).
fn sha256_with_memo(path: &Path, memo_path: &Path) -> Result<String, DaemonError> {
    let (size, mtime_ns) = identity(path)?;
    let key = canonical(path);

    if let Some(entry) = load(memo_path).images.get(&key) {
        if entry.size == size && entry.mtime_ns == mtime_ns {
            tracing::debug!(path = %path.display(), "base image digest served from the memo");
            return Ok(entry.sha256.clone());
        }
    }

    let digest = andler_core::base_image::sha256_of(path)?;
    store(
        memo_path,
        &key,
        &Entry {
            size,
            mtime_ns,
            sha256: digest.clone(),
        },
    );
    Ok(digest)
}

/// An unreadable image is a real failure, not a cache miss.
fn identity(path: &Path) -> Result<(u64, u128), DaemonError> {
    let meta = std::fs::metadata(path).map_err(|source| DaemonError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok((meta.len(), mtime_nanos(&meta)))
}

/// Two caches must not alias when the same image is reached through different
/// relative paths, so the memo is keyed by the absolute one.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn mtime_nanos(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_nanos())
        .unwrap_or(0)
}

fn load(path: &Path) -> Memo {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "base-image digest memo is unreadable; it will be rebuilt"
            );
            Memo::default()
        }),
        Err(_) => Memo::default(),
    }
}

fn store(path: &Path, key: &Path, entry: &Entry) {
    let mut memo = load(path);
    memo.images.insert(key.to_path_buf(), entry.clone());
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                path = %parent.display(),
                error = %e,
                "cannot create the digest memo's directory"
            );
            return;
        }
    }
    let bytes = match serde_json::to_vec(&memo) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(error = %e, "cannot encode the base-image digest memo");
            return;
        }
    };
    // Through a temporary file, so a reader never sees half a document.
    let temporary = path.with_extension("json.tmp");
    if let Err(e) =
        std::fs::write(&temporary, bytes).and_then(|()| std::fs::rename(&temporary, path))
    {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "cannot record a base-image digest; the next create will hash the image again"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("andler-digest-memo-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Seeds the memo the way `sha256` would, for a digest the file cannot
    /// have — seeing it come back proves the file was not hashed.
    fn seed(memo_path: &Path, path: &Path, digest: &str) {
        let (size, mtime_ns) = identity(path).unwrap();
        store(
            memo_path,
            &canonical(path),
            &Entry {
                size,
                mtime_ns,
                sha256: digest.to_string(),
            },
        );
    }

    #[test]
    fn a_matching_memo_entry_is_served_without_reading_the_image() {
        let dir = scratch("seeded");
        let image = dir.join("base.qcow2");
        std::fs::write(&image, b"not a real image").unwrap();
        let impossible = "\u{2740}".repeat(64);
        let memo = dir.join("memo.json");
        seed(&memo, &image, &impossible);

        assert_eq!(
            sha256_with_memo(&image, &memo).unwrap(),
            impossible,
            "a memo entry matching the file's size and mtime must win over hashing"
        );
    }

    #[test]
    fn a_rewritten_image_misses_the_memo_and_hashes_again() {
        let dir = scratch("rewritten");
        let image = dir.join("base.qcow2");
        std::fs::write(&image, b"content v1").unwrap();
        let memo = dir.join("memo.json");
        seed(&memo, &image, &"\u{2740}".repeat(64));

        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&image, b"content v2").unwrap();

        let fresh = sha256_with_memo(&image, &memo).unwrap();
        assert_eq!(
            fresh,
            andler_core::base_image::sha256_of(&image).unwrap(),
            "a rewritten image must be hashed, not answered from the memo"
        );
    }
}
