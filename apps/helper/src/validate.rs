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

/// Parses an octal mode string (`755`, `0755`, `600`) into a permission mask.
pub fn parse_octal_mode(s: &str) -> Result<u32, String> {
    if s.is_empty() || s.len() > 4 || !s.chars().all(|c| matches!(c, '0'..='7')) {
        return Err(format!("invalid octal mode {s:?}"));
    }
    let mode = u32::from_str_radix(s, 8).map_err(|_| format!("invalid octal mode {s:?}"))?;
    if mode > 0o7777 {
        return Err(format!("mode {s:?} out of range"));
    }
    Ok(mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_or_under_is_component_wise() {
        assert!(same_or_under(Path::new("/a/b"), Path::new("/a")));
        assert!(same_or_under(Path::new("/a"), Path::new("/a")));
        assert!(!same_or_under(Path::new("/ab"), Path::new("/a")));
        assert!(!same_or_under(Path::new("/b"), Path::new("/a")));
    }

    #[test]
    fn canonicalize_ancestor_falls_back_to_existing_parent() {
        let base = std::env::temp_dir().join(format!("andler-helper-val-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let deep = base.join("missing").join("deeper");
        let canon = canonicalize_ancestor(&deep).unwrap();
        assert_eq!(canon, std::fs::canonicalize(&base).unwrap());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn nbd_dev_name_accepts_whole_and_partition_devices() {
        assert_eq!(nbd_dev_name("/dev/nbd0").unwrap(), "nbd0");
        assert_eq!(nbd_dev_name("/dev/nbd0p2").unwrap(), "nbd0p2");
        assert_eq!(nbd_dev_name("/dev/nbd12p3").unwrap(), "nbd12p3");
    }

    #[test]
    fn nbd_dev_name_rejects_garbage() {
        assert!(nbd_dev_name("/dev/sda").is_err());
        assert!(nbd_dev_name("/dev/nbd").is_err());
        assert!(nbd_dev_name("/dev/nbdp2").is_err());
        assert!(nbd_dev_name("/dev/nbd0x").is_err());
        assert!(nbd_dev_name("nbd0").is_err());
        assert!(nbd_dev_name("/dev/nbd/p0").is_err());
        assert!(nbd_dev_name("").is_err());
    }

    #[test]
    fn parse_octal_mode_accepts_usual_forms() {
        assert_eq!(parse_octal_mode("755").unwrap(), 0o755);
        assert_eq!(parse_octal_mode("0755").unwrap(), 0o755);
        assert_eq!(parse_octal_mode("600").unwrap(), 0o600);
        assert_eq!(parse_octal_mode("0").unwrap(), 0);
        assert_eq!(parse_octal_mode("7777").unwrap(), 0o7777);
    }

    #[test]
    fn parse_octal_mode_rejects_bad_input() {
        assert!(parse_octal_mode("").is_err());
        assert!(parse_octal_mode("8").is_err());
        assert!(parse_octal_mode("abc").is_err());
        assert!(parse_octal_mode("77777").is_err());
        assert!(parse_octal_mode("-1").is_err());
    }
}
