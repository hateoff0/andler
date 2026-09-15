use std::path::{Path, PathBuf};

use andler_core::{GuestMutator, MutatorError, MutatorOp};
use async_trait::async_trait;

const DEFAULT_GUESTFS_TIMEOUT_SECS: u64 = 300;
const MIN_GUESTFS_TIMEOUT_SECS: u64 = 30;
const DEFAULT_GUEST_PACKAGE_TIMEOUT_SECS: u64 = 600;
const MIN_GUEST_PACKAGE_TIMEOUT_SECS: u64 = 30;

/// The wall-clock bound of one appliance session, and the variable that
/// raises it. A package session runs several package-manager steps back to
/// back (index refresh, download, install) and gets the package budget the
/// online path gives a single in-guest package step; everything else gets
/// the plain appliance bound.
#[derive(Debug, Clone, Copy)]
enum SessionBudget {
    Appliance,
    Package,
}

impl SessionBudget {
    fn timeout(self) -> std::time::Duration {
        let secs = match self {
            SessionBudget::Appliance => andler_core::timeout::env_secs(
                "ANDLERD_GUESTFS_TIMEOUT_SECS",
                DEFAULT_GUESTFS_TIMEOUT_SECS,
                MIN_GUESTFS_TIMEOUT_SECS,
            ),
            SessionBudget::Package => andler_core::timeout::env_secs(
                "ANDLERD_GUEST_PACKAGE_TIMEOUT_SECS",
                DEFAULT_GUEST_PACKAGE_TIMEOUT_SECS,
                MIN_GUEST_PACKAGE_TIMEOUT_SECS,
            ),
        };
        std::time::Duration::from_secs(secs)
    }

    fn variable(self) -> &'static str {
        match self {
            SessionBudget::Appliance => "ANDLERD_GUESTFS_TIMEOUT_SECS",
            SessionBudget::Package => "ANDLERD_GUEST_PACKAGE_TIMEOUT_SECS",
        }
    }
}

/// Offline `GuestMutator` over the libguestfs appliance (guestfish).
/// The appliance boots its own unprivileged QEMU, mounts the guest image
/// with exclusive locking (qemu image lock — the same guarantee the old
/// NbdGuard flock provided, without our own code), and needs zero root.
/// The appliance boots once per mutator and then listens: every later call is
/// a cheap `--remote` client against the same booted session, so a batch still
/// costs one session and the questions around it cost none. The session holds
/// the image lock until the mutator is dropped.
pub struct GuestfsMutator {
    disk: PathBuf,
    /// Explicit `guestfish -m` mount spec (`/dev/sda:/`), used when the
    /// disk has no inspectable OS (conformance images); `None` uses `-i`.
    mount: Option<String>,
    network: bool,
    budget: SessionBudget,
    /// The booted appliance every call is sent to, started on first use. A
    /// session costs seconds before it does anything, so one per mutator
    /// replaces one per question (a translator install asked nine).
    session: tokio::sync::Mutex<Option<ListeningSession>>,
}

/// The booted guestfish appliance: it listens for commands and every later
/// call is a `--remote` client against the same session. `guestfish --listen`
/// forks, so the wrapper process and the serving process are both ours to
/// stop — and so is the appliance's own QEMU, which the *server* started.
///
/// That last one is why the session runs in its own process group and is torn
/// down by group: killing the server alone leaves its QEMU orphaned but alive,
/// still holding the guest disk open. Every later session on that disk then
/// fails to mount it (the file is locked), which is how a single lost session
/// used to poison the disk for the rest of the daemon's life.
struct ListeningSession {
    server_pid: i32,
    /// The session's process group: the wrapper, the server it forks, and the
    /// appliance QEMU that server starts all live in it.
    group: i32,
    child: tokio::process::Child,
}

impl Drop for ListeningSession {
    fn drop(&mut self) {
        // SAFETY: killpg against a group this crate created for one appliance
        // session; the daemon itself is never a member of it.
        unsafe {
            libc::killpg(self.group, libc::SIGKILL);
            libc::kill(self.server_pid, libc::SIGKILL);
        }
        let _ = self.child.start_kill();
    }
}

impl GuestfsMutator {
    pub fn new(disk: PathBuf) -> Self {
        GuestfsMutator {
            disk,
            mount: None,
            network: false,
            budget: SessionBudget::Appliance,
            session: tokio::sync::Mutex::new(None),
        }
    }

