//! Path, device-name and mode validation shared by the operations.

use std::io;
use std::path::{Path, PathBuf};

/// Whether `candidate` is `base` or lives somewhere under it (component-wise —
/// `/xenon` is not under `/x`, `/x/ye` is).
pub fn same_or_under(candidate: &Path, base: &Path) -> bool {
    candidate == base || candidate.starts_with(base)
}

/// Canonicalize `path` itself or, if it does not exist yet, its deepest
/// existing ancestor. Used to pin every path to a real location before deciding
/// anything about it — unresolved paths can't be checked against the mount tree.
pub fn canonicalize_ancestor(path: &Path) -> io::Result<PathBuf> {
    let mut p = path;
    loop {
        match std::fs::canonicalize(p) {
            Ok(c) => return Ok(c),
            Err(_) => match p.parent() {
                Some(parent) => p = parent,
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("no existing ancestor of {}", path.display()),
                    ))
                }
            },
        }
    }
}

/// Validates a device name like `/dev/nbd0` or `/dev/nbd0p2` and returns the
/// bare kernel name (`nbd0`, `nbd0p2`).
pub fn nbd_dev_name(dev: &str) -> Result<String, String> {
    let p = Path::new(dev);
    if !p.is_absolute() {
        return Err(format!("nbd device must be an absolute path, got {dev:?}"));
    }
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("nbd device {dev:?} has no valid name"))?;
    let Some(core) = name.strip_prefix("nbd") else {
        return Err(format!("{dev:?} is not an /dev/nbd* device"));
    };
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    match core.split_once('p') {
        Some((base, part)) if digits(base) && digits(part) => Ok(name.to_string()),
        Some(_) => Err(format!("{dev:?} is not an /dev/nbd* device")),
        None if digits(core) => Ok(name.to_string()),
        None => Err(format!("{dev:?} is not an /dev/nbd* device")),
    }
}
