//! The privileged operations themselves. Every operation re-validates its
//! inputs (paths against the managed mount tree, devices against `/sys`,
//! commands against an allowlist) immediately before acting — nothing trusts
//! arguments on arrival, and no operation shells out.

use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{mountinfo, validate};

const HOST_BIND_SOURCES: [&str; 3] = ["/dev", "/proc", "/sys"];
const CHROOT_ALLOWLIST: [&str; 7] = ["apt-get", "apt", "dnf", "pacman", "ln", "dpkg", "rpm"];

fn find_in_path(bin: &str) -> Option<PathBuf> {
    let path: std::ffi::OsString = std::env::var_os("PATH")
        .unwrap_or_else(|| "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into());
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|full| full.is_file())
}

fn run_external(bin: &str, args: &[&str]) -> Result<(), String> {
    let full = find_in_path(bin).ok_or_else(|| format!("{bin} not found in PATH"))?;
    let status = Command::new(&full)
        .args(args)
        .status()
        .map_err(|e| format!("failed to run {}: {e}", full.display()))?;
    if !status.success() {
        return Err(format!(
            "{} failed with {} (run it manually to see the full error)",
            full.display(),
            status
        ));
    }
    Ok(())
}

fn read_mountinfo() -> Vec<mountinfo::MountEntry> {
    mountinfo::read_mountinfo()
}

fn ensure_guest_root(mount: &Path) -> Result<(), String> {
    let entries = read_mountinfo();
    if !mountinfo::is_managed_guest_root(mount, &entries) {
        return Err(format!(
            "{} is not a managed guest root mount (not an nbd-mounted guest partition)",
            mount.display()
        ));
    }
    Ok(())
}

/// The deepest existing ancestor of `path` must resolve inside `root`.
fn canonicalize_ancestor_under(root: &Path, path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    let canon = validate::canonicalize_ancestor(path)
        .map_err(|e| format!("cannot resolve {}: {e}", path.display()))?;
    if !validate::same_or_under(&canon, root) {
        return Err(format!(
            "{} does not resolve inside the managed guest mount {}",
            path.display(),
            root.display()
        ));
    }
    Ok(())
}

/// Turn a guest path argument into a path relative to the guest root.
/// Both forms are accepted, matching what andler-disk sends:
///   - mount-absolute: `<mount>/etc/resolv.conf`
///   - guest-absolute: `/etc/resolv.conf` (relative to the guest root)
///
/// ParentDir components are rejected; the guest root itself is refused.
fn guest_rel(mount: &Path, path: &Path) -> Result<PathBuf, String> {
    let rel = if let Ok(r) = path.strip_prefix(mount) {
        r.to_path_buf()
    } else if path.is_absolute() {
        path.strip_prefix("/")
            .map_err(|_| format!("{path:?} is just the guest root"))?
            .to_path_buf()
    } else {
        return Err(format!(
            "{path:?} must be an absolute path inside the guest (either under \
             {} or starting with /)",
            mount.display()
        ));
    };
    if rel.as_os_str().is_empty() {
        return Err("refusing to operate on the guest root itself".into());
    }
    if rel
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("{} escapes the guest root via ..", path.display()));
    }
    Ok(rel)
}

/// The parent directory of `path` must exist and be inside the guest root.
fn validated_parent(root: &Path, path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    canonicalize_ancestor_under(root, parent)
}

/// A *fully existing* path whose canonical form sits inside the guest root.
fn validated_existing(root: &Path, path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    let canon =
        fs::canonicalize(path).map_err(|e| format!("cannot resolve {}: {e}", path.display()))?;
    if !validate::same_or_under(&canon, root) {
        return Err(format!(
            "{} escapes the managed guest mount {}",
            path.display(),
            root.display()
        ));
    }
    Ok(canon)
}

fn sys_block(name: &str) -> PathBuf {
    Path::new("/sys/class/block").join(name)
}