    pub fn with_mount(disk: PathBuf, mount: String) -> Self {
        GuestfsMutator {
            disk,
            mount: Some(mount),
            network: false,
            budget: SessionBudget::Appliance,
            session: tokio::sync::Mutex::new(None),
        }
    }

    /// An appliance session for offline package work: the guest starts with
    /// no network of its own, so the appliance's QEMU user networking is
    /// enabled for the manager's mirrors, and the session runs on the
    /// package budget rather than the generic appliance one.
    pub fn for_packages(disk: PathBuf) -> Self {
        GuestfsMutator {
            disk,
            mount: None,
            network: true,
            budget: SessionBudget::Package,
            session: tokio::sync::Mutex::new(None),
        }
    }

    /// The socket of a live appliance session, booting one when there is none
    /// or the previous one exited.
    async fn live_session(&self, slot: &mut Option<ListeningSession>) -> Result<i32, MutatorError> {
        let alive = session_alive(slot);
        if !alive {
            if slot.is_some() {
                tracing::debug!("the listening appliance exited; booting a fresh one");
            }
            *slot = Some(self.start_session().await?);
        }
        match slot.as_ref() {
            Some(session) => Ok(session.server_pid),
            None => Err(MutatorError::Io(
                "the appliance session could not be started".to_string(),
            )),
        }
    }

