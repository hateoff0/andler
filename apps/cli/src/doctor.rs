use std::path::Path;

use andler_rpc::proto::Empty;

use crate::helpers::which;

enum Status {
    Ok(String),
    Warn(String),
    Fail(String),
}

struct Check {
    name: &'static str,
    status: Status,
    fix: Option<String>,
}

fn ok(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        status: Status::Ok(detail.into()),
        fix: None,
    }
}

fn warn(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
    Check {
        name,
        status: Status::Warn(detail.into()),
        fix: Some(fix.into()),
    }
}

fn fail(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
    Check {
        name,
        status: Status::Fail(detail.into()),
        fix: Some(fix.into()),
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

/// Offline guest operations are zero-root since the guestmount+userns
/// migration: the disk is mounted through guestmount (FUSE)
/// and package commands run inside an unprivileged user namespace — no
/// sudoers rules exist anymore. These checks pin the three prerequisites.
fn offline_checks() -> Vec<Check> {
    let mut checks = Vec::new();

    checks.push(match which("oras") {
        Some(p) => ok("oras", p.display().to_string()),
        None => warn(
            "oras",
            "not found in PATH — OCI export/import will fail",
            "install oras (https://oras.land/); it is the OCI registry client andler uses",
        ),
    });

    checks.push(match which("guestmount") {
        Some(p) => ok("guestmount (FUSE)", p.display().to_string()),
        None => warn(
            "guestmount (FUSE)",
            "not found in PATH — offline guest package ops (--offline) will fail",
            "install libguestfs-tools / guestfs-tools; the smart online path needs it only \
             when a VM cannot boot",
        ),
    });

    let fuse = Path::new("/dev/fuse");
    checks.push(if !fuse.exists() {
        warn(
            "/dev/fuse",
            "not present — guestmount cannot mount guest disks",
            "ensure FUSE is enabled in the kernel and /dev/fuse exists (usually automatic)",
        )
    } else {
        ok("/dev/fuse", "accessible")
    });

    let userns_ok = std::process::Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount", "--", "true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    checks.push(if userns_ok {
        ok("unprivileged user namespaces", "allowed")
    } else {
        warn(
            "unprivileged user namespaces",
            "disabled — offline guest package ops (--offline) will fail",
            "enable them: sudo sysctl kernel.unprivileged_userns_clone=1 (Debian/Ubuntu); \
             Arch-based distros allow them by default. The smart online path needs none of this.",
        )
    });

    checks
}

async fn daemon_check(addr: &str) -> Check {
    let connect = crate::traced_client(addr);
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

/// Runs all diagnostic checks and prints them. Returns true when everything
/// is OK (no warnings or failures), for use as the process exit status.
///
/// These checks run on the machine `andler` is invoked on. If `andlerd` runs
/// on a different machine (via --daemon-addr / ANDLERD_ADDR), run
/// `andler doctor` there too — the hypervisor/offline checks describe
/// andlerd's environment, not this one.
pub async fn run(daemon_addr: &str, json: bool) -> bool {
    let hypervisor = hypervisor_checks();
    let offline = offline_checks();
    let daemon = daemon_check(daemon_addr).await;
    let base_images = base_image_check();

    if json {
        let all_ok = all_sections_ok(
            &hypervisor,
            &offline,
            std::slice::from_ref(&daemon),
            std::slice::from_ref(&base_images),
        );
        match serde_json::to_string(&serde_json::json!({
            "overall": if all_ok { "ok" } else { "needs_attention" },
            "checks": flatten_checks(
                &hypervisor,
                &offline,
                std::slice::from_ref(&daemon),
                std::slice::from_ref(&base_images),
            ),
        })) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("failed to serialize doctor output: {e}"),
        }
        return all_ok;
    }

    println!("andler doctor\n");

    let hv_ok = print_section("Hypervisor", &hypervisor);
    let offline_ok = print_section("Offline guest operations (guestmount + userns)", &offline);
    let daemon_ok = print_section("Daemon", std::slice::from_ref(&daemon));
    let base_ok = print_section("Base images", std::slice::from_ref(&base_images));

    let all_ok = hv_ok && offline_ok && daemon_ok && base_ok;

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

/// Whether every check across all four doctor sections passed (no warn/fail).
fn all_sections_ok(
    hypervisor: &[Check],
    offline: &[Check],
    daemon: &[Check],
    base_images: &[Check],
) -> bool {
    hypervisor
        .iter()
        .chain(offline)
        .chain(daemon)
        .chain(base_images)
        .all(|c| matches!(c.status, Status::Ok(_)))
}

/// Flattens every check into a JSON object: name, status (ok/warn/fail),
/// detail, and fix (when present). Used by `doctor --json`.
fn flatten_checks<'a>(
    hypervisor: &'a [Check],
    offline: &'a [Check],
    daemon: &'a [Check],
    base_images: &'a [Check],
) -> Vec<serde_json::Value> {
    let mut checks = Vec::new();
    for check in hypervisor
        .iter()
        .chain(offline)
        .chain(daemon)
        .chain(base_images)
    {
        let (status, detail) = match &check.status {
            Status::Ok(detail) => ("ok", detail.as_str()),
            Status::Warn(detail) => ("warn", detail.as_str()),
            Status::Fail(detail) => ("fail", detail.as_str()),
        };
        let mut map = serde_json::Map::new();
        map.insert(
            "name".into(),
            serde_json::Value::String(check.name.to_string()),
        );
        map.insert(
            "status".into(),
            serde_json::Value::String(status.to_string()),
        );
        map.insert(
            "detail".into(),
            serde_json::Value::String(detail.to_string()),
        );
        if let Some(fix) = &check.fix {
            map.insert("fix".into(), serde_json::Value::String(fix.clone()));
        }
        checks.push(serde_json::Value::Object(map));
    }
    checks
}

/// `andler doctor --metrics` — prints the daemon's internal metrics
/// snapshot: RPC latency p50/p99 per method, error counts
/// by gRPC status code, instance/active-op counts and QMP reconnects.
pub async fn print_metrics(daemon_addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = crate::traced_client(daemon_addr).await?;
    let snap = client
        .get_daemon_metrics(Empty {})
        .await
        .map_err(|e| format!("cannot fetch daemon metrics: {e}"))?
        .into_inner();

    println!("daemon metrics ({} method(s))", snap.latency.len());
    println!(
        "instances: {} total, {} running, {} active op(s); QMP reconnects: {}",
        snap.instance_count, snap.running_count, snap.active_ops, snap.qmp_reconnects
    );
    if snap.running_ram_bytes > 0 {
        println!(
            "running guest RAM: {} across {} instance(s); the daemon refuses starts that would exceed host memory",
            andler_core::sizes::format_size(snap.running_ram_bytes),
            snap.running_count
        );
    }
    if snap.latency.is_empty() {
        println!("  (no RPC traffic recorded yet)");
    }
    for m in &snap.latency {
        println!(
            "  {:<48} count={:<6} p50={}ms p99={}ms",
            m.method, m.count, m.p50_ms, m.p99_ms
        );
    }
    if !snap.by_code.is_empty() {
        println!("errors by status code:");
        for entry in &snap.by_code {
            if entry.code != "OK" {
                println!("  {}: {}", entry.code, entry.count);
            }
        }
    }
    Ok(())
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
        assert!(which("andler-no-such-binary-xyz").is_none());
    }

    #[test]
    fn parse_cap_eff_reads_hex_mask() {
        let status = "CapInh:\t0000000000000000\nCapEff:\t0000000000001000\n";
        assert_eq!(parse_cap_eff(status), Some(0x1000));
    }

    #[test]
    fn parse_cap_eff_missing_line_is_none() {
        assert_eq!(parse_cap_eff("CapInh:\t0\n"), None);
    }

    #[test]
    fn parse_cap_eff_garbage_hex_is_none() {
        assert_eq!(parse_cap_eff("CapEff:\tzzzz\n"), None);
    }

    #[test]
    fn cap_net_admin_bit_is_detected() {
        assert!(parse_cap_eff("CapEff:\t0000000000001000\n").unwrap() & (1u64 << 12) != 0);
    }
}
