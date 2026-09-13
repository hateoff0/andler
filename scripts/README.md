# scripts

Helper scripts for running ANDLER on a real machine. They are not part of the
product: the binaries themselves come from a release or from `cargo build`.

## `install.sh` — binaries, and the user service

```bash
scripts/install.sh                                       # local build: find andlerd, set up the unit
scripts/install.sh /path/to/andlerd                      # explicit local binary
scripts/install.sh --component both --from-release       # latest release: CLI + daemon
scripts/install.sh --component cli --from-release v0.1.0 --no-service
```

| Flag | Effect |
| :--- | :--- |
| `--component daemon\|cli\|both` | what to install (default `daemon`) |
| `--from-release [TAG]` | download from GitHub Releases instead of a local binary; the tag defaults to the latest release (needs `gh`, or `curl` plus an explicit tag) |
| `--bin-dir DIR` | where the binaries land (default `~/.local/bin`) |
| `--no-service` | install binaries only, leave systemd alone |
| `--repo OWNER/REPO` | release source (default `hateoff0/andler`) |

A release publishes one archive per component, plus the pair: `andlerd-<tag>-linux-x86_64.tar.gz`,
`andler-cli-<tag>-linux-x86_64.tar.gz` and `andler-<tag>-linux-x86_64.tar.gz`. The script downloads the one
that matches `--component`, verifies its published `.sha256`, and installs the
binaries into `--bin-dir`. Installing the daemon also writes the systemd user
unit with `ExecStart` pointing at the installed binary, then enables and starts
it.

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
scripts/uninstall.sh --purge          # also delete ~/.andler (asks on a TTY)
```

Nothing else is touched: binaries elsewhere on `PATH` stay, and instance data
survives unless `--purge` is passed.
