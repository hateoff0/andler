use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::str::FromStr;

use crate::TracedClient;
use andler_core::InstanceId;
use andler_rpc::proto::{
    instance_kind, AndroidBootMode, Empty, GetInstanceConfigResponse, GuestReadinessLevel,
    InstanceIdRequest, InstanceStateKind, InstanceStatusResponse,
};
use serde::Serialize;

use crate::helpers::emit_json;
use crate::status::readiness_level_name;
use crate::ConnectLevel;

#[derive(Serialize)]
struct ConnectReport {
    instance_id: String,
    level: &'static str,
    /// Host port to reach the guest on, for `ssh`/`adb`. Absent for
    /// `console`, which has no port — it's a local unix socket.
    #[serde(skip_serializing_if = "Option::is_none")]
    host_port: Option<u16>,
    /// Guest readiness level the choice was made from, `null` while the run
    /// reports none (not started, or the guest could not be probed).
    readiness: Option<&'static str>,
    /// Strongest level this instance's effective `(kind, boot_mode)` profile
    /// can reach at all.
    terminal_readiness: Option<&'static str>,
    /// Why the resolved level is not the strongest one the profile supports.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

/// `andler connect` — single entry point to the guest. The best available
/// level is chosen for the instance's effective profile; `--level` forces
/// one. Phase 2 ships `console` (works on any VM, any state that runs QEMU)
/// and `exec` (needs the guest agent); ssh/adb land with provisioning.
///
/// `--json` resolves and reports the level/port that would be used and
/// stops there — it never launches the interactive session (console pumps
/// raw terminal bytes, ssh/adb hand this process's stdio to a child — there
/// is no "result" to wrap in JSON for either, only a decision to report).
pub async fn handle_connect(
    client: &mut TracedClient,
    instance_id: String,
    level: ConnectLevel,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let full_id = resolve_full_id(client, &instance_id).await?;

    let status = client
        .get_instance_status(InstanceIdRequest {
            instance_id: full_id.clone(),
        })
        .await?
        .into_inner();
    if status.state != InstanceStateKind::Running as i32 {
        return Err(format!(
            "instance {full_id} is not running (state {}); start it first with `andler start`",
            status.state
        )
        .into());
    }

    let (resolved, note) = match level {
        ConnectLevel::Console => (ResolvedLevel::Console, None),
        ConnectLevel::Ssh => (
            ResolvedLevel::PortForward {
                guest_port: 22,
                what: "ssh",
            },
            None,
        ),
        ConnectLevel::Adb => (
            ResolvedLevel::PortForward {
                guest_port: 5555,
                what: "adb",
            },
            None,
        ),
        ConnectLevel::Auto => resolve_auto(client, &full_id, &status).await?,
    };

    if json {
        let (level_name, host_port) = match resolved {
            ResolvedLevel::Console => ("console", None),
            ResolvedLevel::PortForward { guest_port, what } => (
                what,
                Some(resolve_host_port(client, &full_id, guest_port).await?),
            ),
        };
        emit_json(&ConnectReport {
            instance_id: full_id,
            level: level_name,
            host_port,
            readiness: readiness_level_name(status.readiness()),
            terminal_readiness: readiness_level_name(status.terminal_readiness()),
            note,
        })?;
        return Ok(());
    }

    if let Some(note) = note {
        eprintln!("{note}");
    }

    match resolved {
        ResolvedLevel::Console => connect_console(&full_id).await,
        ResolvedLevel::PortForward { guest_port, what } => {
            connect_via_port_forward(client, &full_id, guest_port, what).await
        }
    }
}

/// The `--level auto` decision, made from the guest readiness ladder instead
/// of from an assumption about the guest:
///
/// - a Linux VM keeps the serial console, which is its documented level;
/// - an Android VM in Linux boot mode is reached over ssh, and one in
///   android boot mode over adb — but only once the instance's readiness has
///   reached that profile's terminal level (`GuestOsUp` respectively
///   `WaydroidReady`), so `ssh`/`adb` are never handed a guest that has not
///   reported itself ready yet.
///
/// Until then, and whenever the daemon cannot observe the level at all (no
/// level reached, or a profile that never reports one), the serial console is
/// used — it needs nothing from the guest — with a note naming the level that
/// is still missing. Nothing here waits: an unreached or uncountable level
/// degrades the choice, it never blocks the connection.
async fn resolve_auto(
    client: &mut TracedClient,
    full_id: &str,
    status: &InstanceStatusResponse,
) -> Result<(ResolvedLevel, Option<String>), Box<dyn std::error::Error>> {
    let config: GetInstanceConfigResponse = client
        .get_instance_config(InstanceIdRequest {
            instance_id: full_id.to_string(),
        })
        .await?
        .into_inner();
    let is_android = matches!(
        config.kind.as_ref().and_then(|kind| kind.kind.as_ref()),
        Some(instance_kind::Kind::AndroidVm(_))
    );
    if !is_android {
        return Ok((ResolvedLevel::Console, None));
    }
    let (guest_port, what) = if config.boot_mode == AndroidBootMode::Linux as i32 {
        (22, "ssh")
    } else {
        (5555, "adb")
    };

    let reached = readiness_level_name(status.readiness()).unwrap_or("none");
    let terminal = readiness_level_name(status.terminal_readiness()).unwrap_or("none");
    let at_terminal = status.readiness() == status.terminal_readiness()
        && status.terminal_readiness() != GuestReadinessLevel::Unspecified;
    if !at_terminal {
        return Ok((
            ResolvedLevel::Console,
            Some(format!(
                "guest readiness is {reached} of {terminal}: attaching the serial console, \
                 because {what} needs the guest to report {terminal} first"
            )),
        ));
    }
    match host_port_for(client, full_id, guest_port).await? {
        Some(_) => Ok((ResolvedLevel::PortForward { guest_port, what }, None)),
        None => Ok((
            ResolvedLevel::Console,
            Some(format!(
                "readiness is {terminal} ({what} needs port {guest_port}), but no forward to \
                 that port is configured: add `tcp:<host_port>->{guest_port}` to \
                 network.port_forwards, or keep using the serial console — on this Android VM \
                 that console is the host Linux side it boots, not the Android UI, which runs \
                 on the display"
            )),
        )),
    }
}

enum ResolvedLevel {
    Console,
    PortForward { guest_port: u16, what: &'static str },
}

/// Looks up the host port forwarded to `guest_port`, without launching
/// anything. Shared by the interactive path and `--json`.
async fn resolve_host_port(
    client: &mut TracedClient,
    full_id: &str,
    guest_port: u16,
) -> Result<u16, Box<dyn std::error::Error>> {
    host_port_for(client, full_id, guest_port)
        .await?
        .ok_or_else(|| {
            format!(
                "no port forward to guest port {guest_port} on {full_id}; add \
                 `network.port_forwards = [\"tcp:2222->{guest_port}\"]` (host:guest) to \
                 instance.toml before starting, then `andler connect --level ssh`"
            )
            .into()
        })
}

/// The configured host port for `guest_port`, or `None` when the instance
/// has no forward for it. Absence is an answer here, not an error: the auto
/// level degrades to the console instead of failing.
async fn host_port_for(
    client: &mut TracedClient,
    full_id: &str,
    guest_port: u16,
) -> Result<Option<u16>, Box<dyn std::error::Error>> {
    let config = client
        .get_instance_config(InstanceIdRequest {
            instance_id: full_id.to_string(),
        })
        .await?
        .into_inner()
        .network
        .ok_or_else(|| "daemon returned no network config".to_string())?;

    Ok(config
        .port_forwards
        .iter()
        .find(|f| f.guest_port == guest_port as u32)
        .map(|f| f.host_port as u16))
}

/// Spawns an external client (ssh / adb) pointed at the guest's forwarded
/// port. The port forwarding must have been configured at create time via
/// `network.port_forwards`; the daemon's QEMU `-netdev hostfwd=` does the
/// actual plumbing. The client inherits this process's stdio so it stays
/// interactive.
async fn connect_via_port_forward(
    client: &mut TracedClient,
    full_id: &str,
    guest_port: u16,
    what: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let host_port = resolve_host_port(client, full_id, guest_port).await?;

    let (cmd, args): (&str, Vec<String>) = match what {
        "ssh" => (
            "ssh",
            vec![
                "-p".to_string(),
                host_port.to_string(),
                format!("user@localhost"),
            ],
        ),
        "adb" => {
            // adb connect switches the daemon's target; passing the device
            // through as an argument keeps multiple guests usable.
            (
                "adb",
                vec!["connect".to_string(), format!("localhost:{host_port}")],
            )
        }
        _ => unreachable!(),
    };

    let status = std::process::Command::new(cmd)
        .args(&args)
        .status()
        .map_err(|e| format!("cannot run `{cmd}`: {e} (is it installed and on PATH?)"))?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

/// Resolves a partial/64-hex id to the full id through the instance list
/// (the daemon resolves ids server-side per RPC, but the console socket
/// path needs the full id client-side).
async fn resolve_full_id(
    client: &mut TracedClient,
    raw: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let needle = raw.to_ascii_lowercase();
    let entries = client
        .list_instances(Empty {})
        .await?
        .into_inner()
        .instances;
    let matches: Vec<String> = entries
        .iter()
        .filter(|e| e.instance_id.starts_with(&needle))
        .map(|e| e.instance_id.clone())
        .collect();
    match matches.len() {
        0 => Err(format!("no instance found matching {raw:?}").into()),
        1 => Ok(matches[0].clone()),
        _ => Err(format!(
            "instance id {raw:?} is ambiguous ({} instances match)",
            matches.len()
        )
        .into()),
    }
}

/// Ctrl+] ends the console session. The relay runs with `ISIG` off so that ^C,
/// ^Z and ^\\ reach the guest like they would on a physical serial line, which
/// is also why the local session needs an escape of its own: without one there
/// is no key that ends it, and a guest that never returns to its prompt (or
/// never boots) leaves the operator attached with no way out.
const DETACH: u8 = 0x1d;

/// Set by the exit-signal handlers. The relay polls with a short timeout, so a
/// signal that arrives while it is attached leaves through the same path as a
/// detach — terminal restored first — instead of leaving the tty in raw mode.
static SIGNALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn install_exit_handlers() {
    extern "C" fn on_signal(_signal: libc::c_int) {
        SIGNALLED.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    // SAFETY: the handler only stores to an atomic — async-signal-safe — and
    // every other sigaction field is a null/zero constant set below.
    unsafe {
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            let mut action: libc::sigaction = std::mem::zeroed();
            // Through a raw pointer: casting a function item straight to an
            // integer is what `function_casts_as_integer` forbids.
            let handler: extern "C" fn(libc::c_int) = on_signal;
            action.sa_sigaction = handler as *const () as libc::sighandler_t;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

/// How an attached console session ended.
enum Detached {
    /// The operator pressed the detach key.
    ByKey,
    /// The serial side closed (VM stopped, or QEMU exited).
    Serial,
    /// A signal asked this process to leave.
    Signal,
}

/// Attaches to the serial console socket in raw terminal mode. The chardev
/// serves a single client; the daemon never touches this socket, so a CLI
/// attach session cannot starve it (unlike the QMP/QGA sockets).
async fn connect_console(full_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let id = InstanceId::from_str(full_id).map_err(|e| format!("invalid instance id: {e}"))?;
    let socket_path = andler_core::paths::console_socket_path(&id);
    let mut stream = UnixStream::connect(&socket_path).map_err(|e| {
        format!(
            "cannot connect to console socket {}: {e} (is the instance running?)",
            socket_path.display()
        )
    })?;
    stream.set_read_timeout(None)?;

    install_exit_handlers();
    eprintln!("[console attached — Ctrl+] detaches, ^C goes to the guest]");
    let mut terminal = RawTerminal::enter()?;
    let outcome = pump_console(&mut stream);
    terminal.restore();
    match outcome? {
        Detached::ByKey => {
            println!("[detached]");
            Ok(())
        }
        Detached::Serial => {
            println!("[console disconnected — VM serial closed]");
            Ok(())
        }
        Detached::Signal => Ok(()),
    }
}

/// Where the detach key sits in a chunk of typed bytes, if it is there at all.
fn detach_at(bytes: &[u8]) -> Option<usize> {
    bytes.iter().position(|byte| *byte == DETACH)
}

fn pump_console(stream: &mut UnixStream) -> Result<Detached, Box<dyn std::error::Error>> {
    use std::os::unix::io::AsRawFd;

    let mut stdout = std::io::stdout();
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 4096];
    // EOF on stdin only closes the input direction (piped input); the
    // session stays attached until the serial side closes, so guest output
    // that arrives after the last input byte is still relayed.
    let mut stdin_open = true;
    loop {
        let mut fds = [
            libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: if stdin_open { libc::POLLIN } else { 0 },
                revents: 0,
            },
        ];
        // SAFETY: poll(2) watches two valid open fds with a fresh pollfd array.
        // The timeout only bounds how long a signal can wait behind the relay.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, 200) };
        if SIGNALLED.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(Detached::Signal);
        }
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            // A signal that arrived between the check above and here is not a
            // relay failure; anything else is.
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if fds[0].revents & libc::POLLIN != 0 {
            let read = stream.read(&mut buf)?;
            if read == 0 {
                return Ok(Detached::Serial);
            }
            stdout.write_all(&buf[..read])?;
            stdout.flush()?;
        }
        if stdin_open && fds[1].revents & libc::POLLIN != 0 {
            let read = stdin.read(&mut buf)?;
            if read == 0 {
                stdin_open = false;
                continue;
            }
            match detach_at(&buf[..read]) {
                Some(at) => {
                    stream.write_all(&buf[..at])?;
                    stream.flush()?;
                    return Ok(Detached::ByKey);
                }
                None => {
                    stream.write_all(&buf[..read])?;
                    stream.flush()?;
                }
            }
        }
    }
}

/// Puts the terminal into raw mode (echo off, no line buffering) for the
/// console session and restores it on drop/exit. When stdin is not a
/// terminal (pipes, e2e), the mode is left untouched — there is no
/// terminal state to preserve.
struct RawTerminal {
    original: Option<libc::termios>,
}

impl RawTerminal {
    fn enter() -> Result<Self, Box<dyn std::error::Error>> {
        let fd = libc::STDIN_FILENO;
        // SAFETY: isatty(2) only inspects the fd.
        if unsafe { libc::isatty(fd) } != 1 {
            return Ok(RawTerminal { original: None });
        }
        // SAFETY: zeroed termios is immediately overwritten by tcgetattr
        // below (which fills every field or fails).
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: tcgetattr(2) fills the termios or fails with an error we surface.
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return Err("tcgetattr failed (stdin is not a terminal?)".into());
        }
        let mut raw = original;
        raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG | libc::IEXTEN);
        raw.c_iflag &= !(libc::IXON | libc::ICRNL | libc::BRKINT | libc::INPCK | libc::ISTRIP);
        raw.c_oflag &= !libc::OPOST;
        raw.c_cflag |= libc::CS8;
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: tcsetattr(2) applies the termios we just prepared.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err("tcsetattr failed (stdin is not a terminal?)".into());
        }
        Ok(RawTerminal {
            original: Some(original),
        })
    }

    fn restore(&mut self) {
        if let Some(original) = &self.original {
            // SAFETY: restores the termios captured in enter().
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, original);
            }
        }
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_detach_key_is_found_wherever_it_is_typed() {
        assert_eq!(detach_at(b"ls -la"), None);
        assert_eq!(detach_at(b"abc"), None);
        assert_eq!(
            detach_at(b"\x03\x03"),
            None,
            "^C belongs to the guest — the escape that ends the session has to be its own key"
        );
        assert_eq!(detach_at(&[b'a', DETACH, b'b']), Some(1));
        assert_eq!(detach_at(&[DETACH]), Some(0));
    }
}
