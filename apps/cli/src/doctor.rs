use std::path::{Path, PathBuf};

use andler_rpc::proto::andler_service_client::AndlerServiceClient;

use crate::helpers::which;

enum Status {
    Ok(String),
    Warn(String),
    Fail(String),
}

/// The single privileged entry point: a root-owned helper binary that performs
/// every operation that used to need its own `NOPASSWD` rule. One sudoers rule
/// (`youruser ALL=(root) NOPASSWD: /usr/local/sbin/andler-helper`) authorizes
/// it; everything else stays off the policy surface.
const HELPER_PATH: &str = "/usr/local/sbin/andler-helper";

/// Legacy per-binary rules that an earlier `--fix` (or the old README) may have
/// written. `--fix` migrates them: lines whose command list consists only of
/// these binaries are replaced by the single helper rule; lines containing any
/// other command are left untouched.
const LEGACY_SUDOERS_BINS: [&str; 10] = [
    "modprobe", "qemu-nbd", "mount", "umount", "chroot", "mkdir", "cp", "mv", "rm", "chmod",
];

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
        "add via `sudo visudo`: youruser ALL=(root) NOPASSWD: {} — only needed for offline \
         guest package ops when a VM cannot boot (qemu-nbd/chroot); the smart online path \
         needs no root on the host",
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
                warn(name, "passwordless sudo not configured", fix)
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

/// What, if anything, is wrong with the installed andler-helper binary. A
/// valid helper is a regular root-owned file with mode 0755 — anything else
/// (or nothing at all) means `sudo` would be authorizing a wrong binary.
fn helper_issue() -> Option<String> {
    let p = Path::new(HELPER_PATH);
    if !p.exists() {
        return Some("not installed".to_string());
    }
    let meta = std::fs::metadata(p).ok()?;
    if !meta.is_file() {
        return Some("not a regular file".to_string());
    }
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if meta.uid() != 0 {
        return Some("not owned by root".to_string());
    }
    if meta.permissions().mode() & 0o777 != 0o755 {
        return Some("not mode 0755".to_string());
    }
    None
}

/// Single `sudo -n /usr/local/sbin/andler-helper --version` probe — the one
/// command andlerd will run through sudo, so one check covers the whole
/// privileged surface that used to need ten per-binary probes.
fn check_passwordless_helper() -> Check {
    let check = check_passwordless_sudo(
        "passwordless sudo for andler-helper",
        Path::new(HELPER_PATH),
    );
    check.with_sudoers_rule(Path::new(HELPER_PATH))
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

    checks.push(cap_net_admin_check());

    checks
}