fn nbd_free(name: &str) -> Result<(), String> {
    let sys = sys_block(name);
    if !sys.is_dir() {
        return Err(format!("/sys/class/block/{name} does not exist"));
    }
    if sys.join("pid").exists() {
        return Err(format!("/dev/{name} is already connected"));
    }
    let size = fs::read_to_string(sys.join("size"))
        .map_err(|e| format!("cannot read {}: {e}", sys.join("size").display()))?;
    if size.trim() != "0" {
        return Err(format!("/dev/{name} is already in use"));
    }
    Ok(())
}

fn nbd_partition_ready(name: &str) -> Result<(), String> {
    let sys = sys_block(name);
    if !sys.is_dir() {
        return Err(format!("/sys/class/block/{name} does not exist"));
    }
    let size = fs::read_to_string(sys.join("size"))
        .map_err(|e| format!("cannot read {}: {e}", sys.join("size").display()))?;
    if size.trim() == "0" {
        return Err(format!("partition /dev/{name} has no size"));
    }
    Ok(())
}

pub fn nbd_connect(dev: &str, image: &str) -> Result<i32, String> {
    let name = validate::nbd_dev_name(dev)?;
    if name.contains('p') {
        return Err("nbd-connect expects a whole device, not a partition".into());
    }
    nbd_free(&name)?;

    if !Path::new(image).is_absolute() {
        return Err(format!("image path must be absolute, got {image:?}"));
    }
    let meta = fs::metadata(image).map_err(|e| format!("cannot stat image {image}: {e}"))?;
    if !meta.file_type().is_file() {
        return Err(format!("{image} is not a regular file"));
    }

    run_external("qemu-nbd", &["--connect", dev, image, "--format=qcow2"])?;
    Ok(0)
}

pub fn nbd_disconnect(dev: &str) -> Result<i32, String> {
    let name = validate::nbd_dev_name(dev)?;
    if name.contains('p') {
        return Err("nbd-disconnect expects a whole device, not a partition".into());
    }
    // Idempotent: a device that is already free has nothing to disconnect.
    if !sys_block(&name).join("pid").exists() {
        return Ok(0);
    }
    run_external("qemu-nbd", &["--disconnect", dev])?;
    Ok(0)
}

pub fn modprobe_nbd() -> Result<i32, String> {
    run_external("modprobe", &["nbd", "max_part=8"])?;
    Ok(0)
}

fn check_mountpoint_owner(mp: &Path) -> Result<(), String> {
    // Under sudo the invoking user is SUDO_UID; the mount point directory was
    // created by andlerd (the invoking user), so it must be owned by them —
    // this is what stops mounting over /etc or any other root-owned directory.
    let Ok(uid_str) = std::env::var("SUDO_UID") else {
        return Ok(()); // run directly as root: no invoking user to compare
    };
    let uid: u32 = uid_str
        .parse()
        .map_err(|_| format!("SUDO_UID={uid_str} is not a numeric uid"))?;
    let meta =
        fs::metadata(mp).map_err(|e| format!("cannot stat mount point {}: {e}", mp.display()))?;
    if meta.uid() != uid {
        return Err(format!(
            "mount point {} is owned by uid {}, not the invoking user ({uid})",
            mp.display(),
            meta.uid()
        ));
    }
    Ok(())
}

pub fn mount_partition(dev: &str, mp: &str) -> Result<i32, String> {
    let name = validate::nbd_dev_name(dev)?;
    let base = name.split('p').next().unwrap_or(&name);
    if !sys_block(base).is_dir() {
        return Err(format!("/sys/class/block/{base} does not exist"));
    }
    if name.contains('p') {
        nbd_partition_ready(&name)?;
    }

    let mount_point = Path::new(mp);
    if !mount_point.is_absolute() {
        return Err(format!("mount point must be absolute, got {mp:?}"));
    }
    if !fs::symlink_metadata(mount_point)
        .map_err(|e| format!("mount point {}: {e}", mount_point.display()))?
        .is_dir()
    {
        return Err(format!(
            "mount point {} is not a directory",
            mount_point.display()
        ));
    }
    check_mountpoint_owner(mount_point)?;

    let entries = read_mountinfo();
    let canon = fs::canonicalize(mount_point)
        .map_err(|e| format!("cannot canonicalize {}: {e}", mount_point.display()))?;
    if entries.iter().any(|e| e.mount_point == canon) {
        return Err(format!(
            "{} is already a mount point",
            mount_point.display()
        ));
    }

    run_external("mount", &["-o", "rw", dev, mp])?;
    Ok(0)
}

