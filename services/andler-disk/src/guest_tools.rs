use std::path::Path;

use andler_core::config::InstanceKind;
use andler_core::{GuestMutator, MutatorError, MutatorOp};

use crate::error::DiskError;

pub use andler_core::package_manager::PackageManager;

/// The libguestfs appliance's QEMU user networking (libguestfs launch-direct
/// picks this link-local subnet and its gateway). A package session configures
/// `eth0` with these itself: the appliance's own boot-time DHCP is a `dhcpcd`
/// the host does not always install, and without an address every mirror
/// download fails.
const APPLIANCE_ADDRESS: &str = "169.254.2.15/16";
const APPLIANCE_GATEWAY: &str = "169.254.2.2";

/// The resolver file the appliance bind-mounts into the guest is read-only
/// and names no server, so the session writes its own into a runtime tmpfs
/// and mounts that over `/etc/resolv.conf` — the guest image keeps whatever
/// it had, and the guest's own boot writes the file back anyway.
const APPLIANCE_RUNTIME_DIR: &str = "/run/andler";
const GUEST_RESOLV_CONF: &str = "/etc/resolv.conf";
const SYSTEMD_UNIT_DIR: &str = "/lib/systemd/system";
const SYSTEMD_WANTS_DIR: &str = "/etc/systemd/system/multi-user.target.wants";

const MANAGER_PROBES: &[(&str, PackageManager)] = &[
    ("/usr/bin/apt-get", PackageManager::Apt),
    ("/usr/bin/dnf", PackageManager::Dnf),
    ("/usr/bin/yum", PackageManager::Dnf),
    ("/usr/bin/pacman", PackageManager::Pacman),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageAction {
    Install,
    Remove,
}

fn checked_package_name(package: &str) -> Result<&str, DiskError> {
    if package.is_empty() {
        return Err(DiskError::InvalidPackageName {
            package: package.to_string(),
            reason: "the name is empty".to_string(),
        });
    }
    match package
        .chars()
        .find(|character| !package_name_character(*character))
    {
        Some(character) => Err(DiskError::InvalidPackageName {
            package: package.to_string(),
            reason: format!("`{character}` is not allowed in a package name the guest shell runs"),
        }),
        None => Ok(package),
    }
}

fn package_name_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || "._+-,:=@".contains(character)
}

fn appliance_network_command() -> String {
    format!(
        "ip link set eth0 up && ip addr replace {APPLIANCE_ADDRESS} dev eth0 && \
         ip route replace default via {APPLIANCE_GATEWAY}"
    )
}

/// The resolvers a package session may use.
///
/// The appliance's own resolver is the host's copied by libguestfs with
/// loopback entries dropped, so a host that resolves through a local stub
/// (systemd-resolved on 127.0.0.53) leaves the appliance with no resolver at
/// all — the slirp DNS proxy then has nothing to forward to and every mirror
/// download fails with "Could not resolve host". systemd's own upstream file
/// is read first, because that is where the real servers live when the stub is
/// in use.
fn guest_nameservers() -> Vec<String> {
    let mut servers = Vec::new();
    for path in ["/run/systemd/resolve/resolv.conf", "/etc/resolv.conf"] {
        if let Ok(text) = std::fs::read_to_string(path) {
            for address in parse_nameservers(&text) {
                if !servers.contains(&address) {
                    servers.push(address);
                }
            }
        }
        if !servers.is_empty() {
            break;
        }
    }
    servers
}

/// The usable `nameserver` addresses in a resolver configuration.
///
/// A local stub is skipped (it names the appliance's own loopback inside the
/// session), and only addresses made of digits, hex digits, dots and colons
/// survive: the address travels into a shell command, so anything else is
/// dropped rather than quoted.
fn parse_nameservers(text: &str) -> Vec<String> {
    let mut servers: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("nameserver") else {
            continue;
        };
        if !rest.starts_with([' ', '\t']) {
            continue;
        }
        let address = rest.trim();
        if address.is_empty() || is_local_resolver(address) {
            continue;
        }
        if !address
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':')
        {
            continue;
        }
        if !servers.iter().any(|known| known == address) {
            servers.push(address.to_string());
        }
    }
    // The appliance reaches the world over IPv4 only, so an IPv4 server must be
    // tried first whatever order the host listed them in.
    servers.sort_by_key(|address| address.contains(':'));
    servers
}

