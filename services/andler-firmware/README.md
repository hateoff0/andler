# andler-firmware

Host hardware auto-detection and GPU metrics collection for ANDLER.

## Modules

### `detect/` — Hardware Auto-Detection

The `detect_all()` entry point returns a `HardwareDefaults` struct with auto-detected values for all hardware components.

| Module | What It Detects | How |
|--------|----------------|-----|
| `ovmf.rs` | UEFI/OVMF firmware paths | Scans distro-specific paths for CODE+VARS pairs |
| `gpu.rs` | GPU vendor, render backend, display engine | sysfs + lspci, Venus requirement checks (kernel/QEMU/Mesa versions) |
| `arm.rs` | ARM translator (libndk/libhoudini) | `/proc/cpuinfo` vendor_id (AMD→libndk, Intel→libhoudini) |
| `audio.rs` | Audio server (PipeWire/PulseAudio/None) | Socket file detection in `$XDG_RUNTIME_DIR` |
| `network.rs` | NAT backend (passt available) | Binary check for `/usr/bin/passt` |

### `detect/ovmf.rs` — UEFI/OVMF Detection

| Function | Description |
|---|---|
| `detect_matched_pair()` | Primary entrypoint. Finds CODE+VARS from the same distro package. Falls back to independent `detect()`. |
| `detect()` | Independent search — CODE and VARS found separately. |
| `provision_vars(template, dest)` | Copy OVMF_VARS template into instance directory. |
| `reset_vars(template, dest)` | Delete + re-copy VARS (equivalent to `--reset-boot`). |

**Supported distros:**

| Distro | OVMF_CODE | OVMF_VARS |
|---|---|---|
| Arch / CachyOS / Manjaro | `/usr/share/edk2/x64/OVMF_CODE.4m.fd` | `/usr/share/edk2/x64/OVMF_VARS.4m.fd` |
| Ubuntu / Debian | `/usr/share/OVMF/OVMF_CODE_4M.fd` | `/usr/share/OVMF/OVMF_VARS_4M.fd` |
| openSUSE | `/usr/share/qemu/ovmf-x86_64-code.bin` | `/usr/share/qemu/ovmf-x86_64-vars.bin` |
| Fedora / RHEL | `/usr/share/edk2/ovmf/OVMF_CODE.fd` | `/usr/share/edk2/ovmf/OVMF_VARS.fd` |
| Fallback | `/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd` | `/usr/share/edk2-ovmf/x64/OVMF_VARS.4m.fd` |

**Environment variable overrides (`andlerd`):**
```
ANDLERD_OVMF_CODE=/path/to/OVMF_CODE.fd andlerd
ANDLERD_OVMF_VARS=/path/to/OVMF_VARS.fd andlerd
```

### `detect/gpu.rs` — GPU Detection

Detects GPU vendor via sysfs (primary) and lspci (fallback), then selects appropriate render backend and display engine.

| GPU Vendor | Default Render Backend | Default Display Engine |
|------------|----------------------|----------------------|
| NVIDIA | Venus (if requirements met) | SDL |
| AMD | Venus (if requirements met) | GTK |
| Intel | Venus (if requirements met) | GTK |
| Unknown | Venus (if requirements met) | SDL |

**Venus requirement checks:**
- Kernel >= 6.13
- QEMU >= 9.2
- Mesa >= 24.2

Falls back to VirGL if requirements not met.

### `detect/arm.rs` — ARM Translator Detection

Reads `/proc/cpuinfo` to detect CPU vendor and selects the appropriate ARM→x86 translator.

| CPU Vendor | Translator |
|------------|-----------|
| AuthenticAMD | Libndk |
| GenuineIntel | Libhoudini |
| Unknown | None |

### `detect/audio.rs` — Audio Server Detection

Checks for PipeWire and PulseAudio socket files in `$XDG_RUNTIME_DIR` and `/run/user/{uid}/`.

| Socket Found | Backend |
|-------------|---------|
| `pipewire-0` | PipeWire |
| `pulse/native` | PulseAudio |
| Neither | None |

### `detect/network.rs` — Network Backend Detection

Checks for the `passt` binary at `/usr/bin/passt` and `/usr/local/bin/passt`.

| Binary Found | NAT Backend |
|-------------|-------------|
| `passt` exists | Passt |
| `passt` missing | Slirp (fallback) |

## `metrics/` — GPU Metrics Collection

Collects GPU utilization metrics from the host. Called by `andler-qemu/src/metrics.rs`.

| Module | Source | Metrics |
|--------|--------|---------|
| `gpu_amd.rs` | sysfs (`/sys/class/drm/card*/device/`) | VRAM used/total, GPU busy percent |
| `gpu_nvidia.rs` | NVML (`nvml-wrapper` crate, primary) + `nvidia-smi` CLI fallback | VRAM used/total, GPU utilization |
| `gpu_intel.rs` | sysfs (`/sys/class/drm/card*/device/`) | GPU load via rc6_residency_ms delta |

**Vendor priority:** AMD → NVIDIA → Intel (first found vendor wins).

| Function | Description |
|----------|-------------|
| `read_gpu_metrics()` | Collect metrics from the first available GPU vendor |
| `merge_gpu_metrics(base, gpu)` | Merge GPU metrics into a `ResourceMetrics` struct |

## `HardwareDefaults`

Returned by `detect_all()`. Contains auto-detected values for all hardware components:

```rust
pub struct HardwareDefaults {
    pub ovmf: Result<DetectedOvmf, FirmwareError>,
    pub gpu_render: RenderBackend,
    pub display_engine: DisplayEngine,
    pub audio_server: AudioServer,
    pub arm_translator: Option<ArmTranslator>,
    pub venus_supported: bool,
    pub passt_available: bool,
}
```

Used by the interactive wizard to pre-fill defaults.

## Tests

48 tests across `detect/` and `metrics/`:

| Module | Tests |
|--------|-------|
| `detect/gpu.rs` | 10 |
| `detect/ovmf.rs` | 6 |
| `detect/arm.rs` | 5 |
| `detect/audio.rs` | 5 |
| `detect/network.rs` | 1 |
| `metrics/gpu_intel.rs` | 11 |
| `metrics/mod.rs` | 5 |
| `metrics/gpu_nvidia.rs` | 4 |
| `metrics/gpu_amd.rs` | 1 |