fn target_is_plain_dir(target: &Path, entries: &[mountinfo::MountEntry]) -> Result<(), String> {
    if !target.is_absolute() {
        return Err(format!("target must be absolute, got {}", target.display()));
    }
    if !fs::symlink_metadata(target)
        .map_err(|e| format!("target {}: {e}", target.display()))?
        .is_dir()
    {
        return Err(format!("target {} is not a directory", target.display()));
    }
    let canon = fs::canonicalize(target)
        .map_err(|e| format!("cannot canonicalize {}: {e}", target.display()))?;
    if entries.iter().any(|e| e.mount_point == canon) {
        return Err(format!("{} is already a mount point", target.display()));
    }
    Ok(())
}

fn bind_source_canon(src: &str) -> Result<PathBuf, String> {
    if !Path::new(src).is_absolute() {
        return Err(format!("bind source must be absolute, got {src:?}"));
    }
    let canon =
        fs::canonicalize(src).map_err(|e| format!("cannot canonicalize bind source {src}: {e}"))?;
    if !HOST_BIND_SOURCES.iter().any(|s| canon == Path::new(s)) {
        return Err(format!(
            "bind source {src} is not one of {}",
            HOST_BIND_SOURCES.join(", ")
        ));
    }
    Ok(canon)
}

pub fn mount_bind(src: &str, tgt: &str) -> Result<i32, String> {
    let canon_src = bind_source_canon(src)?;
    let entries = read_mountinfo();
    let root = mountinfo::managed_root_for(Path::new(tgt), &entries)
        .ok_or_else(|| format!("{} is not inside a managed guest mount", tgt))?;
    let _ = root;
    target_is_plain_dir(Path::new(tgt), &entries)?;
    run_external("mount", &["--bind", canon_src.to_str().unwrap_or(src), tgt])?;
    Ok(0)
}

pub fn mount_tmpfs(tgt: &str) -> Result<i32, String> {
    let entries = read_mountinfo();
    let root = mountinfo::managed_root_for(Path::new(tgt), &entries)
        .ok_or_else(|| format!("{} is not inside a managed guest mount", tgt))?;
    let _ = root;
    target_is_plain_dir(Path::new(tgt), &entries)?;
    run_external("mount", &["-t", "tmpfs", "tmpfs", tgt])?;
    Ok(0)
}

pub fn umount(mp: &str) -> Result<i32, String> {
    let target = Path::new(mp);
    if !target.is_absolute() {
        return Err(format!("mount point must be absolute, got {mp:?}"));
    }
    let entries = read_mountinfo();
    let root = mountinfo::managed_root_for(target, &entries)
        .ok_or_else(|| format!("{} is not inside a managed guest mount", mp))?;
    if !mountinfo::is_mount_in_tree(&root, target, &entries) {
        return Err(format!("{} is not one of our mounts", mp));
    }
    run_external("umount", &["-l", mp])?;
    Ok(0)
}

fn chroot_into(mount: &Path) -> Result<(), String> {
    let c = CString::new(mount.as_os_str().as_bytes())
        .map_err(|_| "mount path contains a NUL byte".to_string())?;
    // SAFETY: chroot(2) only fails with EPERM/EACCES/ENOENT/ENOTDIR — all
    // reported below; the mount was just validated against mountinfo.
    if unsafe { libc::chroot(c.as_ptr()) } != 0 {
        return Err(format!(
            "chroot({}) failed: {}",
            mount.display(),
            std::io::Error::last_os_error()
        ));
    }
    std::env::set_current_dir("/")
        .map_err(|e| format!("cannot set working directory after chroot: {e}"))?;
    Ok(())
}

