# scripts

Helper scripts for running ANDLER on a real machine. They are not part of the
product: the binaries themselves come from a release or from `cargo build`.

| File | Purpose |
| :--- | :--- |
| `install.sh` | host dependency report (optional), binaries, and the systemd user unit |
| `uninstall.sh` | the reverse — service, binaries, and (on request) the data root |
| `ui.sh` | presentation helpers shared by both: banner, aligned rows, ✓ / ⚠ / ✗ lines |
| `andlerd.service` | the systemd **user** unit both scripts install and remove |

## `install.sh` — dependencies, binaries, and the user service

```bash
scripts/install.sh                                       # local build: daemon + CLI + user service
scripts/install.sh /path/to/andlerd                      # explicit local daemon binary
scripts/install.sh --from-release                        # latest release, same three things
scripts/install.sh --check-deps                          # host dependency report only
scripts/install.sh --component cli --no-service --from-release v0.1.0
```

| Flag | Effect |
| :--- | :--- |
| `--component daemon\|cli\|both` | what to install (default `both` — the CLI refuses a daemon from a different version, so the pair is the only combination that cannot drift) |
| `--from-release [TAG]` | install a published release instead of local binaries; the tag defaults to the latest release. Uses `gh` when present, otherwise `curl` against the GitHub API (`GH_TOKEN`/`GITHUB_TOKEN`/`ANDLER_INSTALL_TOKEN` for a private repository) |
| `--bin-dir DIR` | where the binaries land (default `~/.local/bin`) |
| `--no-service` | install binaries only, leave systemd alone |
| `--check-deps` | print the dependency report and exit — status 1 when a *required* one is missing |
| `--skip-deps` | install without checking (for images/CI that provision later) |
| `--repo OWNER/REPO` | release source (default `hateoff0/andler`) |
| `--no-color` | plain output (`NO_COLOR` is honoured too) |

### The dependency report

Every run except `--skip-deps` starts with the host check — the same questions
`andler doctor` asks, but scoped to what this invocation actually covers
(`--component cli` on a client machine requires nothing local; the systemd check
only runs when the unit is being installed):

```text
▸ Host dependencies
  required for '--component both' + the systemd user service
  ✓ qemu-system-x86_64 · /usr/bin/qemu-system-x86_64
  ✓ qemu-img · /usr/bin/qemu-img
  ✓ OVMF/UEFI firmware · /usr/share/edk2/x64/OVMF_CODE.4m.fd + /usr/share/edk2/x64/OVMF_VARS.4m.fd
  ✓ /dev/kvm · accessible
  ✓ systemd --user · systemd --user reachable

  optional — a missing one disables only the feature named, everything else works
  ✓ ip (iproute2) · /usr/bin/ip
  ⚠ guestfish (guestfs-tools) · offline guest operations (install/remove, boot mode, ARM translators)
      → sudo pacman -S --needed libguestfs

  ✓ required dependencies · all present
  1 optional dependency is missing — the feature named above stays unavailable
```

Required dependencies (KVM access, `qemu-system-x86_64`, `qemu-img`, an
OVMF/UEFI pair, and `tar`/`sha256sum`/`curl` on the release path) stop the
install with the command that fixes them — one per gap, plus a single
`pacman`/`apt`/`dnf` line that installs all of them at once for Arch, Debian and
Fedora families. Optional ones (`ip`, `unshare`, `/dev/net/tun`,
`CAP_NET_ADMIN`, `passt`, `guestfish`, `debugfs`, `oras`, `lspci`, `glxinfo`,
`nvidia-smi`) are reported with the feature they carry and never block. The full
list with package names per family is in the README's *Host Requirements &
Dependencies* section.

Release installs are checksum-verified against the published `.sha256` before
anything is written, and the script re-checks the two installed versions
afterwards, warning when a locally assembled pair mismatches.

### What it writes

Nothing outside `--bin-dir` and `~/.config/systemd/user/`. Installing the
daemon writes the unit with `ExecStart` pointing at the binary it just
installed, then `systemctl --user daemon-reload` + `enable --now andlerd`, and
probes the daemon through the freshly installed CLI before declaring success.
Re-running is idempotent: it overwrites, re-checks and re-points.

A release publishes one archive per component, plus the pair:
`andlerd-<tag>-linux-x86_64.tar.gz`, `andler-cli-<tag>-linux-x86_64.tar.gz` and
`andler-<tag>-linux-x86_64.tar.gz`. The script downloads the one that matches
`--component` and verifies its published `.sha256`.

The CLI and the daemon must come from the same release: `andler` checks the
daemon's version before every command and refuses a daemon built from a
different one.

## `andlerd.service` — the user unit

Systemd **user** service for `andlerd`. Deliberately a user unit
(`systemctl --user`), not a system unit — `andlerd`'s GPU/audio/display
auto-detection and the QEMU windows it spawns depend on the invoking user's own
session (see the comment at the top of the unit for the full reasoning).
`install.sh` copies it and substitutes the binary path; installing by hand means
copying it to `~/.config/systemd/user/`, `systemctl --user daemon-reload` and
`systemctl --user enable --now andlerd`.

## `uninstall.sh` — remove what install.sh put there

```bash
scripts/uninstall.sh                  # stop + disable + delete the unit
scripts/uninstall.sh --binaries       # also remove ~/.local/bin/{andler,andlerd}
scripts/uninstall.sh --binaries --purge --yes   # …and the data root, unattended
```

| Flag | Effect |
| :--- | :--- |
| `--binaries` | remove `andler`/`andlerd` from `--bin-dir` |
| `--bin-dir DIR` | where they live (default `~/.local/bin`; implies `--binaries`) |
| `--purge` | delete the data root — instances, disks, snapshots, database |
| `--yes` | answer the `--purge` confirmation (which otherwise needs a TTY and a typed `yes`) |
| `--no-color` | plain output |

The data root is `$ANDLER_HOME`, defaulting to `~/.andler`; `--purge` deletes
exactly that directory, and the runtime socket directory
(`$XDG_RUNTIME_DIR/andler`) when no `andlerd` process is using it. Nothing else
is touched: binaries elsewhere on `PATH` stay, and instance data survives
without `--purge`.
