use std::path::{Path, PathBuf};

use andler_rpc::proto::andler_service_client::AndlerServiceClient;

use crate::helpers::which;

enum Status {
    Ok(String),
    Warn(String),
    Fail(String),
}

/// A `youruser ALL=(root) NOPASSWD: <path>` line that `andler doctor --fix` can offer
/// to write to /etc/sudoers.d/andler on behalf of a failed/warning passwordless-sudo
/// check. Only ever attached to checks produced by `check_passwordless_sudo`.
#[derive(Clone)]
struct SudoersRule {
    binary: PathBuf,
}

struct Check {
    name: &'static str,
    status: Status,
    fix: Option<String>,
    sudoers_rule: Option<SudoersRule>,
}

impl Check {
    fn with_sudoers_rule(mut self, path: &Path) -> Check {
        self.sudoers_rule = Some(SudoersRule {
            binary: path.to_path_buf(),
        });
        self
    }

    fn needs_fix(&self) -> bool {
        !matches!(self.status, Status::Ok(_))
    }
}

fn ok(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        status: Status::Ok(detail.into()),
        fix: None,
        sudoers_rule: None,
    }
}

fn warn(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
    Check {
        name,
        status: Status::Warn(detail.into()),
        fix: Some(fix.into()),
        sudoers_rule: None,
    }
}

fn fail(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
    Check {
        name,
        status: Status::Fail(detail.into()),
        fix: Some(fix.into()),
        sudoers_rule: None,
    }
}

/// Runs a program via `sudo -n <path> --version` to test, side-effect-free, whether
/// passwordless sudo is configured for it — matches exactly what andlerd itself will
/// invoke at runtime, without actually connecting/mounting/chrooting into anything.
fn check_passwordless_sudo(name: &'static str, path: &Path) -> Check {
    let output = std::process::Command::new("sudo")
        .arg("-n")
        .arg(path)
        .arg("--version")
        .output();

    let fix = format!(
        "add via `sudo visudo`: youruser ALL=(root) NOPASSWD: {}",
        path.display()
    );

    let check = match output {
        Ok(o) if o.status.success() => {
            return ok(
                name,
                format!("passwordless sudo configured ({})", path.display()),
            )
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            if stderr.contains("password is required") || stderr.contains("no tty present") {
                fail(name, "passwordless sudo not configured", fix)
            } else {
                warn(
                    name,
                    format!("sudo ran but exited unexpectedly: {}", stderr.trim()),
                    fix,
                )
            }
        }
        Err(e) => fail(name, format!("could not run sudo: {e}"), fix),
    };

    // Only non-Ok checks reach here, so it's always correct to attach the rule that
    // would fix them — used by `andler doctor --fix` to build the sudoers snippet.
    check.with_sudoers_rule(path)
}

fn check_binary_and_sudo(name: &'static str, bin: &str) -> Vec<Check> {
    match which(bin) {
        Some(path) => vec![check_passwordless_sudo(name, &path)],
        None => vec![fail(
            name,
            format!("`{bin}` not found in PATH"),
            format!("install the package that provides `{bin}`"),
        )],
    }
}