/// CAP_NET_ADMIN (bit 12) is required for bridge mode: `ip link add ... type
/// tap` / `master <bridge>` run directly in andlerd, with no sudo hop (see
/// services/andler-net). Users without it can still use nat/isolated modes.
fn parse_cap_eff(status: &str) -> Option<u64> {
    status
        .lines()
        .find(|line| line.starts_with("CapEff:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
}

fn cap_net_admin_check() -> Check {
    let caps = std::fs::read_to_string("/proc/self/status")
        .ok()
        .as_deref()
        .and_then(parse_cap_eff);
    match caps {
        Some(eff) if eff & (1u64 << 12) != 0 => {
            ok("CAP_NET_ADMIN", "present — bridge networking available")
        }
        _ => warn(
            "CAP_NET_ADMIN",
            "absent — bridge networking (network mode = \"Bridge\") will fail",
            "grant the capability or run andlerd under a user that has it: \
             sudo setcap cap_net_admin+ep $(which andlerd), or use mode = \"Nat\"",
        ),
    }
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

    // modprobe's passwordless-sudo rule is what makes the "andlerd will try to
    // load it automatically" note above actually true — without it,
    // try_autoload_nbd_module() fails silently (see nbd.rs) and the user only
    // finds out when something using nbd breaks later. All of it (modprobe +
    // qemu-nbd + mount/umount + chroot + guest-write) is now the
    // single andler-helper binary, so one presence check plus one sudo probe
    // covers the whole surface.
    match helper_issue() {
        None => checks.push(ok("andler-helper", HELPER_PATH)),
        Some(issue) => checks.push(
            warn(
                "andler-helper",
                format!("{HELPER_PATH}: {issue}"),
                "run `andler doctor --fix` to install it (will require sudo) — only needed \
                 for offline guest package ops when a VM cannot boot; the smart online path \
                 needs no root on the host",
            )
            .with_sudoers_rule(Path::new(HELPER_PATH)),
        ),
    }
    if helper_issue().is_none() {
        checks.push(check_passwordless_helper());
    }

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
/// If `fix` is set, after printing, offers to install the andler-helper binary
/// (if missing) and write the single passwordless-sudo rule to
/// /etc/sudoers.d/andler, migrating any legacy per-binary rules away — see
/// `fix_sudoers` for exactly what that does and doesn't do unattended. If the
/// fix is applied, the nbd/sudo section (the only one `--fix` can affect) is
/// re-run so the printed summary and the returned/exit status reflect the
/// machine's state *after* the fix, not before it.
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

/// True when a sudoers line is a legacy per-binary andler rule (possibly a
/// comma-separated command list): `user ALL=(root) NOPASSWD:` followed only by
/// paths under /usr/bin|/usr/sbin|/bin|/sbin whose basename is one of the ten
/// legacy binaries. Any other command in the line makes it foreign — such a
/// line is preserved as-is rather than partially rewritten.
fn is_legacy_andler_rule(line: &str, user: &str) -> bool {
    let Some(rest) = line.strip_prefix(&format!("{user} ALL=(root) NOPASSWD: ")) else {
        return false;
    };
    if rest.trim().is_empty() {
        return false;
    }
    for part in rest.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let cmd = part.split_whitespace().next().unwrap_or("");
        let path = Path::new(cmd);
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        let in_legacy = LEGACY_SUDOERS_BINS.contains(&name);
        let in_standard_dir = matches!(
            path.parent().and_then(|p| p.to_str()),
            Some("/usr/bin" | "/usr/sbin" | "/bin" | "/sbin")
        );
        if !in_legacy || !in_standard_dir {
            return false;
        }
    }
    true
}

/// Builds the sudoers content `--fix` should write: the existing lines minus
/// any legacy per-binary andler rules, plus the single helper rule. Returns
/// the content and whether it differs from what is currently there. Foreign
/// lines (rules andler never wrote) are preserved verbatim.
fn build_sudoers_content(existing: &[String], user: &str) -> (String, bool) {
    let helper_line = format!("{user} ALL=(root) NOPASSWD: {HELPER_PATH}");
    let mut changed = false;
    let mut has_helper = false;
    let mut kept: Vec<String> = Vec::new();
    for line in existing {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_legacy_andler_rule(trimmed, user) {
            changed = true;
            continue;
        }
        if trimmed == helper_line {
            has_helper = true;
            continue;
        }
        kept.push(trimmed.to_string());
    }
    if !has_helper {
        changed = true;
    }
    kept.push(helper_line);
    kept.sort();
    let mut content = kept.join("\n");
    if !content.is_empty() {
        content.push('\n');
    }
    (content, changed)
}

/// Writes the andler privileged setup to /etc/sudoers.d/andler: installs the
/// andler-helper binary if it is missing or wrong, then replaces any legacy
/// per-binary `NOPASSWD` rules with the single helper rule. Returns true if,
/// afterwards, the file should contain everything it needs to — the caller
/// uses this to decide whether to re-run the affected checks.
///
/// Deliberately NOT fully unattended, unlike the nbd module auto-load: this
/// edits a security-policy file, so it always (a) shows the exact lines before
/// touching anything, (b) asks for explicit y/N confirmation, (c) validates
/// syntax with `visudo -c` against a temp file before it ever touches the real
/// sudoers.d file, and (d) installs with a real (interactive) `sudo`, not
/// `sudo -n` — this is a one-time, user-confirmed action, not something that
/// should ever run silently.
fn fix_sudoers(binaries: &[PathBuf]) -> bool {
    if binaries.is_empty() {
        println!("--fix: no passwordless-sudo issues found, nothing to do.");
        return true;
    }

    let user = std::env::var("SUDO_USER")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "youruser".to_string());

    // Step 1: install/repair the helper binary itself. The sudoers rule is
    // only as good as what it points at — a missing or user-owned helper must
    // be fixed before the policy line is written.
    if let Some(issue) = helper_issue() {
        println!("--fix: andler-helper is missing or broken ({issue}).");
        let source = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("andler-helper")));
        match source.as_deref().filter(|s| s.is_file()) {
            Some(src) => {
                println!(
                    "--fix: installing {} from {} (needs sudo — you may be \
                     prompted for a password)",
                    HELPER_PATH,
                    src.display()
                );
                let install = std::process::Command::new("sudo")
                    .args(["install", "-o", "root", "-g", "root", "-m", "0755"])
                    .arg(src)
                    .arg(HELPER_PATH)
                    .status();
                match install {
                    Ok(s) if s.success() => {
                        println!("--fix: installed {HELPER_PATH}");
                    }
                    Ok(s) => {
                        println!(
                            "--fix: `sudo install` exited with {s}; andler-helper \
                             was not installed"
                        );
                        return false;
                    }
                    Err(e) => {
                        println!("--fix: could not run `sudo install`: {e}");
                        return false;
                    }
                }
            }
            None => {
                println!(
                    "--fix: cannot find an andler-helper binary next to {}; install \
                     it manually as root:root 0755 at {HELPER_PATH} and re-run",
                    std::env::current_exe()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| "the andler binary".to_string())
                );
                return false;
            }
        }
    }

    // Step 2: read what is there (or treat absence as empty), then build the
    // migrated content.
    let target = Path::new("/etc/sudoers.d/andler");
    let existing: Vec<String> = match read_sudoers_file(target) {
        Some(content) => content.lines().map(str::to_string).collect(),
        None if !target.exists() => Vec::new(),
        None => {
            // Overwriting an unreadable file would silently drop the rules it
            // already contains (the file is 0440 root:root after the first
            // --fix; the chroot fallback read also needs its sudoers rule).
            println!(
                "--fix: {} exists but could not be read (needs root); nothing was \
                 changed. Extend it manually: sudo visudo -f {}",
                target.display(),
                target.display()
            );
            return false;
        }
    };

    let (content, changed) = build_sudoers_content(&existing, &user);
    if !changed {
        println!(
            "--fix: {} already contains the needed rule.",
            target.display()
        );
        return true;
    }

    println!("--fix: {} would become:\n", target.display());
    for line in content.lines() {
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
/// `0440 root:root`, so a plain (non-root) read only works before the first
/// --fix; on later runs it falls back to `sudo -n andler-helper sudoers-print`
/// (the helper's own read-back subcommand — no `cat` rule needed), and from
/// there to the legacy `sudo -n chroot / cat <file>` so --fix still works on
/// machines that kept their old per-binary rules. If neither works, callers
/// must treat the file as "readable" only when it doesn't exist: overwriting
/// an unreadable file would silently drop rules that were already there.
fn read_sudoers_file(target: &Path) -> Option<String> {
    std::fs::read_to_string(target).ok().or_else(|| {
        if !target.exists() {
            return None;
        }
        if Path::new(HELPER_PATH).is_file() {
            if let Ok(o) = std::process::Command::new("sudo")
                .args(["-n", HELPER_PATH, "sudoers-print"])
                .output()
            {
                if o.status.success() {
                    return Some(String::from_utf8_lossy(&o.stdout).into_owned());
                }
            }
        }
        std::process::Command::new("sudo")
            .args(["-n", "chroot", "/", "cat"])
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

    #[test]
    fn legacy_sudoers_rules_are_replaced_by_the_helper_line() {
        let existing = vec![
            "user ALL=(root) NOPASSWD: /usr/bin/mount".to_string(),
            "user ALL=(root) NOPASSWD: /usr/bin/qemu-nbd".to_string(),
            "user ALL=(root) NOPASSWD: /usr/bin/chmod".to_string(),
        ];
        let (content, changed) = build_sudoers_content(&existing, "user");
        assert!(changed, "legacy rules must trigger a rewrite");
        assert_eq!(
            content,
            format!("user ALL=(root) NOPASSWD: {HELPER_PATH}\n")
        );
    }

    #[test]
    fn migrated_file_is_idempotent_on_a_second_fix() {
        let existing = vec![format!("user ALL=(root) NOPASSWD: {HELPER_PATH}")];
        let (content, changed) = build_sudoers_content(&existing, "user");
        assert!(!changed);
        assert_eq!(
            content,
            format!("user ALL=(root) NOPASSWD: {HELPER_PATH}\n")
        );
    }

    #[test]
    fn foreign_sudoers_lines_are_preserved_verbatim() {
        let existing = vec![
            "user ALL=(root) NOPASSWD: /usr/bin/systemctl".to_string(),
            // Mixed line: the mount rule is legacy but ssh is foreign — the
            // whole line is nobody's to rewrite, so it is kept exactly as-is.
            "user ALL=(root) NOPASSWD: /usr/bin/mount, /usr/bin/ssh".to_string(),
        ];
        let (content, changed) = build_sudoers_content(&existing, "user");
        assert!(changed, "the helper rule must still be added");
        let lines: Vec<&str> = content.lines().collect();
        assert!(lines.contains(&"user ALL=(root) NOPASSWD: /usr/bin/systemctl"));
        assert!(lines.contains(&"user ALL=(root) NOPASSWD: /usr/bin/mount, /usr/bin/ssh"));
        assert!(lines.contains(&format!("user ALL=(root) NOPASSWD: {HELPER_PATH}").as_str()));
    }

    #[test]
    fn comma_separated_legacy_lines_are_migrated() {
        let existing = vec![
            "user ALL=(root) NOPASSWD: /usr/bin/modprobe nbd max_part=8, \
             /usr/bin/mount, /usr/bin/umount, /usr/bin/qemu-nbd, /usr/bin/chroot, \
             /usr/bin/mkdir, /usr/bin/cp, /usr/bin/mv, /usr/bin/rm, /usr/bin/chmod"
                .to_string(),
        ];
        let (content, changed) = build_sudoers_content(&existing, "user");
        assert!(changed);
        assert_eq!(
            content,
            format!("user ALL=(root) NOPASSWD: {HELPER_PATH}\n")
        );
    }

    #[test]
    fn other_users_lines_are_never_touched() {
        let existing = vec!["alice ALL=(root) NOPASSWD: /usr/bin/mkdir".to_string()];
        let (content, changed) = build_sudoers_content(&existing, "bob");
        assert!(changed);
        assert!(content.contains(&"alice ALL=(root) NOPASSWD: /usr/bin/mkdir"));
        assert!(content.contains(&format!("bob ALL=(root) NOPASSWD: {HELPER_PATH}").as_str()));
    }

    #[cfg(test)]
    mod cap_tests {
        use super::parse_cap_eff;

        #[test]
        fn parse_cap_eff_reads_hex_mask() {
            assert_eq!(
                parse_cap_eff("Name:	foo\nCapEff:\t000001ffffffffff\nCapBnd:\t000001ffffffffff\n"),
                Some(0x000001ffffffffff)
            );
        }

        #[test]
        fn parse_cap_eff_missing_line_is_none() {
            assert_eq!(parse_cap_eff("Name:\tfoo\n"), None);
        }

        #[test]
        fn parse_cap_eff_garbage_hex_is_none() {
            assert_eq!(parse_cap_eff("CapEff:\tzebra\n"), None);
        }

        #[test]
        fn cap_net_admin_bit_is_detected() {
            // bit 12 = 0x1000
            let status = format!("CapEff:\t{:x}\n", 1u64 << 12);
            let caps = parse_cap_eff(&status).unwrap();
            assert_ne!(caps & (1u64 << 12), 0);
        }
    }
}
