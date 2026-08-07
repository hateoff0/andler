//! Parsing of `/proc/self/mountinfo` and the "managed mount" checks that gate
//! every guest-filesystem operation.
//!
//! The core invariant: the helper may only touch mounts *it* (i.e. andlerd)
//! created — a guest root partition mounted from an `/dev/nbd*` device, the
//! tmpfs and host binds layered into it. Everything a subcommand touches must
//! resolve inside that tree; anything else is refused with exit 2. This is what
//! keeps a malformed path (a `..`, a symlink that escapes the guest, a chroot
//! into `/etc`) from becoming a host-side operation.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    pub root: PathBuf,
    pub mount_point: PathBuf,
    pub fs_type: String,
    pub source: String,
}

pub fn read_mountinfo() -> Vec<MountEntry> {
    std::fs::read_to_string("/proc/self/mountinfo")
        .map(|content| content.lines().filter_map(parse_line).collect())
        .unwrap_or_default()
}

fn parse_line(line: &str) -> Option<MountEntry> {
    let (left, right) = line.split_once(" - ")?;
    let left: Vec<&str> = left.split_whitespace().collect();
    let right: Vec<&str> = right.split_whitespace().collect();
    if left.len() < 5 || right.len() < 2 {
        return None;
    }
    Some(MountEntry {
        root: PathBuf::from(unescape(left[3])),
        mount_point: PathBuf::from(unescape(left[4])),
        fs_type: right[0].to_string(),
        source: unescape(right[1]),
    })
}

/// mountinfo escapes spaces, tabs, newlines and backslashes as octal sequences.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            match &bytes[i + 1..i + 4] {
                b"040" => {
                    out.push(b' ');
                    i += 4;
                    continue;
                }
                b"011" => {
                    out.push(b'\t');
                    i += 4;
                    continue;
                }
                b"012" => {
                    out.push(b'\n');
                    i += 4;
                    continue;
                }
                b"134" => {
                    out.push(b'\\');
                    i += 4;
                    continue;
                }
                _ => {}
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// true when `source` is a whole/partitioned nbd device (`/dev/nbd0`, `/dev/nbd0p2`).
pub fn is_nbd_source(source: &str) -> bool {
    let Some(name) = source
        .strip_prefix("/dev/")
        .and_then(|s| s.strip_prefix("nbd"))
    else {
        return false;
    };
    let (core, partition) = match name.split_once('p') {
        Some((core, part)) => (core, Some(part)),
        None => (name, None),
    };
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    digits(core) && partition.is_none_or(digits)
}

/// The guest's own root filesystem: the mountinfo entry whose mount point is
/// exactly `mount`, rooted at `/` and sourced from an nbd partition.
pub fn is_managed_guest_root(mount: &Path, entries: &[MountEntry]) -> bool {
    entries
        .iter()
        .any(|e| e.mount_point == mount && e.root == Path::new("/") && is_nbd_source(&e.source))
}

/// The managed guest-root mount whose tree contains `path` (deepest match).
pub fn managed_root_for(path: &Path, entries: &[MountEntry]) -> Option<PathBuf> {
    let canon = crate::validate::canonicalize_ancestor(path).ok()?;
    let mut candidates: Vec<&PathBuf> = entries
        .iter()
        .filter(|e| e.root == Path::new("/") && is_nbd_source(&e.source))
        .filter(|e| crate::validate::same_or_under(&canon, &e.mount_point))
        .map(|e| &e.mount_point)
        .collect();
    candidates.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    candidates.into_iter().next().cloned()
}