fn hypervisor_checks() -> Vec<Check> {
    let mut checks = Vec::new();

    let kvm = Path::new("/dev/kvm");
    checks.push(if !kvm.exists() {
        fail(
            "/dev/kvm",
            "not found — KVM not available on this machine",
            "check `lsmod | grep kvm` and that virtualization is enabled in BIOS/UEFI",
        )
    } else {
        match std::fs::OpenOptions::new().read(true).write(true).open(kvm) {
            Ok(_) => ok("/dev/kvm", "accessible"),
            Err(e) => fail(
                "/dev/kvm",
                format!("found but not accessible: {e}"),
                "add your user to the `kvm` group: sudo usermod -aG kvm $USER (then re-login)",
            ),
        }
    });

    checks.push(match which("qemu-system-x86_64") {
        Some(p) => ok("qemu-system-x86_64", p.display().to_string()),
        None => fail(
            "qemu-system-x86_64",
            "not found in PATH",
            "install qemu-system-x86 (or your distro's qemu-full/qemu-desktop package)",
        ),
    });

    checks.push(match which("qemu-img") {
        Some(p) => ok("qemu-img", p.display().to_string()),
        None => fail(
            "qemu-img",
            "not found in PATH",
            "install qemu-img (usually part of qemu-utils)",
        ),
    });

    checks.push(match andler_firmware::detect_matched_pair() {
        Ok(found) => ok(
            "OVMF/UEFI firmware",
            format!(
                "{} + {}",
                found.code.display(),
                found.vars_template.display()
            ),
        ),
        Err(e) => fail(
            "OVMF/UEFI firmware",
            format!("not found: {e}"),
            "install your distro's OVMF/edk2-ovmf package",
        ),
    });

    checks
}

fn nbd_checks() -> Vec<Check> {
    let mut checks = Vec::new();

    match andler_disk::nbd::nbd_status() {
        Ok(status) => {
            checks.push(if status.loaded {
                ok(
                    "nbd kernel module",
                    format!(
                        "loaded ({} of {} devices free)",
                        status.free_devices, status.total_devices
                    ),
                )
            } else {
                warn(
                    "nbd kernel module",
                    "not loaded",
                    "andlerd will try to load it automatically the first time it's needed \
                     (requires the sudoers rule below); or load it now: sudo modprobe nbd max_part=8",
                )
            });
        }
        Err(e) => checks.push(fail(
            "nbd kernel module",
            format!("cannot inspect: {e}"),
            "Run: sudo modprobe nbd max_part=8",
        )),
    }

    // modprobe's passwordless-sudo rule is what makes the "andlerd will try to load it
    // automatically" note above actually true — without it, try_autoload_nbd_module()
    // fails silently (see nbd.rs) and the user only finds out when something using nbd
    // breaks later. Check it explicitly rather than leaving that as a silent gap.
    checks.extend(check_binary_and_sudo("modprobe", "modprobe"));
    checks.extend(check_binary_and_sudo("qemu-nbd", "qemu-nbd"));
    checks.extend(check_binary_and_sudo("mount", "mount"));
    checks.extend(check_binary_and_sudo("umount", "umount"));
    checks.extend(check_binary_and_sudo("chroot", "chroot"));

    checks
}

async fn daemon_check(addr: &str) -> Check {
    let connect = AndlerServiceClient::connect(addr.to_string());
    match tokio::time::timeout(std::time::Duration::from_secs(3), connect).await {
        Ok(Ok(_client)) => ok("andlerd", format!("reachable at {addr}")),
        Ok(Err(e)) => fail(
            "andlerd",
            format!("not reachable at {addr}: {e}"),
            "start it with: andlerd",
        ),
        Err(_) => fail(
            "andlerd",
            format!("timed out connecting to {addr}"),
            "check --daemon-addr / ANDLERD_ADDR, and that andlerd is running there",
        ),
    }
}

fn base_image_check() -> Check {
    let dir = andler_core::paths::base_images_dir();
    let images: Vec<_> = andler_core::base_image::list_all()
        .map(|infos| infos.into_iter().map(|i| i.qcow2_path).collect())
        .unwrap_or_default();

    if images.is_empty() {
        warn(
            "base images",
            format!("none found in {}", dir.display()),
            "build one with docker/images/build.sh <android-major> <VANILLA|GAPPS>",
        )
    } else {
        let total_bytes: u64 = images
            .iter()
            .filter_map(|p| p.metadata().ok())
            .map(|m| m.len())
            .sum();
        ok(
            "base images",
            format!(
                "{} found in {} ({:.1} GiB total)",
                images.len(),
                dir.display(),
                total_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
            ),
        )
    }
}

