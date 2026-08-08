//! Argument parsing and subcommand dispatch for `andler-helper`.
//!
//! The only entry point andlerd has into root: every privileged offline-guest
//! operation is one validated subcommand here. Dispatch and validation happen
//! before anything is executed; unknown subcommands and malformed arguments are
//! rejected with exit code 2 and a message on stderr.

mod mountinfo;
mod ops;
mod validate;

use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
andler-helper — privileged operations for andlerd's offline guest flows.

Invoked by andlerd through the single sudoers rule:
    youruser ALL=(root) NOPASSWD: /usr/local/sbin/andler-helper
Never run interactively; every subcommand validates its arguments strictly
and none of them shell out.

Subcommands:
  nbd-connect <dev> <image>   connect a free /dev/nbd* to a qcow2 image
  nbd-disconnect <dev>        disconnect (no-op when already free)
  modprobe-nbd                load the nbd module (fixed arguments)
  mount-partition <dev> <dir> mount a guest partition rw on the mount point
  mount-bind <src> <dir>      bind-mount /dev, /proc or /sys into the guest
  mount-tmpfs <dir>           mount a tmpfs on the guest's /run
  umount <dir>                lazy-unmount one of our mounts
  chroot-run <dir> <cmd> ...  run a command chrooted into the guest root
  guest-write <dir> <path>    write stdin into a file in the guest root
  file <dir> <op> <paths...>  mkdir-p | cp-a | mv | rm-rf | chmod under the guest
  sudoers-print               print /etc/sudoers.d/andler (for doctor --fix)
  --version | --help          print and exit 0

Exit codes: 0 success, 1 operation failed (child exit code for chroot-run),
2 usage or validation error.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    match args[0].as_str() {
        "--help" | "-h" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        "--version" | "-V" => {
            println!("andler-helper {VERSION}");
            ExitCode::SUCCESS
        }
        // SAFETY: geteuid(2) is a pure POSIX call — no arguments, no mutable state.
        _ if unsafe { libc::geteuid() } != 0 => {
            eprintln!("andler-helper: must run as root (via sudo)");
            ExitCode::from(2)
        }
        "nbd-connect" => run_exact(&args, 3, |a| ops::nbd_connect(&a[1], &a[2])),
        "nbd-disconnect" => run_exact(&args, 2, |a| ops::nbd_disconnect(&a[1])),
        "modprobe-nbd" => run_exact(&args, 1, |_| ops::modprobe_nbd()),
        "mount-partition" => run_exact(&args, 3, |a| ops::mount_partition(&a[1], &a[2])),
        "mount-bind" => run_exact(&args, 3, |a| ops::mount_bind(&a[1], &a[2])),
        "mount-tmpfs" => run_exact(&args, 2, |a| ops::mount_tmpfs(&a[1])),
        "umount" => run_exact(&args, 2, |a| ops::umount(&a[1])),
        "chroot-run" => run_chroot_run(&args),
        "guest-write" => run_exact(&args, 3, |a| ops::guest_write(&a[1], &a[2])),
        "file" => run_file_op(&args),
        "sudoers-print" => finish(ops::sudoers_print()),
        other => {
            eprintln!("andler-helper: unknown subcommand {other:?}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run_exact(
    args: &[String],
    n: usize,
    f: impl FnOnce(&[String]) -> Result<i32, String>,
) -> ExitCode {
    if args.len() != n {
        eprintln!(
            "andler-helper: {} expects {n} argument(s), got {}",
            args[0],
            args.len() - 1
        );
        return ExitCode::from(2);
    }
    finish(f(args))
}

fn run_chroot_run(args: &[String]) -> ExitCode {
    if args.len() < 3 {
        eprintln!("andler-helper: chroot-run expects <dir> <cmd> [args...]");
        return ExitCode::from(2);
    }
    finish(ops::chroot_run(&args[1], &args[2], &args[3..]))
}

fn run_file_op(args: &[String]) -> ExitCode {
    if args.len() < 3 {
        eprintln!("andler-helper: file expects <op> <dir> <paths...>");
        return ExitCode::from(2);
    }
    finish(ops::file_op(&args[1], &args[2], &args[3..]))
}

fn finish(result: Result<i32, String>) -> ExitCode {
    match result {
        Ok(code) => ExitCode::from(code as u8),
        Err(msg) => {
            eprintln!("andler-helper: {msg}");
            ExitCode::from(2)
        }
    }
}