    async fn start_session(&self) -> Result<ListeningSession, MutatorError> {
        self.reap_orphaned_listeners();
        let mut cmd = tokio::process::Command::new("guestfish");
        // Its own process group (see `ListeningSession`): the appliance QEMU
        // has to die with the session, not outlive it and lock the disk.
        cmd.process_group(0);
        cmd.args(listen_args(self.mount.as_deref(), self.network));
        cmd.args(["-a", &self.disk.display().to_string()]);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let started = std::time::Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| MutatorError::Io(format!("cannot spawn guestfish: {e}")))?;
        let wrapper_pid = child
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .ok_or_else(|| MutatorError::Io("guestfish session has no pid".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| MutatorError::Io("guestfish stdout unavailable".to_string()))?;
        let bound = self.budget.timeout();
        // The listener announces itself with `GUESTFISH_PID=<pid>; export
        // GUESTFISH_PID` — that line is the readiness signal, and the pid is
        // what every later call and the teardown address.
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        let server_pid = loop {
            match tokio::time::timeout(bound, lines.next_line()).await {
                Ok(Ok(Some(line))) => {
                    if let Some(pid) = line
                        .trim()
                        .strip_prefix("GUESTFISH_PID=")
                        .and_then(|rest| rest.split(';').next())
                        .and_then(|pid| pid.trim().parse::<i32>().ok())
                    {
                        break pid;
                    }
                }
                Ok(Ok(None)) => {
                    let detail = child
                        .wait_with_output()
                        .await
                        .map(|output| String::from_utf8_lossy(&output.stderr).trim().to_string())
                        .unwrap_or_default();
                    let mut message =
                        "the libguestfs appliance closed before it started listening".to_string();
                    if !detail.is_empty() {
                        message.push_str(": ");
                        message.push_str(&detail);
                    }
                    return Err(MutatorError::Io(message));
                }
                Ok(Err(e)) => {
                    return Err(MutatorError::Io(format!(
                        "cannot read the appliance's readiness line: {e}"
                    )))
                }
                Err(_) => {
                    let _ = child.start_kill();
                    return Err(MutatorError::Io(format!(
                        "the libguestfs appliance did not start listening within {}s — it is \
                         stuck starting (or cannot start on this host); `andler doctor` verifies \
                         guestfish and /dev/kvm, and {} raises this bound",
                        bound.as_secs(),
                        self.budget.variable()
                    )));
                }
            }
            if started.elapsed() > bound {
                let _ = child.start_kill();
                return Err(MutatorError::Io(format!(
                    "the libguestfs appliance did not start listening within {}s",
                    bound.as_secs()
                )));
            }
        };
        // Whatever else the listener writes is not this crate's business; keep
        // the pipe drained so it can never block on a full buffer.
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            server_pid,
            "guestfish appliance is listening"
        );
        Ok(ListeningSession {
            server_pid,
            group: wrapper_pid,
            child,
        })
    }

    /// Kills listeners left behind by a daemon that died: `guestfish --listen`
    /// forks, so a killed daemon can leave the serving process holding the
    /// guest image's lock, and nothing else would ever reap it.
    fn reap_orphaned_listeners(&self) {
        let dir = std::env::temp_dir().join(format!(".guestfish-{}", user_id()));
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(pid) = name
                .strip_prefix("socket-")
                .and_then(|pid| pid.parse::<i32>().ok())
            else {
                continue;
            };
            let proc_dir = PathBuf::from(format!("/proc/{pid}"));
            let cmdline = match std::fs::read_to_string(proc_dir.join("cmdline")) {
                Ok(cmdline) => cmdline,
                Err(_) => continue,
            };
            if !cmdline.contains("guestfish") {
                continue;
            }
            let orphaned = std::fs::read_to_string(proc_dir.join("stat"))
                .ok()
                .and_then(|stat| stat.rsplit(')').next().map(str::to_string))
                .and_then(|rest| rest.split_whitespace().nth(1).map(str::to_string))
                .map(|parent| parent == "1")
                .unwrap_or(false);
            if orphaned {
                // The group, not just the listener: a listener's appliance QEMU
                // outlives its parent and holds the guest disk open, so killing
                // only the pid would leave the disk locked with no server left
                // to blame.
                let group = process_group(pid).unwrap_or(pid);
                tracing::debug!(
                    pid,
                    group,
                    "killing an appliance session left by a dead daemon"
                );
                // SAFETY: killpg/kill against pids read from this user's own
                // socket directory after checking the process is a guestfish.
                unsafe {
                    libc::killpg(group, libc::SIGKILL);
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }

    /// Runs one command in the session, held under the session lock: the
    /// appliance serves one command at a time, and holding it keeps a second
    /// caller from interleaving with this one.
    ///
    /// The client retries the gap between one client finishing and the listener
    /// accepting the next: it closes its socket and reopens it per command, so a
    /// call that arrives in between gets "the server is not running" from a
    /// session that is very much alive.
    async fn run_guestfish(&self, script: &str) -> Result<Vec<u8>, MutatorError> {
        let mut slot = self.session.lock().await;
        let mut server_pid = self.live_session(&mut slot).await?;
        let mut attempt = 0;
        let mut reboots = 0;
        loop {
            attempt += 1;
            match self.run_remote(server_pid, script).await {
                Ok(output) => return Ok(output),
                Err(error) => {
                    let transient = matches!(
                        &error,
                        MutatorError::Io(message)
                            if message.contains("server is not running")
                                || message.contains("No such file")
                    );
                    if !transient || attempt >= 20 {
                        return Err(error);
                    }
                    // The listener can end with the command that failed it, and
                    // until it is reaped it still holds the guest image's lock:
                    // drop the old session (which stops it) before booting a
                    // fresh one, or the new listener would fail to mount.
                    if attempt % 4 == 0 && reboots < 3 {
                        reboots += 1;
                        tracing::debug!(reboots, "the appliance session is unreachable; rebooting");
                        if let Some(dead) = slot.take() {
                            drop(dead);
                        }
                        *slot = Some(self.start_session().await?);
                        server_pid = self.live_session(&mut slot).await?;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
    }

    async fn run_remote(&self, server_pid: i32, script: &str) -> Result<Vec<u8>, MutatorError> {
        let mut cmd = tokio::process::Command::new("guestfish");
        // The waiter's timeout is the only thing that ends this client, so it
        // must not outlive the future watching it; the appliance belongs to the
        // session, not to this call.
        cmd.kill_on_drop(true);
        cmd.arg(format!("--remote={server_pid}"));
        cmd.stdin(std::process::Stdio::piped());
        // Stdout is captured on every call, not just the probing ones: a failing
        // appliance command can report on either stream, and its output is the
        // only thing that explains what went wrong.
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let started = std::time::Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| MutatorError::Io(format!("cannot reach the appliance session: {e}")))?;

        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| MutatorError::Io("guestfish stdin unavailable".to_string()))?;
            use tokio::io::AsyncWriteExt;
            stdin
                .write_all(script.as_bytes())
                .await
                .map_err(|e| MutatorError::Io(format!("cannot write guestfish script: {e}")))?;
        }

        // Wall-clock bound: a stuck appliance used to hang the operation that
        // started it, with nothing in the log and no way for the caller to
        // tell "slow" from "never".
        let timeout = self.budget.timeout();
        let budget_var = self.budget.variable();
        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(result) => result.map_err(|e| MutatorError::Io(format!("guestfish failed: {e}")))?,
            Err(_) => {
                return Err(MutatorError::Io(format!(
                    "the libguestfs appliance did not finish within {}s — it is stuck starting \
                     (or cannot start on this host); `andler doctor` verifies guestfish and \
                     /dev/kvm, and {budget_var} raises this bound",
                    timeout.as_secs()
                )))
            }
        };

        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "guestfish call finished"
        );

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let reason = if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            };
            let msg = if reason.is_empty() {
                format!("guestfish exited with {}", output.status)
            } else {
                reason.to_string()
            };
            return Err(classify_failure(&msg));
        }
        Ok(output.stdout)
    }
}