fn print_section(title: &str, checks: &[Check]) -> bool {
    println!("{title}");
    let mut all_ok = true;
    for check in checks {
        let (icon, detail) = match &check.status {
            Status::Ok(detail) => ("✓", detail.as_str()),
            Status::Warn(detail) => {
                all_ok = false;
                ("⚠", detail.as_str())
            }
            Status::Fail(detail) => {
                all_ok = false;
                ("✗", detail.as_str())
            }
        };
        println!("  {icon} {}: {detail}", check.name);
        if let Some(fix) = &check.fix {
            for line in fix.lines() {
                println!("      → {line}");
            }
        }
    }
    println!();
    all_ok
}

/// Runs all diagnostic checks and prints them. Returns true if everything is OK
/// (no warnings or failures), for use as the process exit status.
///
/// These checks run on the machine `andler` is invoked on. If `andlerd` runs on a
/// different machine (via --daemon-addr / ANDLERD_ADDR), run `andler doctor` there
/// too — the hypervisor/nbd checks describe andlerd's environment, not this one.
///
/// If `fix` is set, after printing, offers to write the missing passwordless-sudo
/// rules to /etc/sudoers.d/andler — see `fix_sudoers` for exactly what that does and
/// doesn't do unattended. If the fix is applied, the nbd/sudo section (the only one
/// `--fix` can affect) is re-run so the printed summary and the returned/exit status
/// reflect the machine's state *after* the fix, not before it.
pub async fn run(daemon_addr: &str, fix: bool) -> bool {
    println!("andler doctor\n");

    let hypervisor = hypervisor_checks();
    let mut nbd = nbd_checks();
    let daemon = daemon_check(daemon_addr).await;
    let base_images = base_image_check();

    let hv_ok = print_section("Hypervisor", &hypervisor);
    let mut nbd_ok = print_section("Offline guest operations (nbd)", &nbd);
    let daemon_ok = print_section("Daemon", std::slice::from_ref(&daemon));
    let base_ok = print_section("Base images", std::slice::from_ref(&base_images));

    if fix {
        let rules: Vec<PathBuf> = hypervisor
            .iter()
            .chain(nbd.iter())
            .filter(|c| c.needs_fix())
            .filter_map(|c| c.sudoers_rule.as_ref())
            .map(|r| r.binary.clone())
            .collect();

        println!();
        if fix_sudoers(&rules) {
            println!("\nRe-checking \"Offline guest operations (nbd)\" after --fix:\n");
            nbd = nbd_checks();
            nbd_ok = print_section("Offline guest operations (nbd)", &nbd);
        }
    }

    let all_ok = hv_ok && nbd_ok && daemon_ok && base_ok;

    if all_ok {
        println!("All checks passed.");
    } else {
        println!("Some checks need attention — see → lines above.");
        println!(
            "Note: these checks describe the machine `andler` is running on. If andlerd \
             runs elsewhere, run `andler doctor` there too."
        );
    }

    all_ok
}