fn is_local_resolver(address: &str) -> bool {
    address.starts_with("127.") || address == "::1" || address.starts_with("169.254.")
}

fn guest_resolv_command(nameservers: &[String]) -> String {
    let tmpfs = "mount -t tmpfs -o mode=0755 tmpfs /run";
    let runtime = format!("mkdir -p {APPLIANCE_RUNTIME_DIR}");
    let truncate = format!(": > {APPLIANCE_RUNTIME_DIR}/resolv.conf");
    let servers = nameservers
        .iter()
        .map(|address| format!("echo nameserver {address} >> {APPLIANCE_RUNTIME_DIR}/resolv.conf"))
        .collect::<Vec<String>>()
        .join(" ; ");
    let target = format!(
        "( test -e {GUEST_RESOLV_CONF} || ( rm -f {GUEST_RESOLV_CONF} ; \
         touch {GUEST_RESOLV_CONF} ) )"
    );
    let bind = format!("mount --bind {APPLIANCE_RUNTIME_DIR}/resolv.conf {GUEST_RESOLV_CONF}");
    format!("{tmpfs} && {runtime} && {truncate} && {servers} && {target} && {bind}")
}

/// A package-manager command that sets the resolver up itself.
///
/// Each `sh` line of a session runs in its own mount namespace — a resolver
/// mounted by an earlier step is gone by the time the manager runs, which is
/// exactly how offline installs failed with "Could not resolve host" while the
/// resolver step itself reported success. The setup therefore travels *with*
/// the command that needs it.
fn with_resolver(command: &str, nameservers: &[String]) -> String {
    format!("{} ; {command}", guest_resolv_command(nameservers))
}

fn manager_command(manager: PackageManager, args: &[&str]) -> String {
    let mut argv: Vec<&str> = vec![manager.binary_name()];
    if manager == PackageManager::Apt {
        argv.extend(["Acquire::ForceIPv4=true"]);
        argv.insert(1, "-o");
    }
    argv.extend(args.iter().copied());
    argv.join(" ")
}

fn enable_unit_command(unit: &str) -> String {
    format!(
        "mkdir -p {SYSTEMD_WANTS_DIR} && ln -sf {SYSTEMD_UNIT_DIR}/{unit} \
         {SYSTEMD_WANTS_DIR}/{unit}"
    )
}

fn systemd_unit(package: &str) -> Option<&'static str> {
    KNOWN_PACKAGES
        .iter()
        .chain(ANDROID_PACKAGES.iter())
        .find(|known| known.name == package)
        .and_then(|known| known.systemd_unit)
}

fn package_recipe(
    action: PackageAction,
    manager: PackageManager,
    package: &str,
    nameservers: &[String],
) -> Result<Vec<String>, DiskError> {
    if nameservers.is_empty() {
        return Err(DiskError::OfflineGuestFailed(
            "the guest's package manager has to reach a mirror, but the host's resolver \
             configuration names no upstream nameserver (only a local stub): add a \
             reachable `nameserver` line to /etc/resolv.conf"
                .to_string(),
        ));
    }
    let mut commands = vec![appliance_network_command()];
    match action {
        PackageAction::Install => {
            commands.push(with_resolver(
                &manager_command(manager, &manager.refresh_args()),
                nameservers,
            ));
            commands.push(with_resolver(
                &manager_command(manager, &manager.install_args(package)),
                nameservers,
            ));
            if let Some(unit) = systemd_unit(package) {
                commands.push(enable_unit_command(unit));
            }
        }
        PackageAction::Remove => {
            commands.push(with_resolver(
                &manager_command(manager, &manager.remove_args(package)),
                nameservers,
            ));
        }
    }
    Ok(commands)
}

fn appliance_failure(error: MutatorError, disk_path: &Path, what: &str) -> DiskError {
    let detail = match error {
        MutatorError::Io(detail)
        | MutatorError::NotFound(detail)
        | MutatorError::Unsupported(detail) => detail,
    };
    if detail.contains("no operating system was found") {
        return DiskError::NoGuestOs {
            path: disk_path.to_path_buf(),
        };
    }
    DiskError::OfflineGuestFailed(format!(
        "could not {what} in the guest filesystem on {}: {detail}",
        disk_path.display()
    ))
}