/// A process's process-group id, from `/proc/<pid>/stat` (field 5).
fn process_group(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name sits in parentheses and may contain spaces, so the
    // fields after it are the ones to count from.
    let rest = stat.rsplit(')').next()?;
    rest.split_whitespace().nth(2)?.parse::<i32>().ok()
}

/// Which failure a dead client reported.
///
/// The two cases look alike in text and are nothing alike in cause: a command
/// that could not find a *guest* path is final, while a client that cannot
/// reach its own appliance session is transient — the session is repaired by
/// rebooting it (see `run_guestfish`), and the retry loop only retries what it
/// can tell apart. A session failure names the appliance's socket
/// (`$TMPDIR/.guestfish-<uid>/socket-<pid>`); a guest failure names the guest
/// path.
fn classify_failure(message: &str) -> MutatorError {
    if message.contains("server is not running") || message.contains(".guestfish-") {
        return MutatorError::Io(message.to_string());
    }
    if message.contains("not found") || message.contains("No such file") {
        return MutatorError::NotFound(message.to_string());
    }
    MutatorError::Io(message.to_string())
}

/// Wraps a guest path in single quotes for the guestfish script parser
/// (which is shell-like: it honors quotes), escaping embedded quotes.
fn quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

/// The guestfish arguments that boot the appliance once and leave it
/// listening. `--no-progress` is a session-level flag, so it belongs to the
/// boot rather than to each later call, and `-i`/`-m` launch the appliance
/// here, so later calls are commands rather than a second boot. Kept separate
/// from the spawn so the arguments — the appliance's network in particular —
/// are testable without an appliance.
/// Whether the session's appliance is still there. The client's own error is
/// the ground truth for reachability (see `run_guestfish`); this answers the
/// coarser question of whether there is still a process to stop.
fn session_alive(slot: &Option<ListeningSession>) -> bool {
    match slot {
        Some(session) => process_alive(session.server_pid),
        None => false,
    }
}

/// This process's user id.
fn user_id() -> u32 {
    // SAFETY: getuid takes no arguments and cannot fail.
    unsafe { libc::getuid() }
}

/// Whether the process is still there (signal 0 asks without delivering one).
fn process_alive(pid: i32) -> bool {
    // SAFETY: kill with a pid we spawned and signal 0, which only performs the
    // permission and existence checks.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn listen_args(mount: Option<&str>, network: bool) -> Vec<String> {
    let mut args = vec!["--listen".to_string(), "--no-progress".to_string()];
    if network {
        args.push("--network".to_string());
    }
    match mount {
        Some(spec) => {
            args.push("-m".to_string());
            args.push(spec.to_string());
        }
        None => args.push("-i".to_string()),
    }
    args
}

