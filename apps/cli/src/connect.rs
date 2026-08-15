use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::str::FromStr;

use crate::TracedClient;
use andler_core::InstanceId;
use andler_rpc::proto::{Empty, InstanceIdRequest, InstanceStateKind};

use crate::ConnectLevel;

/// `andler connect` — single entry point to the guest. The best available
/// level is chosen for the instance's effective profile; `--level` forces
/// one. Phase 2 ships `console` (works on any VM, any state that runs QEMU)
/// and `exec` (needs the guest agent); ssh/adb land with provisioning.
pub async fn handle_connect(
    client: &mut TracedClient,
    instance_id: String,
    level: ConnectLevel,
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

    match level {
        ConnectLevel::Console => connect_console(&full_id).await,
        ConnectLevel::Ssh => connect_via_port_forward(client, &full_id, 22, "ssh").await,
        ConnectLevel::Adb => connect_via_port_forward(client, &full_id, 5555, "adb").await,
        // Effective profile (kind × boot_mode, PLAN §12.A): an Android
        // instance booted into linux mode is reached over ssh, one booted
        // into android over adb; plain Linux VMs keep the serial console.
        ConnectLevel::Auto => {
            let config = client
                .get_instance_config(InstanceIdRequest {
                    instance_id: full_id.clone(),
                })
                .await?
                .into_inner();
            let is_android_booted_linux = config.kind.as_ref().is_some_and(|kind| {
                matches!(
                    kind.kind,
                    Some(andler_rpc::proto::instance_kind::Kind::AndroidVm(_))
                )
            }) && config.boot_mode
                == andler_rpc::proto::AndroidBootMode::Linux as i32;
            if is_android_booted_linux {
                connect_via_port_forward(client, &full_id, 22, "ssh").await
            } else {
                connect_console(&full_id).await
            }
        }
    }
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
    let config = client
        .get_instance_config(InstanceIdRequest {
            instance_id: full_id.to_string(),
        })
        .await?
        .into_inner()
        .network
        .ok_or_else(|| "daemon returned no network config".to_string())?;

    let host_port = config
        .port_forwards
        .iter()
        .find(|f| f.guest_port == guest_port as u32)
        .map(|f| f.host_port)
        .ok_or_else(|| {
            format!(
                "no port forward to guest port {guest_port} on {full_id}; add \
                 `network.port_forwards = [\"tcp:2222->{guest_port}\"]` (host:guest) to \
                 instance.toml before starting, then `andler connect --level {what}`"
            )
        })?;

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

    let mut terminal = RawTerminal::enter()?;
    let result = pump_console(&mut stream);
    terminal.restore();
    result
}

fn pump_console(stream: &mut UnixStream) -> Result<(), Box<dyn std::error::Error>> {
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
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ready < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if fds[0].revents & libc::POLLIN != 0 {
            let read = stream.read(&mut buf)?;
            if read == 0 {
                println!("\n[console disconnected — VM serial closed]");
                return Ok(());
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
            stream.write_all(&buf[..read])?;
            stream.flush()?;
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