async fn apply_recipe(
    mutator: &dyn GuestMutator,
    disk_path: &Path,
    commands: &[String],
    what: &str,
) -> Result<(), DiskError> {
    let ops: Vec<MutatorOp> = commands
        .iter()
        .map(|command| {
            tracing::debug!(command = %command, "offline package session step");
            MutatorOp::RunShell {
                command: command.clone(),
            }
        })
        .collect();
    mutator
        .apply(&ops)
        .await
        .map_err(|error| appliance_failure(error, disk_path, what))
}

async fn detect_package_manager(
    mutator: &dyn GuestMutator,
    disk_path: &Path,
) -> Result<PackageManager, DiskError> {
    let probes: Vec<&str> = MANAGER_PROBES.iter().map(|(path, _)| *path).collect();
    let answers = mutator
        .probe_paths(&probes)
        .await
        .map_err(|error| appliance_failure(error, disk_path, "inspect the guest filesystem"))?;
    MANAGER_PROBES
        .iter()
        .zip(answers)
        .find_map(|((_, manager), found)| found.then_some(*manager))
        .ok_or_else(|| DiskError::PackageManagerNotFound {
            disk: disk_path.to_path_buf(),
        })
}

async fn is_agent_installed(
    mutator: &dyn GuestMutator,
    manager: PackageManager,
    package: &str,
) -> Result<bool, DiskError> {
    let (query_binary, query_args) = manager.check_installed_command(package);
    let command = std::iter::once(query_binary)
        .chain(query_args)
        .collect::<Vec<&str>>()
        .join(" ");
    Ok(mutator
        .apply(&[MutatorOp::RunShell { command }])
        .await
        .is_ok())
}

pub async fn install_agent_offline(
    mutator: &dyn GuestMutator,
    disk_path: &Path,
    package: &str,
) -> Result<(), DiskError> {
    let package = checked_package_name(package)?;
    let manager = detect_package_manager(mutator, disk_path).await?;
    if is_agent_installed(mutator, manager, package).await? {
        return Err(DiskError::AgentAlreadyInstalled {
            package: package.to_string(),
        });
    }

    let commands = package_recipe(
        PackageAction::Install,
        manager,
        package,
        &guest_nameservers(),
    )?;
    apply_recipe(
        mutator,
        disk_path,
        &commands,
        &format!("install `{package}`"),
    )
    .await?;
    tracing::info!(
        package = %package,
        manager = %manager.binary_name(),
        "package installed in the guest filesystem through the libguestfs appliance"
    );
    Ok(())
}

pub async fn remove_agent_offline(
    mutator: &dyn GuestMutator,
    disk_path: &Path,
    package: &str,
) -> Result<(), DiskError> {
    let package = checked_package_name(package)?;
    let manager = detect_package_manager(mutator, disk_path).await?;
    if !is_agent_installed(mutator, manager, package).await? {
        return Err(DiskError::AgentNotInstalled {
            package: package.to_string(),
        });
    }

    let commands = package_recipe(
        PackageAction::Remove,
        manager,
        package,
        &guest_nameservers(),
    )?;
    apply_recipe(
        mutator,
        disk_path,
        &commands,
        &format!("remove `{package}`"),
    )
    .await?;
    tracing::info!(
        package = %package,
        manager = %manager.binary_name(),
        "package removed from the guest filesystem through the libguestfs appliance"
    );
    Ok(())
}

pub struct GuestPackage {
    pub name: &'static str,
    pub description: &'static str,

    /// Candidate binary paths, checked in order — distributions differ in
    /// where they install the same tool (e.g. `/usr/bin/qemu-ga` on Arch,
    /// `/usr/sbin/qemu-ga` on Debian/Ubuntu).
    pub binary_checks: &'static [&'static str],

    /// systemd unit the package ships. deb-systemd-helper skips the enable
    /// symlink when no systemd is running (always the case in the offline
    /// chroot), so andler creates the multi-user.target.wants link itself
    /// after install; without it the freshly installed service would never
    /// start on the next boot.
    pub systemd_unit: Option<&'static str>,
}