/// Builds the guestfish script for a mutation batch. `WriteFile` content
/// lands in host temp files under `content_dir` and is uploaded with
/// `upload`, keeping binary content out of the script itself.
fn build_script(ops: &[MutatorOp], content_dir: &Path) -> String {
    let mut lines = Vec::with_capacity(ops.len() + 1);
    for (index, op) in ops.iter().enumerate() {
        match op {
            MutatorOp::WriteFile { path, .. } => {
                let tmp = content_dir.join(format!("upload-{index}"));
                lines.push(format!(
                    "upload {} {}",
                    quote(&tmp.to_string_lossy()),
                    quote(path)
                ));
            }
            MutatorOp::UploadFile { path, host_path } => {
                lines.push(format!(
                    "upload {} {}",
                    quote(&host_path.to_string_lossy()),
                    quote(path)
                ));
            }
            MutatorOp::MkdirP { path } => {
                lines.push(format!("mkdir-p {}", quote(path)));
            }
            MutatorOp::CpA { src, dst } => {
                lines.push(format!("cp-a {} {}", quote(src), quote(dst)));
            }
            MutatorOp::Mv { src, dst } => {
                lines.push(format!("mv {} {}", quote(src), quote(dst)));
            }
            MutatorOp::RmRf { path } => {
                lines.push(format!("rm-rf {}", quote(path)));
            }
            MutatorOp::Chmod { path, mode } => {
                lines.push(format!("chmod 0{mode:o} {}", quote(path)));
            }
            MutatorOp::Symlink { target, link } => {
                lines.push(format!("ln-s {} {}", quote(target), quote(link)));
            }
            MutatorOp::RunShell { command } => {
                lines.push(format!("sh {}", quote(command)));
            }
        }
    }
    lines.join("\n")
}

#[async_trait]
impl GuestMutator for GuestfsMutator {
    fn name(&self) -> &'static str {
        "guestfs"
    }

    async fn apply(&self, ops: &[MutatorOp]) -> Result<(), MutatorError> {
        if ops.is_empty() {
            return Ok(());
        }
        // WriteFile content is staged into a temp dir; each op's upload
        // references its staging file by index (see build_script).
        let content_dir =
            std::env::temp_dir().join(format!("andler-guestfs-{}-{}", std::process::id(), uuid4()));
        std::fs::create_dir_all(&content_dir)
            .map_err(|e| MutatorError::Io(format!("cannot create staging dir: {e}")))?;

        let write_result = async {
            for (index, op) in ops.iter().enumerate() {
                if let MutatorOp::WriteFile { content, .. } = op {
                    let tmp = content_dir.join(format!("upload-{index}"));
                    std::fs::write(&tmp, content)
                        .map_err(|e| MutatorError::Io(format!("cannot stage upload: {e}")))?;
                }
            }
            // Every write batch ends with a flush. The session is torn down by
            // killing the appliance (see `ListeningSession`), and QEMU holds the
            // image with `cache=writeback`: a write that is still in that cache
            // when the kill lands is simply gone, while the caller was already
            // told the batch succeeded. A small write at the end of a short
            // session — enabling a unit, switching the boot target — is exactly
            // the shape that loses the race. `sync` is a guestfish command, so
            // it travels with the batch and flushes the guest filesystem into
            // the virtio write cache, which QEMU turns into an `fdatasync` of
            // the image before it answers.
            let script = format!("{}\nsync", build_script(ops, &content_dir));
            self.run_guestfish(&script).await?;
            Ok::<(), MutatorError>(())
        }
        .await;

        let _ = std::fs::remove_dir_all(&content_dir);
        write_result
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>, MutatorError> {
        let script = format!("download {} -\n", quote(path));
        self.run_guestfish(&script).await
    }

    async fn exists(&self, path: &str) -> Result<bool, MutatorError> {
        let script = format!("exists {}\n", quote(path));
        let out = self.run_guestfish(&script).await?;
        Ok(String::from_utf8_lossy(&out).trim() == "true")
    }

    async fn probe_paths(&self, paths: &[&str]) -> Result<Vec<bool>, MutatorError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut script = String::with_capacity(paths.len() * 32);
        for path in paths {
            script.push_str(&format!("exists {}\n", quote(path)));
        }
        let out = self.run_guestfish(&script).await?;
        parse_probe_answers(&out, paths.len())
    }
}

/// Turns one `exists` answer per line into one answer per path.
///
/// The caller matches answers to paths by position, so a short answer is an
/// error rather than a silently shifted result.
fn parse_probe_answers(stdout: &[u8], expected: usize) -> Result<Vec<bool>, MutatorError> {
    let text = String::from_utf8_lossy(stdout);
    let answers: Vec<bool> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line == "true")
        .collect();
    if answers.len() != expected {
        return Err(MutatorError::Io(format!(
            "the appliance answered {} of {expected} path probes",
            answers.len()
        )));
    }
    Ok(answers)
}