/// Writes missing `NOPASSWD` rules to /etc/sudoers.d/andler, one line per binary that
/// failed its passwordless-sudo check above. Returns true if, afterwards, the file
/// should contain everything it needs to (nothing was missing, or the write
/// succeeded) — the caller uses this to decide whether to re-run the affected checks.
///
/// Deliberately NOT fully unattended, unlike the nbd module auto-load: this edits a
/// security-policy file, so it always (a) shows the exact lines before touching
/// anything, (b) asks for explicit y/N confirmation, (c) validates syntax with
/// `visudo -c` against a temp file before it ever touches the real sudoers.d file, and
/// (d) installs with a real (interactive) `sudo`, not `sudo -n` — this is a one-time,
/// user-confirmed action, not something that should ever run silently.
fn fix_sudoers(binaries: &[PathBuf]) -> bool {
    if binaries.is_empty() {
        println!("--fix: no passwordless-sudo issues found, nothing to do.");
        return true;
    }

    let user = std::env::var("SUDO_USER")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "youruser".to_string());

    let mut new_lines: Vec<String> = binaries
        .iter()
        .map(|bin| format!("{user} ALL=(root) NOPASSWD: {}", bin.display()))
        .collect();
    new_lines.sort();
    new_lines.dedup();

    let target = Path::new("/etc/sudoers.d/andler");
    let existing: Vec<String> = read_sudoers_file(target)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default();

    let to_add: Vec<&String> = new_lines.iter().filter(|l| !existing.contains(l)).collect();
    if to_add.is_empty() {
        println!(
            "--fix: {} already contains all the needed rules.",
            target.display()
        );
        return true;
    }

    println!(
        "--fix: the following line(s) would be added to {}:\n",
        target.display()
    );
    for line in &to_add {
        println!("  {line}");
    }
    print!("\nApply? [y/N] ");
    if std::io::Write::flush(&mut std::io::stdout()).is_err() {
        // best-effort; if the prompt doesn't flush we still try to read input below
    }

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        println!("--fix: could not read confirmation, aborting.");
        return false;
    }
    if !answer.trim().eq_ignore_ascii_case("y") {
        println!("--fix: aborted, nothing was changed.");
        return false;
    }

    let mut content = existing;
    content.extend(to_add.into_iter().cloned());
    let mut content = content.join("\n");
    content.push('\n');

    let tmp = std::env::temp_dir().join(format!("andler-sudoers-{}", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, &content) {
        println!("--fix: failed to write temp file {}: {e}", tmp.display());
        return false;
    }

    // Validate syntax against the temp file before it's anywhere near the real
    // sudoers.d directory — a malformed sudoers file can lock out sudo entirely.
    let validation = std::process::Command::new("visudo")
        .args(["-c", "-f"])
        .arg(&tmp)
        .output();

    let valid = match &validation {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            println!(
                "--fix: generated file failed `visudo -c` validation, aborting:\n{}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            false
        }
        Err(e) => {
            println!("--fix: could not run `visudo -c` to validate: {e}");
            false
        }
    };
    if !valid {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }

    // Install with correct ownership/mode. This is the one step that needs root, and
    // it prompts interactively (no -n) — the person just confirmed this exact change.
    let install = std::process::Command::new("sudo")
        .args(["install", "-m", "0440", "-o", "root", "-g", "root"])
        .arg(&tmp)
        .arg(target)
        .status();

    let _ = std::fs::remove_file(&tmp);

    match install {
        Ok(s) if s.success() => {
            println!("--fix: wrote {}", target.display());
            true
        }
        Ok(s) => {
            println!(
                "--fix: `sudo install` exited with {s}, nothing was written to {}",
                target.display()
            );
            false
        }
        Err(e) => {
            println!("--fix: could not run `sudo install`: {e}");
            false
        }
    }
}

/// Reads /etc/sudoers.d/andler's current contents. Once written, the file is
/// `0440 root:root`, so a plain (non-root) read only works before the first --fix; on
/// later runs it falls back to a non-interactive `sudo -n cat`, so `--fix` can still
/// tell what's already there and doesn't nag about lines it already added. If neither
/// works, callers treat it as "nothing there yet" — worst case that just re-prompts
/// for lines that were already applied.
fn read_sudoers_file(target: &Path) -> Option<String> {
    std::fs::read_to_string(target).ok().or_else(|| {
        if !target.exists() {
            return None;
        }
        std::process::Command::new("sudo")
            .args(["-n", "cat"])
            .arg(target)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_a_binary_known_to_exist_on_any_linux_test_runner() {
        assert!(which("sh").is_some());
    }

    #[test]
    fn which_returns_none_for_a_binary_that_does_not_exist() {
        assert!(which("andler-doctor-definitely-not-a-real-binary-xyz").is_none());
    }
}