/// Packages every guest can take, whatever the platform: the Android base image
/// is an Arch userland running Waydroid, so the same package manager and the
/// same clipboard/agent packages are there too. Keeping one shared list is what
/// makes `guest list` on an Android VM stop hiding `spice-vdagent` — which
/// `guest apply` installs from `input.clipboard_enabled`.
pub const SHARED_PACKAGES: &[GuestPackage] = &[
    GuestPackage {
        name: "spice-vdagent",
        description: "Shared clipboard & copy/paste between host and guest",
        binary_checks: &["/usr/bin/spice-vdagentd", "/usr/sbin/spice-vdagentd"],
        systemd_unit: Some("spice-vdagentd.service"),
    },
    GuestPackage {
        name: "qemu-guest-agent",
        description: "Host-guest communication (guest-exec, freeze/thaw)",
        binary_checks: &["/usr/bin/qemu-ga", "/usr/sbin/qemu-ga"],
        systemd_unit: Some("qemu-guest-agent.service"),
    },
    GuestPackage {
        name: "spice-webdavd",
        description: "Shared folders via SPICE webdav",
        binary_checks: &["/usr/bin/spice-webdavd"],
        systemd_unit: Some("spice-webdavd.service"),
    },
];

/// Linux guests take exactly the shared set.
pub const KNOWN_PACKAGES: &[GuestPackage] = SHARED_PACKAGES;

/// Android-only additions. These are not distribution packages: the translator
/// lands in the Waydroid overlay, so it is staged as files rather than
/// installed, and its "installed" check looks for the staged library.
pub const ANDROID_PACKAGES: &[GuestPackage] = &[
    GuestPackage {
        name: "libndk",
        description: "ARM translation (Google NDK, for AMD CPUs)",
        binary_checks: &["/var/lib/waydroid/overlay/system/lib/libndk_translation.so"],
        systemd_unit: None,
    },
    GuestPackage {
        name: "libhoudini",
        description: "ARM translation (Intel Houdini)",
        binary_checks: &["/var/lib/waydroid/overlay/system/lib/libhoudini.so"],
        systemd_unit: None,
    },
];