pub fn chroot_run(mount: &str, cmd: &str, args: &[String]) -> Result<i32, String> {
    let m = Path::new(mount);
    ensure_guest_root(m)?;
    if !CHROOT_ALLOWLIST.contains(&cmd) {
        return Err(format!(
            "chroot command {cmd:?} is not in the allowlist ({})",
            CHROOT_ALLOWLIST.join(", ")
        ));
    }

    chroot_into(m)?;
    let status = Command::new(cmd)
        .args(args)
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", "/root")
        .status()
        .map_err(|e| format!("failed to run {cmd} inside the guest: {e}"))?;
    Ok(status.code().unwrap_or(1))
}

/// Writes `contents` into `rel_path` inside the chrooted guest, replacing any
/// existing file or symlink first (a dangling symlink would otherwise make the
/// create fail with ENOENT).
fn write_guest_file(rel_path: &Path, contents: &[u8]) -> Result<(), String> {
    let full = Path::new("/").join(rel_path);
    match fs::remove_file(&full) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot remove {}: {e}", full.display())),
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&full)
        .map_err(|e| format!("cannot create {}: {e}", full.display()))?;
    file.write_all(contents)
        .map_err(|e| format!("cannot write {}: {e}", full.display()))?;
    file.sync_all()
        .map_err(|e| format!("cannot sync {}: {e}", full.display()))?;
    Ok(())
}

pub fn guest_write(mount: &str, path: &str) -> Result<i32, String> {
    let m = Path::new(mount);
    ensure_guest_root(m)?;

    let rel = guest_rel(m, Path::new(path))?;
    let full = m.join(&rel);
    // The parent must exist inside the guest (e.g. `/etc`); the deepest
    // existing ancestor check keeps a `..`-free but still escaping path out.
    validated_parent(m, &full)?;

    // Read stdin on the host side (there is no useful /dev/stdin in the guest).
    let mut contents = Vec::new();
    std::io::stdin()
        .read_to_end(&mut contents)
        .map_err(|e| format!("cannot read stdin: {e}"))?;

    chroot_into(m)?;
    write_guest_file(&rel, &contents)?;
    Ok(0)
}

fn remove_guest_path(path: &Path) -> Result<(), String> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("cannot stat {}: {e}", path.display())),
    };
    // With symlink_metadata a symlink is never a dir, so remove_dir_all only
    // ever sees real directories; links are removed with remove_file.
    if meta.is_dir() {
        fs::remove_dir_all(path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
    } else {
        fs::remove_file(path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
    }
    Ok(())
}

/// A `cp -a`-equivalent guest copy: preserves files, directories, modes and
/// symlinks. mtime/ownership parity is not needed for translator staging.
fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    let meta =
        fs::symlink_metadata(src).map_err(|e| format!("cannot stat {}: {e}", src.display()))?;
    if meta.file_type().is_symlink() {
        let target =
            fs::read_link(src).map_err(|e| format!("cannot read link {}: {e}", src.display()))?;
        symlink(&target, dst).map_err(|e| format!("cannot create {}: {e}", dst.display()))?;
        return Ok(());
    }
    if meta.is_dir() {
        fs::create_dir_all(dst).map_err(|e| format!("cannot create {}: {e}", dst.display()))?;
        for entry in fs::read_dir(src)
            .map_err(|e| format!("cannot read {}: {e}", src.display()))?
            .flatten()
        {
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        let mode = meta.permissions().mode() & 0o7777;
        fs::set_permissions(dst, fs::Permissions::from_mode(mode))
            .map_err(|e| format!("cannot chmod {}: {e}", dst.display()))?;
        return Ok(());
    }
    fs::copy(src, dst)
        .map_err(|e| format!("cannot copy {} -> {}: {e}", src.display(), dst.display()))?;
    Ok(())
}

fn one_path<'a>(op: &str, paths: &'a [String]) -> Result<&'a str, String> {
    match paths {
        [p] => Ok(p),
        _ => Err(format!(
            "file {op} expects exactly one path, got {}",
            paths.len()
        )),
    }
}