/// True when `candidate` is one of the mounts inside the guest tree rooted at
/// `root` (the tree's own root mount included).
pub fn is_mount_in_tree(root: &Path, candidate: &Path, entries: &[MountEntry]) -> bool {
    let canon = std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    if !crate::validate::same_or_under(&canon, root) {
        return false;
    }
    entries.iter().any(|e| e.mount_point == canon)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(root: &str, mount_point: &str, fs_type: &str, source: &str) -> MountEntry {
        MountEntry {
            root: PathBuf::from(root),
            mount_point: PathBuf::from(mount_point),
            fs_type: fs_type.to_string(),
            source: source.to_string(),
        }
    }

    const SAMPLE_LINE: &str =
        "36 35 98:0 / /mnt rw,noatime shared:1 - ext3 /dev/root rw,errors=continue";

    #[test]
    fn parse_line_handles_standard_mountinfo_layout() {
        let e = parse_line(SAMPLE_LINE).expect("line must parse");
        assert_eq!(e.root, PathBuf::from("/"));
        assert_eq!(e.mount_point, PathBuf::from("/mnt"));
        assert_eq!(e.fs_type, "ext3");
        assert_eq!(e.source, "/dev/root");
    }

    #[test]
    fn parse_line_unescapes_octal_sequences() {
        let line =
            "36 35 98:0 / /run/user/1000/andler\\040mounts/a\\040b special:1 - ext4 /dev/nbd0p2 rw";
        let e = parse_line(line).expect("line must parse");
        assert_eq!(
            e.mount_point,
            PathBuf::from("/run/user/1000/andler mounts/a b")
        );
        assert_eq!(e.source, "/dev/nbd0p2");
    }

    #[test]
    fn parse_line_rejects_malformed_input() {
        assert!(parse_line("").is_none());
        assert!(parse_line("no separator here").is_none());
        assert!(parse_line("1 2 3 4").is_none());
    }

    #[test]
    fn nbd_source_pattern_matches_devices_only() {
        assert!(is_nbd_source("/dev/nbd0"));
        assert!(is_nbd_source("/dev/nbd0p2"));
        assert!(is_nbd_source("/dev/nbd12p3"));
        assert!(!is_nbd_source("/dev/nbd"));
        assert!(!is_nbd_source("/dev/nbdp2"));
        assert!(!is_nbd_source("/dev/nbd0x"));
        assert!(!is_nbd_source("/dev/sda1"));
        assert!(!is_nbd_source("nbd0"));
        assert!(!is_nbd_source("tmpfs"));
    }

    fn temp_mount(label: &str) -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("andler-helper-mnt-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn is_managed_guest_root_requires_root_and_nbd_source() {
        let m1 = temp_mount("root");
        let guest = entry("/", m1.to_str().unwrap(), "ext4", "/dev/nbd0p2");
        let tmpfs = entry("/", m1.join("run").to_str().unwrap(), "tmpfs", "tmpfs");
        let foreign = entry("/", "/var/lib/cool", "ext4", "/dev/sda2");
        let entries = vec![guest, tmpfs, foreign];

        assert!(is_managed_guest_root(Path::new(&m1), &entries));
        assert!(!is_managed_guest_root(Path::new(&m1.join("run")), &entries));
        // A foreign mount (not nbd-sourced) is never a managed guest root.
        assert!(!is_managed_guest_root(Path::new("/var/lib/cool"), &entries));
        // A root=%2F tmpfs mount of a non-nbd source is not a guest root.
        let non_nbd = entry("/", "/tmp/x", "tmpfs", "tmpfs");
        assert!(!is_managed_guest_root(Path::new("/tmp/x"), &[non_nbd]));
    }

    #[test]
    fn managed_root_for_picks_the_deepest_prefix() {
        let m1 = temp_mount("deep");
        let run = m1.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let guest = entry("/", m1.to_str().unwrap(), "ext4", "/dev/nbd0p2");
        let tmpfs = entry("/", run.to_str().unwrap(), "tmpfs", "tmpfs");
        let entries = vec![guest.clone(), tmpfs];

        assert_eq!(
            managed_root_for(&m1.join("etc/resolv.conf"), &entries).unwrap(),
            m1
        );
        assert_eq!(managed_root_for(&run.join("xdg"), &entries).unwrap(), m1);
        assert!(managed_root_for(Path::new("/etc/passwd"), &entries).is_none());
        let other = temp_mount("other");
        assert!(managed_root_for(&other, &entries).is_none());
    }

    #[test]
    fn is_mount_in_tree_accepts_own_tree_and_rejects_foreign() {
        let mnt = temp_mount("tree");
        let run = mnt.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let guest = entry("/", mnt.to_str().unwrap(), "ext4", "/dev/nbd0p2");
        let tmpfs = entry("/", run.to_str().unwrap(), "tmpfs", "tmpfs");
        let entries = vec![guest, tmpfs];
        let root = Path::new(&mnt);

        assert!(is_mount_in_tree(root, root, &entries));
        assert!(is_mount_in_tree(root, Path::new(&run), &entries));
        // Something mounted outside the tree (or a bare directory) is refused —
        // the helper must never unmount a mount it did not create.
        assert!(!is_mount_in_tree(root, Path::new("/home"), &entries));
        let var = mnt.join("var");
        std::fs::create_dir_all(&var).unwrap();
        assert!(!is_mount_in_tree(root, Path::new(&var), &entries));
    }
}
