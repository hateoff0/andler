# andler-helper

The privileged counterpart of `andlerd`'s offline guest flows — a single
root-owned binary authorized by one `NOPASSWD` sudoers rule, replacing the ten
per-binary rules (`qemu-nbd`, `mount`, `umount`, `chroot`, `modprobe`,
`mkdir`, `cp`, `mv`, `rm`, `chmod`) the daemon previously needed to run
offline disk/guest operations.

```
youruser ALL=(root) NOPASSWD: /usr/local/sbin/andler-helper
```

Installed to `/usr/local/sbin/andler-helper` (root:root 0755) by
`andler doctor --fix` (interactive `sudo install`; `install.sh` stays
root-less). `andler doctor` audits the binary and the sudoers rule;
`uninstall.sh --purge` removes the binary.

## Why a helper binary instead of per-tool sudoers rules

- **One line to audit**, and `doctor --fix` can migrate legacy rule sets
  in place (it refuses files that contain foreign lines rather than
  overwriting them).
- **Argument validation at the boundary**: every subcommand re-validates its
  arguments immediately before acting — nbd devices against `/sys/class/block`
  (must exist and be free), mount points against the invoking user's
  `SUDO_UID`, guest paths against the managed-mount tree parsed from
  `/proc/self/mountinfo` (a path must resolve inside a mount whose source is
  our nbd device / tmpfs / host bind), chrooted commands against an
  `apt-get|apt|dnf|pacman|ln` allowlist.
- **No shellouts anywhere**: no string construction that could be
  misquoted; paths stay `CString`-bound and never pass through `sh`.
- **Install surface**: std-only plus `libc` (for `chroot(2)`), no
  vendored dependencies, no network access.

The helper narrows and structure-hardens the privilege boundary — it is not a
sandbox. A compromised daemon can still invoke any subcommand or reinstall the
binary; the model is defence against bugs and other users, documented honestly
in `docs/ARCHITECTURE.md`.

## Subcommands

| Subcommand | Arguments | What it does |
|---|---|---|
| `nbd-connect` | `<dev> <image>` | connect a free `/dev/nbdN` to a qcow2 image (dev regex + free-check via `/sys/class/block`, image realpath + regular file) |
| `nbd-disconnect` | `<dev>` | disconnect; idempotent no-op when already free |
| `modprobe-nbd` | — | load the nbd module with fixed arguments (`modprobe nbd max_part=8`), zero input |
| `mount-partition` | `<dev> <dir>` | mount a guest partition rw on the mount point |
| `mount-bind` | `<src> <dir>` | bind-mount `/dev`, `/proc` or `/sys` into the guest |
| `mount-tmpfs` | `<dir>` | fresh tmpfs on the guest's `/run` (gpg-agent needs writable `/run`) |
| `umount` | `<dir>` | lazy-unmount one of our mounts (membership-checked against mountinfo) |
| `chroot-run` | `<dir> <cmd> <args…>` | `chroot(2)` + `execvp`, allowlisted commands, clean env (`PATH`, `HOME=/root`) |
| `guest-write` | `<dir> <path>` | stdin → file inside the guest root (replaces the `sh -c` resolv hack and the host `/tmp` temp-file detour). `path` is guest-relative: either `<dir>/etc/resolv.conf` or `/etc/resolv.conf` (guest root-absolute) — never a raw host path |
| `file` | `<dir> <op> <paths…>` | `mkdir-p` / `cp-a` / `mv` / `rm-rf` / `chmod` under the guest. All paths use the guest-relative forms above and must resolve inside a managed mount — except `cp-a`'s **source**, which is a host path (the translator cache under `~/.andler/`), validated only to exist and be read; the destination stays inside the guest |
| `sudoers-print` | — | print `/etc/sudoers.d/andler` (backstop read for `doctor` when the daemon's user has no read access) |
| `--version` / `--help` | — | allowed unprivileged, exit 0 |

Exit codes: `0` success, `1` operation failed (child exit code for
`chroot-run`), `2` usage or validation error. `--help`/`--version` work
without root; every operation requires euid 0.

## Interface contracts

- Every operation requires `SUDO_UID` unless the invoking euid is 0 already
  (`$SUDO_UID` ownership checks on mount points and guest paths).
- Managed mounts: parsed from `/proc/self/mountinfo` — a mount is managed when
  `root=="/"` and the source is a nbd device / tmpfs / `/dev|/proc|/sys` bind,
  or when the mount point lies under `/run/user/$SUDO_UID/andler-mounts`.
- The daemon (`andler-disk`) calls every subcommand through
  `sudo -n /usr/local/sbin/andler-helper <sub> …`; failures are surfaced via
  `describe_helper_failure` which points at `andler doctor --fix`.

## Tests

17 unit tests (validation + mountinfo parsing + copy/remove helpers, all
root-less) plus an `#[ignore]`-gated full cycle
(`tests/helper_cycle.rs`): temporary qcow2 → nbd-connect → mkfs → mount →
file ops → guest-write → umount → disconnect ×2, requiring root, `qemu-img`,
the nbd module and a free `/dev/nbd*`.