fn two_paths<'a>(op: &str, paths: &'a [String]) -> Result<(&'a str, &'a str), String> {
    match paths {
        [a, b] => Ok((a, b)),
        _ => Err(format!(
            "file {op} expects exactly two paths, got {}",
            paths.len()
        )),
    }
}

pub fn file_op(mount: &str, op: &str, paths: &[String]) -> Result<i32, String> {
    let m = Path::new(mount);
    ensure_guest_root(m)?;

    match op {
        "mkdir-p" => {
            let p = Path::new(one_path(op, paths)?);
            let rel = guest_rel(m, p)?;
            let full = m.join(&rel);
            validated_parent(m, &full)?;
            fs::create_dir_all(&full).map_err(|e| format!("mkdir {}: {e}", p.display()))?;
        }
        "rm-rf" => {
            let p = Path::new(one_path(op, paths)?);
            let rel = guest_rel(m, p)?;
            let full = m.join(&rel);
            validated_parent(m, &full)?;
            remove_guest_path(&full)?;
        }
        "mv" => {
            let (src, dst) = two_paths(op, paths)?;
            let src_rel = guest_rel(m, Path::new(src))?;
            let dst_rel = guest_rel(m, Path::new(dst))?;
            let full_src = m.join(&src_rel);
            let full_dst = m.join(&dst_rel);
            validated_parent(m, &full_src)?;
            validated_parent(m, &full_dst)?;
            fs::rename(&full_src, &full_dst)
                .map_err(|e| format!("cannot move {} -> {}: {e}", src, dst))?;
        }
        "cp-a" => {
            // The source is a *host* path (the translator cache lives under
            // ~/.andler, outside any guest mount) and is only read; the
            // destination is inside the guest. Writing stays restricted to
            // the managed mount; the read side is bounded by the daemon being
            // the only caller (a compromised daemon is root anyway).
            let (src, dst) = two_paths(op, paths)?;
            let src = Path::new(src);
            if !src.is_absolute() {
                return Err(format!("cp source must be absolute, got {src:?}"));
            }
            fs::symlink_metadata(src).map_err(|e| format!("cp source {}: {e}", src.display()))?;
            let rel = guest_rel(m, Path::new(dst))?;
            let full = m.join(&rel);
            validated_parent(m, &full)?;
            copy_tree(src, &full)?;
        }
        "chmod" => {
            let (mode_str, path) = two_paths(op, paths)?;
            let mode = validate::parse_octal_mode(mode_str)?;
            let rel = guest_rel(m, Path::new(path))?;
            let full = m.join(&rel);
            let canon = validated_existing(m, &full)?;
            fs::set_permissions(&canon, fs::Permissions::from_mode(mode))
                .map_err(|e| format!("cannot chmod {}: {e}", path))?;
        }
        other => return Err(format!("unknown file operation {other:?}")),
    }
    Ok(0)
}