/// Short random hex for staging dir uniqueness (no uuid dependency).
fn uuid4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{}", std::process::id())
}

#[cfg(test)]
mod timeout_tests {
    use super::{
        listen_args, SessionBudget, DEFAULT_GUESTFS_TIMEOUT_SECS,
        DEFAULT_GUEST_PACKAGE_TIMEOUT_SECS, MIN_GUESTFS_TIMEOUT_SECS,
        MIN_GUEST_PACKAGE_TIMEOUT_SECS,
    };
    #[test]
    fn the_appliance_bound_is_configurable_and_never_silly() {
        std::env::remove_var("ANDLERD_GUESTFS_TIMEOUT_SECS");
        assert_eq!(
            SessionBudget::Appliance.timeout().as_secs(),
            DEFAULT_GUESTFS_TIMEOUT_SECS
        );

        std::env::set_var("ANDLERD_GUESTFS_TIMEOUT_SECS", "900");
        assert_eq!(SessionBudget::Appliance.timeout().as_secs(), 900);

        std::env::set_var("ANDLERD_GUESTFS_TIMEOUT_SECS", "1");
        assert_eq!(
            SessionBudget::Appliance.timeout().as_secs(),
            MIN_GUESTFS_TIMEOUT_SECS
        );

        std::env::set_var("ANDLERD_GUESTFS_TIMEOUT_SECS", "whenever");
        assert_eq!(
            SessionBudget::Appliance.timeout().as_secs(),
            DEFAULT_GUESTFS_TIMEOUT_SECS
        );
        std::env::remove_var("ANDLERD_GUESTFS_TIMEOUT_SECS");
    }

    #[test]
    fn package_sessions_get_the_package_budget() {
        // A package session runs an index refresh and a download on a guest
        // that has never synced; the generic appliance bound is too tight for
        // it, and it is a different variable, so it is a different budget.
        std::env::remove_var("ANDLERD_GUEST_PACKAGE_TIMEOUT_SECS");
        assert_eq!(
            SessionBudget::Package.timeout().as_secs(),
            DEFAULT_GUEST_PACKAGE_TIMEOUT_SECS
        );
        assert_eq!(
            SessionBudget::Package.variable(),
            "ANDLERD_GUEST_PACKAGE_TIMEOUT_SECS"
        );

        std::env::set_var("ANDLERD_GUEST_PACKAGE_TIMEOUT_SECS", "1200");
        assert_eq!(SessionBudget::Package.timeout().as_secs(), 1200);

        std::env::set_var("ANDLERD_GUEST_PACKAGE_TIMEOUT_SECS", "1");
        assert_eq!(
            SessionBudget::Package.timeout().as_secs(),
            MIN_GUEST_PACKAGE_TIMEOUT_SECS
        );
        std::env::remove_var("ANDLERD_GUEST_PACKAGE_TIMEOUT_SECS");
    }

