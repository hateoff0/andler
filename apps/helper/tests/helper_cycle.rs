//! Root integration cycle for the `andler-helper` binary: qcow2 → nbd-connect
//! → mount-partition → file ops → guest-write → mount-bind/umount →
//! umount → nbd-disconnect. Mirrors exactly what andlerd invokes via sudo for
//! offline guest operations. `#[ignore]`d: requires root, qemu-img, the nbd
//! kernel module and a free /dev/nbd* device.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn helper_bin() -> PathBuf {
    let mut bin = std::env::current_exe().expect("current_exe");
    bin.pop(); // deps/
    bin.pop(); // target/{debug,release}/
    bin.push("andler-helper");
    assert!(
        bin.is_file(),
        "helper binary not found at {}",
        bin.display()
    );
    bin
}

fn have(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn free_nbd_device() -> Option<String> {
    for name in ["/dev/nbd0", "/dev/nbd1", "/dev/nbd2"] {
        let sys = Path::new("/sys/class/block").join(name.trim_start_matches("/dev/"));
        if sys.join("pid").exists() {
            continue;
        }
        let size = std::fs::read_to_string(sys.join("size")).ok()?;
        if size.trim() == "0" && Path::new(name).exists() {
            return Some(name.to_string());
        }
    }
    None
}

fn run_helper(helper: &Path, args: &[&str]) -> std::process::Output {
    Command::new(helper)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn helper")
}

fn run_helper_stdin(helper: &Path, args: &[&str], input: &[u8]) -> std::process::Output {
    use std::io::Write;
    let mut child = Command::new(helper)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn helper");
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().expect("wait helper")
}

fn expect_success(out: &std::process::Output, what: &str) {
    if !out.status.success() {
        panic!(
            "{what} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
#[ignore = "requires root, qemu-img, the nbd module and a free /dev/nbd* device"]
fn offline_guest_cycle_through_the_helper() {
    // Running as root is required for every step; skip cleanly otherwise so the
    // ignored suite can still be collected on unprivileged machines.
    // SAFETY: geteuid(2) is a pure POSIX call — no arguments, no mutable state.
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: andler-helper cycle needs root");
        return;
    }
    if !have("qemu-img") {
        eprintln!("skipping: qemu-img not available");
        return;
    }
    if !have("mkfs.ext4") {
        eprintln!("skipping: mkfs.ext4 not available");
        return;
    }
    let Some(dev) = free_nbd_device() else {
        eprintln!("skipping: no free /dev/nbd* device (nbd module not loaded?)");
        return;
    };
    let helper = helper_bin();

    let work = std::env::temp_dir().join(format!("andler-helper-cycle-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let img = work.join("disk.qcow2");
    let mount = work.join("mount");
    std::fs::create_dir_all(&mount).unwrap();

    // --version / --help work unprivileged, any user.
    let version = Command::new(&helper).arg("--version").output().unwrap();
    expect_success(&version, "--version");
    assert!(String::from_utf8_lossy(&version.stdout).contains("andler-helper"));

    // qcow2 -> nbd.
    assert!(Command::new("qemu-img")
        .args(["create", "-f", "qcow2"])
        .arg(&img)
        .arg("256M")
        .status()
        .unwrap()
        .success());
    let out = run_helper(&helper, &["nbd-connect", &dev, img.to_str().unwrap()]);
    expect_success(&out, "nbd-connect");

    // Wait for the partitioned device, then ensure it is writable.
    std::thread::sleep(std::time::Duration::from_millis(500));
    // The image has no partitions yet — make a filesystem on the whole device,
    // which is what the helper's mount-partition validation expects.
    let mkfs = Command::new("mkfs.ext4")
        .arg("-F")
        .arg(&dev)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(mkfs.success(), "mkfs.ext4 failed on {dev}");

    let out = run_helper(&helper, &["mount-partition", &dev, mount.to_str().unwrap()]);
    expect_success(&out, "mount-partition");

    // file operations under the managed guest root.
    let mount_str = mount.to_str().unwrap();
    let out = run_helper(
        &helper,
        &[
            "file",
            mount_str,
            "mkdir-p",
            mount.join("etc").to_str().unwrap(),
        ],
    );
    expect_success(&out, "mkdir-p /etc");

    let resolv = mount.join("etc").join("resolv.conf");
    let out = run_helper_stdin(
        &helper,
        &["guest-write", mount_str, "/etc/resolv.conf"],
        b"nameserver 9.9.9.9\n",
    );
    expect_success(&out, "guest-write /etc/resolv.conf");
    assert_eq!(
        std::fs::read_to_string(&resolv).unwrap(),
        "nameserver 9.9.9.9\n",
        "guest-write replaced the guest file"
    );

    let a = mount.join("a.txt");
    std::fs::write(&a, b"hello").unwrap();
    let out = run_helper(
        &helper,
        &[
            "file",
            mount_str,
            "cp-a",
            a.to_str().unwrap(),
            mount.join("b.txt").to_str().unwrap(),
        ],
    );
    expect_success(&out, "cp-a");
    assert_eq!(
        std::fs::read_to_string(mount.join("b.txt")).unwrap(),
        "hello"
    );

    // The install flow's cp-a source is a *host* path outside the guest
    // (the translator cache under ~/.andler); it must be accepted as a
    // read-only source with the destination inside the guest.
    let host_src = work.join("host-payload");
    std::fs::create_dir_all(host_src.join("bin")).unwrap();
    std::fs::write(host_src.join("bin/app"), b"\x7fELF").unwrap();
    std::fs::write(host_src.join("top.cfg"), b"cfg").unwrap();
    let out = run_helper(
        &helper,
        &[
            "file",
            mount_str,
            "cp-a",
            host_src.to_str().unwrap(),
            mount.join("payload").to_str().unwrap(),
        ],
    );
    expect_success(&out, "cp-a (host source)");
    assert_eq!(
        std::fs::read_to_string(mount.join("payload/bin/app")).unwrap(),
        "\x7fELF"
    );
    assert_eq!(
        std::fs::read_to_string(mount.join("payload/top.cfg")).unwrap(),
        "cfg"
    );

    let out = run_helper(
        &helper,
        &[
            "file",
            mount_str,
            "chmod",
            "600",
            mount.join("a.txt").to_str().unwrap(),
        ],
    );
    expect_success(&out, "chmod");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&a).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let out = run_helper(
        &helper,
        &[
            "file",
            mount_str,
            "mv",
            mount.join("a.txt").to_str().unwrap(),
            mount.join("c.txt").to_str().unwrap(),
        ],
    );
    expect_success(&out, "mv");
    assert!(!a.exists() && mount.join("c.txt").exists());

    let out = run_helper(
        &helper,
        &[
            "file",
            mount_str,
            "rm-rf",
            mount.join("c.txt").to_str().unwrap(),
        ],
    );
    expect_success(&out, "rm-rf");
    assert!(!mount.join("c.txt").exists());

    // bind /dev and a tmpfs /run into the guest, then unmount exactly those.
    let dev_mount = mount.join("hostdev");
    std::fs::create_dir_all(&dev_mount).unwrap();
    let out = run_helper(
        &helper,
        &["mount-bind", "/dev", dev_mount.to_str().unwrap()],
    );
    expect_success(&out, "mount-bind /dev");
    let run_mount = mount.join("run");
    std::fs::create_dir_all(&run_mount).unwrap();
    let out = run_helper(&helper, &["mount-tmpfs", run_mount.to_str().unwrap()]);
    expect_success(&out, "mount-tmpfs");
    let out = run_helper(&helper, &["umount", run_mount.to_str().unwrap()]);
    expect_success(&out, "umount (tmpfs)");
    let out = run_helper(&helper, &["umount", dev_mount.to_str().unwrap()]);
    expect_success(&out, "umount (bind)");

    // Validation rejects escapes: a file op outside the guest mount must fail.
    let outside = work.join("outside.txt");
    std::fs::write(&outside, b"x").unwrap();
    let out = run_helper(
        &helper,
        &["file", mount_str, "rm-rf", outside.to_str().unwrap()],
    );
    assert!(
        !out.status.success(),
        "file op outside the managed mount must be refused"
    );

    // Tear down: unmount, disconnect (twice: idempotent).
    let out = run_helper(&helper, &["umount", mount_str]);
    expect_success(&out, "umount (root)");
    let out = run_helper(&helper, &["nbd-disconnect", &dev]);
    expect_success(&out, "nbd-disconnect");
    let out = run_helper(&helper, &["nbd-disconnect", &dev]);
    expect_success(&out, "nbd-disconnect (idempotent)");

    std::fs::remove_dir_all(&work).ok();
}
