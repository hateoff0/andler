# andler-firmware

UEFI/OVMF firmware detection and provisioning for ANDLER.

## What it does

Implements the auto-detection logic from `start.sh` and `start2.sh` as a reusable Rust library:

```bash
# start.sh equivalent
for p in "${BIOS_CODE_PATHS[@]}"; do [ -f "$p" ] && OVMF_CODE="$p" && break; done
```

## API

| Function | Description |
|---|---|
| `detect_matched_pair()` | Primary entrypoint. Finds the first CODE+VARS pair where *both* files belong to the same distro package. Falls back to independent `detect()` if no complete pair found. |
| `detect()` | Independent search — CODE and VARS found separately. |
| `provision_vars(template, dest)` | Copy OVMF_VARS template into an instance directory (`async`). |
| `reset_vars(template, dest)` | Delete + re-copy VARS — equivalent to `--reset-boot` in `start.sh`. |
| `KNOWN_OVMF_CODE_PATHS` | Const list of system paths checked, in priority order. |
| `KNOWN_OVMF_VARS_PATHS` | Const list of system paths checked, in priority order. |

## Supported distros / paths

| Distro | Package | OVMF_CODE | OVMF_VARS |
|---|---|---|---|
| Arch / CachyOS / Manjaro | `edk2-ovmf` | `/usr/share/edk2/x64/OVMF_CODE.4m.fd` | `/usr/share/edk2/x64/OVMF_VARS.4m.fd` |
| Ubuntu / Debian | `ovmf` | `/usr/share/OVMF/OVMF_CODE_4M.fd` | `/usr/share/OVMF/OVMF_VARS_4M.fd` |
| openSUSE | `qemu` | `/usr/share/qemu/ovmf-x86_64-code.bin` | `/usr/share/qemu/ovmf-x86_64-vars.bin` |
| Fedora / RHEL | `edk2-ovmf` | `/usr/share/edk2/ovmf/OVMF_CODE.fd` | `/usr/share/edk2/ovmf/OVMF_VARS.fd` |
| Fallback | `edk2-ovmf` | `/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd` | `/usr/share/edk2-ovmf/x64/OVMF_VARS.4m.fd` |

## Why `detect_matched_pair` over `detect`?

`detect()` finds CODE and VARS independently — theoretically you could end up with
an Arch CODE + a Fedora VARS. That usually works, but `detect_matched_pair()` prefers
to return files from the same package, which is cleaner. It degrades to `detect()` on
unusual systems (e.g. only one file from a pair exists).

## Environment variable overrides (`andlerd`)

```
ANDLERD_OVMF_CODE=/path/to/OVMF_CODE.fd andlerd
ANDLERD_OVMF_VARS=/path/to/OVMF_VARS.fd andlerd
```

Either or both can be set. Unset = auto-detect for that path.