/// Everything `guest list` (and the offline check) reports for an instance:
/// the shared packages, then the platform's own.
pub fn packages_for(kind: &InstanceKind) -> Vec<&'static GuestPackage> {
    let shared = SHARED_PACKAGES.iter();
    match kind {
        InstanceKind::LinuxVm { .. } => shared.collect(),
        InstanceKind::AndroidVm { .. } => shared.chain(ANDROID_PACKAGES.iter()).collect(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageStatus {
    Installed,
    NotInstalled,
}

pub async fn check_packages_offline(
    mutator: &dyn GuestMutator,
    disk_path: &Path,
    kind: &InstanceKind,
) -> Result<Vec<(&'static GuestPackage, PackageStatus)>, DiskError> {
    let packages = packages_for(kind);
    let probes: Vec<&str> = packages
        .iter()
        .flat_map(|package| package.binary_checks.iter().copied())
        .collect();
    let answers = mutator
        .probe_paths(&probes)
        .await
        .map_err(|error| appliance_failure(error, disk_path, "inspect the guest filesystem"))?;

    let mut offset = 0;
    let mut statuses = Vec::with_capacity(packages.len());
    for package in packages {
        let end = offset + package.binary_checks.len();
        let installed = answers[offset..end].iter().any(|found| *found);
        statuses.push((
            package,
            if installed {
                PackageStatus::Installed
            } else {
                PackageStatus::NotInstalled
            },
        ));
        offset = end;
    }
    Ok(statuses)
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use tokio::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeAppliance {
        probe_answers: Vec<bool>,
        probe_error: Option<String>,
        query_fails: bool,
        recipe_error: Option<String>,
        asks: Mutex<Vec<String>>,
        applies: Mutex<Vec<Vec<MutatorOp>>>,
    }

    impl FakeAppliance {
        async fn applied_commands(&self) -> Vec<Vec<String>> {
            self.applies
                .lock()
                .await
                .iter()
                .map(|batch| {
                    batch
                        .iter()
                        .map(|op| match op {
                            MutatorOp::RunShell { command } => command.clone(),
                            other => {
                                panic!("the appliance path must run shell commands, got {other:?}")
                            }
                        })
                        .collect()
                })
                .collect()
        }

        async fn apply_count(&self) -> usize {
            self.applies.lock().await.len()
        }

        async fn asked_paths(&self) -> Vec<String> {
            self.asks.lock().await.clone()
        }
    }

    #[async_trait]
    impl GuestMutator for FakeAppliance {
        fn name(&self) -> &'static str {
            "fake"
        }

        async fn apply(&self, ops: &[MutatorOp]) -> Result<(), MutatorError> {
            let mut applies = self.applies.lock().await;
            let is_query = applies.is_empty();
            applies.push(ops.to_vec());
            drop(applies);
            if is_query && self.query_fails {
                return Err(MutatorError::Io(
                    "error: package 'spice-vdagent' was not found".to_string(),
                ));
            }
            match &self.recipe_error {
                Some(text) if !is_query => Err(MutatorError::Io(text.clone())),
                _ => Ok(()),
            }
        }

        async fn read_file(&self, _path: &str) -> Result<Vec<u8>, MutatorError> {
            Err(MutatorError::Unsupported("read_file".to_string()))
        }

        async fn exists(&self, _path: &str) -> Result<bool, MutatorError> {
            Err(MutatorError::Unsupported("exists".to_string()))
        }

        async fn probe_paths(&self, paths: &[&str]) -> Result<Vec<bool>, MutatorError> {
            self.asks
                .lock()
                .await
                .extend(paths.iter().map(|path| (*path).to_string()));
            match &self.probe_error {
                Some(text) => Err(MutatorError::Io(text.clone())),
                None => Ok(self.probe_answers.clone()),
            }
        }
    }

    fn android_kind() -> InstanceKind {
        InstanceKind::AndroidVm {
            android_profile: andler_core::AndroidProfile {
                android_version: andler_core::AndroidVersion::Android13,
                gapps: false,
                microg: false,
                arm_translator: andler_core::ArmTranslator::Libndk,
                boot_mode: andler_core::AndroidBootMode::Android,
                base_image_pin: None,
            },
        }
    }

    fn linux_kind() -> InstanceKind {
        InstanceKind::LinuxVm {
            iso_path: "/tmp/test.iso".into(),
            cdrom_bus: andler_core::CdromBus::Ide,
        }
    }

    fn manager_answers(manager: PackageManager) -> Vec<bool> {
        MANAGER_PROBES
            .iter()
            .map(|(_, candidate)| *candidate == manager)
            .collect()
    }

    #[test]
    fn every_probe_path_is_absolute() {
        // The appliance answers `exists` only for absolute paths: a relative one
        // fails the whole probe batch, which took `guest list` down with it.
        for package in ANDROID_PACKAGES.iter().chain(KNOWN_PACKAGES.iter()) {
            for check in package.binary_checks {
                assert!(
                    check.starts_with('/'),
                    "{} probes {check:?}, which the appliance cannot resolve",
                    package.name
                );
            }
        }
    }

    #[test]
    fn android_packages_include_the_shared_clipboard_agent() {
        // Regression: `guest list` on an Android VM listed only the ARM
        // translators, so the clipboard agent that `guest apply` installs from
        // `input.clipboard_enabled` looked like it did not exist.
        let names: Vec<&str> = packages_for(&android_kind())
            .iter()
            .map(|p| p.name)
            .collect();
        assert!(names.contains(&"spice-vdagent"), "{names:?}");
        assert!(names.contains(&"qemu-guest-agent"), "{names:?}");
        assert!(names.contains(&"libndk"), "{names:?}");
        assert!(names.contains(&"libhoudini"), "{names:?}");
    }

    #[test]
    fn linux_packages_are_the_shared_set_without_translators() {
        let names: Vec<&str> = packages_for(&linux_kind()).iter().map(|p| p.name).collect();
        assert!(names.contains(&"spice-vdagent"), "{names:?}");
        assert!(!names.contains(&"libndk"), "{names:?}");
    }

    fn resolvers() -> Vec<String> {
        vec!["192.0.2.53".to_string()]
    }

    fn steps_for(action: PackageAction, manager: PackageManager, package: &str) -> Vec<String> {
        package_recipe(action, manager, package, &resolvers()).expect("a resolver is configured")
    }

    #[test]
    fn the_resolver_configuration_keeps_only_usable_upstreams() {
        let parsed = parse_nameservers(
            "# comment\nnameserver 127.0.0.53\nnameserver 192.168.1.1\n\
             nameserver 192.168.1.1\nnameserver fd8c:d476:5cc2::1\n\
             nameserver 169.254.2.3\nnameserver $(reboot)\nnameserver\n",
        );
        assert_eq!(
            parsed,
            vec!["192.168.1.1".to_string(), "fd8c:d476:5cc2::1".to_string()],
            "a local stub, a slirp proxy, a duplicate and a shell metacharacter \
             must not reach the guest's resolver"
        );

        // The appliance's network is IPv4 only, so an IPv4 server must be tried
        // first whatever order the host listed them in.
        assert_eq!(
            parse_nameservers("nameserver fd8c:d476:5cc2::1\nnameserver 192.168.1.1\n"),
            vec!["192.168.1.1".to_string(), "fd8c:d476:5cc2::1".to_string()]
        );
    }

    #[test]
    fn a_session_without_a_usable_resolver_is_refused() {
        let error = package_recipe(
            PackageAction::Install,
            PackageManager::Pacman,
            "spice-vdagent",
            &[],
        )
        .expect_err("a session with no resolver would fail on every mirror");
        assert!(
            error.to_string().contains("/etc/resolv.conf"),
            "the refusal must name the file to fix: {error}"
        );
    }

    #[test]
    fn install_recipe_runs_every_step_in_order() {
        let steps = steps_for(
            PackageAction::Install,
            PackageManager::Pacman,
            "spice-vdagent",
        );
        assert_eq!(
            steps,
            vec![
                "ip link set eth0 up && ip addr replace 169.254.2.15/16 dev eth0 && \
                 ip route replace default via 169.254.2.2"
                    .to_string(),
                format!("{} ; pacman -Sy", resolver_step()),
                format!("{} ; pacman -S --noconfirm spice-vdagent", resolver_step()),
                "mkdir -p /etc/systemd/system/multi-user.target.wants && \
                 ln -sf /lib/systemd/system/spice-vdagentd.service \
                 /etc/systemd/system/multi-user.target.wants/spice-vdagentd.service"
                    .to_string(),
            ]
        );
    }

    fn resolver_step() -> String {
        "mount -t tmpfs -o mode=0755 tmpfs /run && mkdir -p /run/andler && \
         : > /run/andler/resolv.conf && \
         echo nameserver 192.0.2.53 >> /run/andler/resolv.conf && \
         ( test -e /etc/resolv.conf || ( rm -f /etc/resolv.conf ; touch /etc/resolv.conf ) ) && \
         mount --bind /run/andler/resolv.conf /etc/resolv.conf"
            .to_string()
    }

    #[test]
    fn the_resolver_travels_with_the_command_that_needs_it() {
        // Each `sh` line of a session runs in its own mount namespace: a
        // resolver set up by an earlier step is gone by the time the package
        // manager runs, which is how offline installs failed with "Could not
        // resolve host" while the resolver step itself succeeded.
        let steps = steps_for(PackageAction::Install, PackageManager::Apt, "htop");
        let manager_steps: Vec<&String> = steps
            .iter()
            .filter(|step| step.contains("apt-get"))
            .collect();
        assert_eq!(manager_steps.len(), 2, "{steps:?}");
        for step in manager_steps {
            assert!(
                step.contains("mount --bind /run/andler/resolv.conf /etc/resolv.conf"),
                "a manager step without its resolver would fail every mirror: {step}"
            );
        }
        assert!(
            !steps
                .iter()
                .any(|step| step.starts_with("mount -t tmpfs") && !step.contains("apt-get")),
            "a standalone resolver step would not survive into the next line: {steps:?}"
        );
    }

    #[test]
    fn install_recipe_enables_only_the_units_the_package_ships() {
        let unknown = steps_for(PackageAction::Install, PackageManager::Dnf, "htop");
        assert!(
            unknown
                .last()
                .is_some_and(|step| step.ends_with("; dnf install -y htop")),
            "a package without a unit ends on its install step: {unknown:?}"
        );

        let agent = steps_for(
            PackageAction::Install,
            PackageManager::Dnf,
            "qemu-guest-agent",
        );
        assert!(
            agent
                .last()
                .is_some_and(|step| step.ends_with("qemu-guest-agent.service")),
            "{agent:?}"
        );
    }

    #[test]
    fn install_recipe_for_apt_keeps_the_ipv4_override_and_drops_the_userns_sandbox() {
        // `APT::Sandbox::User=root` existed only because the package manager
        // ran inside an unprivileged user namespace where apt's setuid sandbox
        // is unavailable. In the appliance apt is root, so the override must
        // not travel with the steps; the appliance's network is IPv4 only, so
        // the IPv4 override must.
        let steps = steps_for(PackageAction::Install, PackageManager::Apt, "spice-vdagent");
        assert!(
            steps[1].ends_with("; apt-get -o Acquire::ForceIPv4=true update"),
            "{:?}",
            steps[1]
        );
        assert!(
            steps[2].ends_with("; apt-get -o Acquire::ForceIPv4=true install -y spice-vdagent"),
            "{:?}",
            steps[2]
        );
        assert!(
            !steps.iter().any(|step| step.contains("Sandbox")),
            "{steps:?}"
        );
    }

    #[test]
    fn remove_recipe_removes_without_refreshing_the_index() {
        let steps = steps_for(
            PackageAction::Remove,
            PackageManager::Pacman,
            "spice-vdagent",
        );
        assert!(
            steps
                .last()
                .is_some_and(|step| step.ends_with("; pacman -R --noconfirm spice-vdagent")),
            "{steps:?}"
        );
        assert!(!steps.iter().any(|step| step.contains("-Sy")), "{steps:?}");
    }

    #[test]
    fn package_names_the_guest_shell_would_interpret_are_refused() {
        for package in ["spice-vdagent; rm -rf /", "a b", "pkg$(id)", "pkg`id`", ""] {
            assert!(
                checked_package_name(package).is_err(),
                "{package:?} must be refused"
            );
        }
        for package in [
            "spice-vdagent",
            "qemu-guest-agent",
            "lib32-libfoo",
            "pkg=1.2",
            "pkg:amd64",
        ] {
            assert_eq!(checked_package_name(package).unwrap(), package);
        }
    }

    #[tokio::test]
    async fn install_runs_the_whole_recipe_in_one_appliance_batch() {
        let appliance = FakeAppliance {
            probe_answers: manager_answers(PackageManager::Pacman),
            query_fails: true,
            ..FakeAppliance::default()
        };

        install_agent_offline(&appliance, Path::new("/d/disk.qcow2"), "spice-vdagent")
            .await
            .unwrap();

        let batches = appliance.applied_commands().await;
        assert_eq!(batches.len(), 2, "{batches:?}");
        assert_eq!(batches[0], vec!["pacman -Qi spice-vdagent"]);
        let expected = steps_for(
            PackageAction::Install,
            PackageManager::Pacman,
            "spice-vdagent",
        );
        let batch = &batches[1];
        assert_eq!(
            batch.len(),
            expected.len(),
            "the appliance runs every install step in one session: {batch:?}"
        );
        // The resolver step carries the host's own upstream servers, so only its
        // shape is fixed here; `install_recipe_runs_every_step_in_order` pins
        // the exact text with a resolver it controls.
        assert!(
            batch[1].starts_with("mount -t tmpfs -o mode=0755 tmpfs /run")
                && batch[1]
                    .contains("mount --bind /run/andler/resolv.conf /etc/resolv.conf ; pacman -Sy"),
            "the session gives the guest a resolver in the same command the package manager \
             runs in, because a mount does not survive into the next line: {:?}",
            batch[1]
        );
        // The resolver prefix carries the host's own servers, so compare the
        // command each step actually runs (the text after its last separator).
        for (produced, wanted) in batch.iter().zip(&expected) {
            assert_eq!(
                produced.rsplit(" ; ").next(),
                wanted.rsplit(" ; ").next(),
                "the command after the resolver setup is the recipe's"
            );
        }
        assert_eq!(
            &appliance.asked_paths().await,
            &[
                "/usr/bin/apt-get",
                "/usr/bin/dnf",
                "/usr/bin/yum",
                "/usr/bin/pacman"
            ]
        );
    }

    #[tokio::test]
    async fn install_of_a_package_the_query_reports_leaves_the_disk_alone() {
        let appliance = FakeAppliance {
            probe_answers: manager_answers(PackageManager::Pacman),
            ..FakeAppliance::default()
        };

        let error = install_agent_offline(&appliance, Path::new("/d/disk.qcow2"), "spice-vdagent")
            .await
            .unwrap_err();

        assert!(
            matches!(error, DiskError::AgentAlreadyInstalled { ref package } if package == "spice-vdagent"),
            "{error:?}"
        );
        assert_eq!(appliance.apply_count().await, 1);
    }

    #[tokio::test]
    async fn remove_of_a_package_the_query_reports_missing_leaves_the_disk_alone() {
        let appliance = FakeAppliance {
            probe_answers: manager_answers(PackageManager::Dnf),
            query_fails: true,
            ..FakeAppliance::default()
        };

        let error = remove_agent_offline(&appliance, Path::new("/d/disk.qcow2"), "spice-vdagent")
            .await
            .unwrap_err();

        assert!(
            matches!(error, DiskError::AgentNotInstalled { .. }),
            "{error:?}"
        );
        assert_eq!(appliance.apply_count().await, 1);
    }

    #[tokio::test]
    async fn a_guest_without_a_package_manager_is_named_not_guessed() {
        let appliance = FakeAppliance {
            probe_answers: vec![false; MANAGER_PROBES.len()],
            ..FakeAppliance::default()
        };

        let error = install_agent_offline(&appliance, Path::new("/d/disk.qcow2"), "spice-vdagent")
            .await
            .unwrap_err();

        assert!(
            matches!(error, DiskError::PackageManagerNotFound { ref disk } if disk == Path::new("/d/disk.qcow2")),
            "{error:?}"
        );
        assert_eq!(appliance.apply_count().await, 0);
    }

    #[tokio::test]
    async fn a_disk_without_a_guest_os_keeps_its_own_error() {
        let appliance = FakeAppliance {
            probe_error: Some(
                "guestfish: no operating system was found on this disk\n\nIf using guestfish \
                 '-i' option, remove this option and instead use the commands 'run' followed by \
                 'list-filesystems'."
                    .to_string(),
            ),
            ..FakeAppliance::default()
        };

        let error = install_agent_offline(&appliance, Path::new("/d/disk.qcow2"), "spice-vdagent")
            .await
            .unwrap_err();

        assert!(matches!(error, DiskError::NoGuestOs { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn a_failed_install_reports_the_appliance_and_the_disk() {
        let appliance = FakeAppliance {
            probe_answers: manager_answers(PackageManager::Pacman),
            query_fails: true,
            recipe_error: Some("error: target not found: spice-vdagent".to_string()),
            ..FakeAppliance::default()
        };

        let error = install_agent_offline(&appliance, Path::new("/d/disk.qcow2"), "spice-vdagent")
            .await
            .unwrap_err();

        let text = error.to_string();
        assert!(text.contains("install `spice-vdagent`"), "{text}");
        assert!(text.contains("/d/disk.qcow2"), "{text}");
        assert!(text.contains("target not found"), "{text}");
    }

    #[tokio::test]
    async fn offline_listing_answers_every_package_from_one_probe_batch() {
        let mut answers = Vec::new();
        for package in packages_for(&linux_kind()) {
            for (index, _) in package.binary_checks.iter().enumerate() {
                answers.push(package.name == "spice-vdagent" && index == 1);
            }
        }
        let appliance = FakeAppliance {
            probe_answers: answers,
            ..FakeAppliance::default()
        };

        let statuses =
            check_packages_offline(&appliance, Path::new("/d/disk.qcow2"), &linux_kind())
                .await
                .unwrap();

        let status = |name: &str| {
            statuses
                .iter()
                .find(|(package, _)| package.name == name)
                .map(|(_, status)| *status)
        };
        assert_eq!(status("spice-vdagent"), Some(PackageStatus::Installed));
        assert_eq!(
            status("qemu-guest-agent"),
            Some(PackageStatus::NotInstalled)
        );

        let asked = appliance.asked_paths().await;
        let expected: Vec<String> = packages_for(&linux_kind())
            .iter()
            .flat_map(|package| {
                package
                    .binary_checks
                    .iter()
                    .map(|check| (*check).to_string())
            })
            .collect();
        assert_eq!(asked, expected);
    }

    #[test]
    fn known_packages_has_entries() {
        assert!(!KNOWN_PACKAGES.is_empty());
        assert!(KNOWN_PACKAGES.iter().any(|p| p.name == "spice-vdagent"));
    }
}