    #[test]
    fn the_appliance_boots_once_and_listens() {
        // The boot is the cost: a session takes seconds before it does any
        // work, so every call after the first is a client of this one. `-i`
        // and `-m` launch the appliance at boot, so the later calls are plain
        // commands rather than another launch.
        let packages = listen_args(None, true);
        assert_eq!(
            packages,
            vec!["--listen", "--no-progress", "--network", "-i"],
            "{packages:?}"
        );

        let plain = listen_args(None, false);
        assert_eq!(plain, vec!["--listen", "--no-progress", "-i"], "{plain:?}");

        assert_eq!(
            listen_args(Some("/dev/sda:/"), false),
            vec!["--listen", "--no-progress", "-m", "/dev/sda:/"]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_group_is_readable_from_proc() {
        let own = process_group(std::process::id() as i32).expect("own /proc entry");
        assert!(own > 0, "the test process has a process group");
        assert_eq!(process_group(i32::MAX), None, "a missing pid has no group");
    }

    #[test]
    fn a_dead_session_is_not_confused_with_a_missing_guest_path() {
        // The retry loop reboots the session on the first and refuses on the
        // second, so the two must not classify alike — and the dead-session
        // text also contains "No such file", which is what made an install
        // fail after two seconds instead of being retried.
        let dead = "/tmp/.guestfish-1000/socket-2447725: No such file or directory\n                    guestfish: remote: looks like the server is not running";
        assert!(
            matches!(classify_failure(dead), MutatorError::Io(_)),
            "a session failure must stay retryable"
        );
        assert!(matches!(
            classify_failure("libguestfs: error: stat: /etc/nope: No such file or directory"),
            MutatorError::NotFound(_)
        ));
        assert!(matches!(
            classify_failure("chroot: failed to run command 'nope': No such file or directory"),
            MutatorError::NotFound(_)
        ));
    }

    #[test]
    fn probe_answers_must_match_the_paths_asked() {
        assert_eq!(
            parse_probe_answers(b"true\nfalse\n", 2).unwrap(),
            vec![true, false]
        );
        assert_eq!(parse_probe_answers(b"false\n", 1).unwrap(), vec![false]);
        // One answer for two paths would shift every later path onto the wrong
        // result, so it is refused.
        assert!(parse_probe_answers(b"true\n", 2).is_err());
    }

    #[test]
    fn script_maps_every_op_to_a_guestfish_command() {
        let ops = vec![
            MutatorOp::WriteFile {
                path: "/etc/foo.conf".to_string(),
                content: b"x".to_vec(),
            },
            MutatorOp::MkdirP {
                path: "/var/lib/foo".to_string(),
            },
            MutatorOp::CpA {
                src: "/a".to_string(),
                dst: "/b".to_string(),
            },
            MutatorOp::Mv {
                src: "/b".to_string(),
                dst: "/c".to_string(),
            },
            MutatorOp::RmRf {
                path: "/old".to_string(),
            },
            MutatorOp::Chmod {
                path: "/etc/foo.conf".to_string(),
                mode: 0o640,
            },
            MutatorOp::Symlink {
                target: "foo.conf".to_string(),
                link: "/etc/foo-link".to_string(),
            },
        ];
        let script = build_script(&ops, Path::new("/tmp/stage"));
        let lines: Vec<&str> = script.lines().collect();
        assert_eq!(lines.len(), ops.len());
        assert_eq!(lines[0], "upload '/tmp/stage/upload-0' '/etc/foo.conf'");
        assert_eq!(lines[1], "mkdir-p '/var/lib/foo'");
        assert_eq!(lines[2], "cp-a '/a' '/b'");
        assert_eq!(lines[3], "mv '/b' '/c'");
        assert_eq!(lines[4], "rm-rf '/old'");
        assert_eq!(lines[5], "chmod 0640 '/etc/foo.conf'");
        assert_eq!(lines[6], "ln-s 'foo.conf' '/etc/foo-link'");
    }

    #[test]
    fn script_quotes_paths_with_quotes_and_spaces() {
        let ops = vec![MutatorOp::MkdirP {
            path: "/tmp/a b'c".to_string(),
        }];
        let script = build_script(&ops, Path::new("/tmp/stage"));
        assert_eq!(script, "mkdir-p '/tmp/a b'\\''c'");
    }

    #[test]
    fn empty_batch_produces_empty_script() {
        assert_eq!(build_script(&[], Path::new("/tmp/stage")), "");
    }
}

#[cfg(test)]
mod conformance_tests {
    use andler_core::guest_mutator::conformance::run_conformance;
    use andler_core::GuestMutator;

    use super::GuestfsMutator;

    #[tokio::test]
    #[ignore = "requires guestfish + qemu-img and a mountable guest image, see docker/e2e/README.md"]
    async fn guestfs_conformance_matches_qga() {
        let disk = std::path::PathBuf::from("/tmp/andler-guestfs-conformance.qcow2");
        let status = tokio::process::Command::new("qemu-img")
            .args(["create", "-f", "raw"])
            .arg(&disk)
            .arg("512M")
            .status()
            .await
            .expect("spawn qemu-img");
        assert!(status.success(), "qemu-img create failed");
        let mkfs = tokio::process::Command::new("mke2fs")
            .args(["-q", "-F", "-t", "ext4"])
            .arg(&disk)
            .status()
            .await
            .expect("spawn mke2fs");
        assert!(mkfs.success(), "mke2fs failed");
        let mutator = GuestfsMutator::with_mount(disk.clone(), "/dev/sda:/".to_string());
        assert_eq!(mutator.name(), "guestfs");
        run_conformance(&mutator).await;
        let _ = std::fs::remove_file(&disk);
    }
}