pub fn sudoers_print() -> Result<i32, String> {
    let path = Path::new("/etc/sudoers.d/andler");
    if let Ok(content) = fs::read_to_string(path) {
        print!("{content}");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_external_propagates_failures_and_missing_bins() {
        run_external("true", &[]).expect("true must succeed");
        let err = run_external("false", &[]).expect_err("false must fail loudly");
        assert!(err.contains("failed with"), "err: {err}");
        let err = run_external("andler-helper-no-such-bin-xyz", &[])
            .expect_err("missing binary must be reported");
        assert!(err.contains("not found"), "err: {err}");
    }

    fn make_tree(base: &Path) {
        fs::create_dir_all(base.join("dir")).unwrap();
        fs::write(base.join("dir/file.txt"), b"content").unwrap();
        fs::write(base.join("top.txt"), b"top").unwrap();
        symlink("dir", base.join("linkdir")).unwrap();
    }

    #[test]
    fn copy_tree_preserves_files_dirs_and_symlinks() {
        let base = std::env::temp_dir().join(format!("andler-helper-cp-{}", std::process::id()));
        let src = base.join("src");
        let dst = base.join("dst");
        make_tree(&src);

        copy_tree(&src, &dst).expect("copy must succeed");

        assert_eq!(
            fs::read_to_string(dst.join("dir/file.txt")).unwrap(),
            "content"
        );
        assert_eq!(fs::read_to_string(dst.join("top.txt")).unwrap(), "top");
        assert!(fs::symlink_metadata(dst.join("linkdir"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_link(dst.join("linkdir")).unwrap(),
            Path::new("dir")
        );

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn copy_tree_copies_symlink_targets_as_links() {
        let base = std::env::temp_dir().join(format!("andler-helper-cpl-{}", std::process::id()));
        let src = base.join("src");
        fs::create_dir_all(&src).unwrap();
        symlink("elsewhere", src.join("l")).unwrap();
        let dst = base.join("dst");

        copy_tree(&src, &dst).expect("copy must succeed");
        assert!(fs::symlink_metadata(dst.join("l"))
            .unwrap()
            .file_type()
            .is_symlink());

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn remove_guest_path_is_idempotent_and_handles_links() {
        let base = std::env::temp_dir().join(format!("andler-helper-rm-{}", std::process::id()));
        fs::create_dir_all(base.join("sub")).unwrap();
        fs::write(base.join("f"), b"x").unwrap();
        symlink("f", base.join("l")).unwrap();

        remove_guest_path(&base.join("l")).expect("remove link");
        assert!(!base.join("l").exists());
        assert!(base.join("f").exists());
        remove_guest_path(&base.join("sub")).expect("remove dir");
        remove_guest_path(&base.join("nope")).expect("missing is a no-op");

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn mountpoint_owner_check_skipped_without_sudo_uid() {
        let base = std::env::temp_dir().join(format!("andler-helper-own-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        std::env::remove_var("SUDO_UID");
        check_mountpoint_owner(&base).expect("no SUDO_UID means no ownership constraint");
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn guest_rel_accepts_mount_absolute_and_guest_absolute_forms() {
        let mount = std::env::temp_dir().join(format!("andler-helper-mnt-{}", std::process::id()));
        fs::create_dir_all(&mount).unwrap();

        let rel = guest_rel(&mount, &mount.join("etc/resolv.conf")).expect("mount-absolute");
        assert_eq!(rel, Path::new("etc/resolv.conf"));

        let rel = guest_rel(&mount, Path::new("/etc/resolv.conf")).expect("guest-absolute");
        assert_eq!(rel, Path::new("etc/resolv.conf"));

        let rel = guest_rel(&mount, Path::new("/")).expect_err("guest root must be refused");
        assert!(rel.contains("guest root"));

        fs::remove_dir_all(&mount).ok();
    }

    #[test]
    #[test]
    fn chroot_allowlist_covers_package_manager_queries() {
        // offline `guest remove` runs the package manager's own query
        // binary (`dpkg -l` / `rpm -q`) through chroot-run; a query binary
        // missing from the allowlist makes every removal report
        // "package is not installed" while the install path (apt-get) works.
        for query_bin in ["dpkg", "rpm"] {
            assert!(
                CHROOT_ALLOWLIST.contains(&query_bin),
                "chroot allowlist must include {query_bin}"
            );
        }
    }

    fn guest_rel_rejects_escapes_and_relative_paths() {
        let mount = std::env::temp_dir().join(format!("andler-helper-esc-{}", std::process::id()));
        fs::create_dir_all(&mount).unwrap();

        let err = guest_rel(&mount, Path::new("/../etc/passwd")).expect_err(".. must be refused");
        assert!(err.contains(".."), "err: {err}");
        let err = guest_rel(&mount, &mount.join("../etc/passwd"))
            .expect_err("mount-absolute .. must be refused");
        assert!(err.contains(".."), "err: {err}");
        let err = guest_rel(&mount, Path::new("etc/passwd")).expect_err("relative must be refused");
        assert!(err.contains("absolute"), "err: {err}");

        fs::remove_dir_all(&mount).ok();
    }
}
