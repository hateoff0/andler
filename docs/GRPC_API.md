# gRPC API Reference

**Protocol definition:** `services/andler-rpc/proto/andler.proto`  
**Implementation:** Tonic/Prost (Rust)

This document provides a complete reference for the Andler gRPC API, including service methods, message schemas, enumerations, and design rationale.

---

## Service: `AndlerService`

The core service exposing all instance management operations.

| RPC Method | Request Type | Response Type | Streaming | Description |
|------------|--------------|---------------|-----------|-------------|
| `CreateInstance` | `CreateInstanceRequest` | `CreateInstanceResponse` | Unary | Creates a Linux VM from an ISO. |
| `CreateAndroidInstance` | `CreateAndroidInstanceRequest` | `CreateInstanceResponse` | Unary | Creates an Android VM from a profile. |
| `StartInstance` | `InstanceIdRequest` | `Empty` | Unary | Starts a created instance. |
| `StopInstance` | `StopInstanceRequest` | `Empty` | Unary | Stops a running instance. |
| `PauseInstance` | `InstanceIdRequest` | `Empty` | Unary | Pauses a running instance. |
| `ResumeInstance` | `InstanceIdRequest` | `Empty` | Unary | Resumes a paused instance. |
| `GetInstanceStatus` | `InstanceIdRequest` | `InstanceStatusResponse` | Unary | Retrieves the current state of an instance. |
| `ListInstances` | `Empty` | `ListInstancesResponse` | Unary | Lists all registered instances. |
| `RemoveInstance` | `RemoveInstanceRequest` | `Empty` | Unary | Removes an instance record. |
| `GetInstanceConfig` | `InstanceIdRequest` | `GetInstanceConfigResponse` | Unary | Returns the full configuration of an instance. |
| `UpdateInstanceConfig` | `UpdateInstanceConfigRequest` | `Empty` | Unary | Replaces the entire configuration of an instance. |
| `StreamInstanceLogs` | `InstanceIdRequest` | `stream LogLineResponse` | Server-streaming | Streams live logs from the hypervisor process. |
| `StreamResourceMetrics` | `InstanceIdRequest` | `stream ResourceMetricsResponse` | Server-streaming | Streams CPU, memory, disk, network, and GPU metrics. |
| `CloneInstance` | `CloneInstanceRequest` | `CreateInstanceResponse` | Unary | Clones an existing instance. |
| `ExportInstanceDisk` | `ExportInstanceDiskRequest` | `ExportInstanceDiskResponse` | Unary | Exports the disk file of a Linux or Android VM to a standalone path. |
| `CreateSnapshot` | `CreateSnapshotRequest` | `CreateSnapshotResponse` | Unary | Creates a snapshot of a running/paused instance. |
| `RestoreSnapshot` | `RestoreSnapshotRequest` | `Empty` | Unary | Restores an instance to a snapshot state. |
| `DeleteSnapshot` | `DeleteSnapshotRequest` | `Empty` | Unary | Deletes a snapshot. |
| `ListSnapshots` | `InstanceIdRequest` | `ListSnapshotsResponse` | Unary | Lists all snapshots for an instance. |
| `InstallGuestAgent` | `InstallGuestAgentRequest` | `Empty` | Unary | Installs a package in the guest OS. |
| `RemoveGuestAgent` | `RemoveGuestAgentRequest` | `Empty` | Unary | Removes a package from the guest OS. |
| `ListGuestPackages` | `InstanceIdRequest` | `ListGuestPackagesResponse` | Unary | Lists known guest packages and their installation status. |
| `SwitchArmTranslator` | `SwitchArmTranslatorRequest` | `Empty` | Unary | Switches the ARM translator in offline mode. |
| `SetInstanceConfig` | `SetInstanceConfigRequest` | `Empty` | Unary | Partially updates an instance configuration by key. |
| `SwitchAndroidBootMode` | `SwitchAndroidBootModeRequest` | `Empty` | Unary | Switches the Android boot mode in offline mode. |
| `GetAndroidBootMode` | `InstanceIdRequest` | `GetAndroidBootModeResponse` | Unary | Gets the current Android boot mode. |
| `AttachDisk` | `AttachDiskRequest` | `AttachDiskResponse` | Unary | Hot-plugs an extra disk into a running/paused instance and persists it in the config. |
| `DetachDisk` | `DetachDiskRequest` | `Empty` | Unary | Hot-unplugs an extra disk (the image file is kept). |
| `AttachNetwork` | `AttachNetworkRequest` | `AttachNetworkResponse` | Unary | Hot-plugs an extra network device into a running/paused instance and persists it in the config. |
| `DetachNetwork` | `DetachNetworkRequest` | `Empty` | Unary | Hot-unplugs an extra network device by index. |

---

## Messages and Enums

### `CpuPriority`

Defines the scheduling priority for the VM process.

| Value | Description |
|-------|-------------|
| `CPU_PRIORITY_UNSPECIFIED` (0) | Unspecified (invalid for requests). |
| `LOW` (1) | Low priority. |
| `NORMAL` (2) | Normal priority (default). |
| `HIGH` (3) | High priority. |

### `CpuConfig`

CPU configuration.

| Field | Type | Description |
|-------|------|-------------|
| `cores` | `uint32` | Number of virtual cores. |
| `sockets` | `uint32` | Number of sockets (chips). |
| `threads` | `uint32` | Threads per core (SMT). |
| `affinity` | `repeated uint64` | List of physical core IDs to pin the VM to. Empty list means no pinning (`None`). |
| `priority` | `CpuPriority` | Scheduling priority. |

### `MemoryConfig`

Memory configuration.

| Field | Type | Description |
|-------|------|-------------|
| `size_bytes` | `uint64` | Allocated RAM size in bytes. |
| `ballooning` | `bool` | Enable memory ballooning (dynamic reclamation). |
| `zram` | `bool` | Enable zram for compressed swap. |
| `ksm` | `bool` | Enable Kernel Same-page Merging for deduplication. |

### `DiskFormat`

Disk image format.

| Value | Description |
|-------|-------------|
| `DISK_FORMAT_UNSPECIFIED` (0) | Unspecified (invalid). |
| `QCOW2` (1) | QEMU QCOW2 format (default). |
| `RAW` (2) | Raw disk image. |
| `VDI` (3) | VirtualBox VDI format. |

### `CdromBus`

CD-ROM bus interface. `CDROM_BUS_UNSPECIFIED` is rejected on the create/update paths (`ConvertError::MissingField("cdrom_bus")` → `INVALID_ARGUMENT`) — the field is mandatory in requests. The conversion from a stored config (daemon → proto direction) maps `CdromBus::Ide`/`VirtioScsi` explicitly; there is no silent default.

| Value | Description |
|-------|-------------|
| `CDROM_BUS_UNSPECIFIED` (0) | Invalid in requests — rejected with `INVALID_ARGUMENT` |
| `VIRTIO_SCSI` (1) | VirtIO SCSI (high performance). |
| `IDE` (2) | Traditional IDE (safe fallback). |

### `DiskConfig`

Disk configuration.

| Field | Type | Description |
|-------|------|-------------|
| `path` | `string` | Path to the disk image file. |
| `size_bytes` | `uint64` | Total size of the disk in bytes. |
| `format` | `DiskFormat` | Image format. |
| `base_image` | `string` | Path to a backing image (empty = independent disk). |
| `thin_provisioning` | `bool` | Allocate disk space on write (if supported). |
| `trim_on_shutdown` | `bool` | Issue TRIM commands when VM shuts down. |
| `snapshot_timeout_secs` | `optional uint64` | Accepted for compatibility; unused — snapshot operations are synchronous. |
| `compact_on_shutdown` | `bool` | Automatically compact the disk after shutdown (qcow2 only). |

### `Resolution`

Display resolution.

| Field | Type | Description |
|-------|------|-------------|
| `width` | `uint32` | Horizontal pixels. |
| `height` | `uint32` | Vertical pixels. |

### `DisplayEngine`

Graphics backend/engine.

| Value | Description |
|-------|-------------|
| `DISPLAY_ENGINE_UNSPECIFIED` (0) | Unspecified (invalid). |
| `SDL` (1) | Simple DirectLayer (basic). |
| `SPICE` (2) | SPICE protocol (remote desktop). |
| `DBUS` (3) | D-Bus interface. |
| `DISPLAY_NONE` (4) | No display (headless). |
| `GTK` (5) | GTK rendering (default for desktop hosts, except NVIDIA). |

### `DisplayConfig`

Display configuration.

| Field | Type | Description |
|-------|------|-------------|
| `resolution` | `Resolution` | Screen resolution. |
| `dpi` | `uint32` | Dots per inch. |
| `fps_limit` | `uint32` | Maximum frames per second. |
| `display_engine` | `DisplayEngine` | Graphics backend. |
| `fullscreen` | `bool` | Run in full screen. |

### `RenderBackend`

GPU rendering backend (oneof). Supports passthrough with PCI ID.

| Sub-message | Description |
|-------------|-------------|
| `Venus` | Venus (Vulkan) backend. |
| `VirtioGpu` | VirtIO GPU (software). |
| `VirGl` | VirGL (OpenGL over Vulkan). |
| `Cpu` | CPU rendering (no GPU). |
| `Passthrough` | GPU passthrough (carries `gpu_pci_id`). |

### `GpuConfig`

GPU configuration.

| Field | Type | Description |
|-------|------|-------------|
| `render_backend` | `RenderBackend` | Rendering backend. |
| `hostmem_bytes` | `uint64` | Host memory reserved for GPU (bytes). |
| `blob` | `bool` | Use proprietary GPU drivers if available. |
| `gl` | `bool` | Enable OpenGL. |

### `NetworkMode`

Network configuration (oneof).

| Sub-message | Description |
|-------------|-------------|
| `Nat` | NAT networking (default). |
| `Bridge` | Bridged networking (carries `interface` name). |
| `Isolated` | Isolated (no network). |

### `NatBackend`

NAT implementation (used when `NetworkMode.nat` is selected).

| Value | Description |
|-------|-------------|
| `NAT_BACKEND_UNSPECIFIED` (0) | Unspecified (defaults to `SLIRP`). |
| `SLIRP` (1) | SLIRP user-mode networking. |
| `PASST` (2) | `passt` simple NAT. |

### `NetworkConfig`

Network configuration.

| Field | Type | Description |
|-------|------|-------------|
| `mode` | `NetworkMode` | Network mode (nat/bridge/isolated). |
| `device_model` | `string` | Virtual network device model (e.g., `virtio-net`). |
| `nat_backend` | `NatBackend` | NAT implementation (ignored for bridge/isolated). |

### `FirmwareConfig`

UEFI firmware configuration.

| Field | Type | Description |
|-------|------|-------------|
| `enable_uefi` | `bool` | Enable UEFI boot. |
| `ovmf_code_path` | `string` | Path to OVMF CODE.fd. |
| `ovmf_vars_path` | `string` | Path to OVMF VARS.fd. |

### `AudioBackend`

Audio backend.

| Value | Description |
|-------|-------------|
| `AUDIO_BACKEND_UNSPECIFIED` (0) | Unspecified (defaults to `VIRTIO_SOUND`). |
| `PIPEWIRE` (1) | PipeWire audio. |
| `PULSEAUDIO` (2) | PulseAudio. |
| `AUDIO_NONE` (3) | No audio. |

### `AudioDevice`

Audio device exposed to guest.

| Value | Description |
|-------|-------------|
| `AUDIO_DEVICE_UNSPECIFIED` (0) | Unspecified (defaults to `VIRTIO_SOUND`). |
| `VIRTIO_SOUND` (1) | VirtIO sound device. |
| `ICH9_HDA` (2) | Intel HDA (HD Audio). |

### `AudioConfig`

Audio configuration.

| Field | Type | Description |
|-------|------|-------------|
| `backend` | `AudioBackend` | Host audio backend. |
| `device` | `AudioDevice` | Virtual audio device for guest. |

### `PointerMode`

Pointer input mode.

| Value | Description |
|-------|-------------|
| `POINTER_MODE_UNSPECIFIED` (0) | Unspecified (invalid). |
| `TABLET` (1) | Tablet (absolute) mode. |
| `MOUSE` (2) | Mouse (relative) mode. |

### `InputConfig`

Input configuration.

| Field | Type | Description |
|-------|------|-------------|
| `tablet_mode` | `bool` | **Deprecated** — use `pointer_mode` instead. |
| `hide_host_cursor` | `bool` | Hide host cursor when pointer captured. |
| `clipboard_enabled` | `bool` | Enable shared clipboard. |
| `pointer_mode` | `PointerMode` | Pointer mode (tablet or mouse). |

### `CreateInstanceRequest`

Request to create a Linux VM. Fields correspond 1:1 to `InstanceConfig` except `id` and `backend` (generated by daemon).

| Field | Type | Description |
|-------|------|-------------|
| `name` | `string` | Instance name. |
| `iso_path` | `string` | Path to installer ISO. |
| `cpu` | `CpuConfig` | CPU configuration. |
| `memory` | `MemoryConfig` | Memory configuration. |
| `disk` | `DiskConfig` | Disk configuration. |
| `display` | `DisplayConfig` | Display configuration. |
| `gpu` | `GpuConfig` | GPU configuration. |
| `network` | `NetworkConfig` | Network configuration. |
| `firmware` | `FirmwareConfig` | Firmware configuration. |
| `audio` | `AudioConfig` | Audio configuration. |
| `input` | `InputConfig` | Input configuration. |
| `cdrom_bus` | `CdromBus` | CD-ROM bus (defaults to `IDE`). |

### `AndroidVersion`

Android version to emulate.

| Value | Description |
|-------|-------------|
| `ANDROID_VERSION_UNSPECIFIED` (0) | Unspecified (invalid). |
| `ANDROID_11` (1) | Android 11. |
| `ANDROID_13` (2) | Android 13. |

### `ArmTranslator`

ARM-to-x86 translation layer.

| Value | Description |
|-------|-------------|
| `ARM_TRANSLATOR_UNSPECIFIED` (0) | Unspecified (invalid). |
| `ARM_TRANSLATOR_NONE` (1) | No translation (native ARM). |
| `LIBNDK` (2) | Use NDK translation. |
| `LIBHOUDINI` (3) | Use Houdini translation. |

### `AndroidProfile`

Android VM profile.

| Field | Type | Description |
|-------|------|-------------|
| `android_version` | `AndroidVersion` | Android version. |
| `gapps` | `bool` | Include Google Apps. |
| `microg` | `bool` | Include microG. |
| `arm_translator` | `ArmTranslator` | ARM translation layer. |

### `CreateAndroidInstanceRequest`

Request to create an Android VM.

| Field | Type | Description |
|-------|------|-------------|
| `name` | `string` | Instance name. |
| `profile` | `AndroidProfile` | Android profile. |
| `base_image_path` | `string` | Path to base disk image. |
| `instances_root` | `string` | Root directory for instance files. |
| `overlay_size_bytes` | `uint64` | Size of overlay disk. |
| `ovmf_vars_template` | `string` | Template for OVMF variables. |
| `linked_overlay` | `bool` | Use a linked (backing-file) overlay instead of a standalone copy. |

### `CreateInstanceResponse`

Response after instance creation.

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Generated 64-hex ID of the new instance. |

### `InstanceIdRequest`

Simple request carrying an instance identifier (full 64-hex ID or hex prefix).

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Instance ID. |

### `RemoveInstanceRequest`

Request to remove an instance.

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Instance ID. |
| `purge` | `bool` | If true, delete disk and firmware files. |

### `StopInstanceRequest`

Request to stop an instance.

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Instance ID. |
| `graceful` | `bool` | Send SIGTERM and wait (vs immediate kill). |

### `InstanceStateKind`

VM lifecycle state.

| Value | Description |
|-------|-------------|
| `INSTANCE_STATE_UNSPECIFIED` (0) | Unspecified (invalid). |
| `CREATED` (1) | Instance created, not started. |
| `STARTING` (2) | Starting. |
| `RUNNING` (3) | Running. |
| `PAUSED` (4) | Paused. |
| `STOPPING` (5) | Stopping. |
| `STOPPED` (6) | Stopped. |
| `ERROR` (7) | Error state. |

### `InstanceStatusResponse`

Status of a single instance.

| Field | Type | Description |
|-------|------|-------------|
| `state` | `InstanceStateKind` | Current state. |
| `error_message` | `string` | Error message (if state == ERROR). |
| `detail` | `string` | Additional diagnostic info. |

### `InstanceListEntry`

Entry in the instance list (summary).

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Instance ID. |
| `name` | `string` | Instance name. |
| `state` | `InstanceStateKind` | State. |
| `error_message` | `string` | Error message (empty unless state == ERROR). |

### `ListInstancesResponse`

Response containing a list of instances.

| Field | Type | Description |
|-------|------|-------------|
| `instances` | `repeated InstanceListEntry` | List of instances. |

### `BackendKind`

Hypervisor backend type.

| Value | Description |
|-------|-------------|
| `BACKEND_KIND_UNSPECIFIED` (0) | Unspecified (invalid). |
| `QEMU` (1) | QEMU hypervisor. |

### `InstanceKind`

Type of instance (oneof).

| Sub-message | Description |
|-------------|-------------|
| `LinuxVm` | Linux VM (carries `iso_path` and `cdrom_bus`). |
| `AndroidVm` | Android VM (carries `android_profile`). |

### `GetInstanceConfigResponse`

Full configuration of an instance (read-only).

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Instance ID. |
| `name` | `string` | Instance name. |
| `kind` | `InstanceKind` | VM type. |
| `backend` | `BackendKind` | Hypervisor backend. |
| `cpu` | `CpuConfig` | CPU config. |
| `memory` | `MemoryConfig` | Memory config. |
| `disk` | `DiskConfig` | Disk config. |
| `display` | `DisplayConfig` | Display config. |
| `gpu` | `GpuConfig` | GPU config. |
| `network` | `NetworkConfig` | Network config. |
| `extra_disks` | `repeated DiskConfig` | Hot-plugged extra disks (`andler attach disk`), in attach order. |
| `extra_networks` | `repeated NetworkConfig` | Hot-plugged extra network devices (`andler attach net`), in attach order. |
| `firmware` | `FirmwareConfig` | Firmware config. |
| `audio` | `AudioConfig` | Audio config. |
| `input` | `InputConfig` | Input config. |

### `UpdateInstanceConfigRequest`

Request to replace the entire configuration of an instance. Must match current `id`, `kind`, and `disk.path`.

| Field | Type | Description |
|-------|------|-------------|
| `instance_ref` | `string` | Instance ID (full or prefix). |
| `name` | `string` | New name. |
| `kind` | `InstanceKind` | VM type (must match current). |
| `backend` | `BackendKind` | Backend (must match current). |
| `cpu` | `CpuConfig` | CPU config. |
| `memory` | `MemoryConfig` | Memory config. |
| `disk` | `DiskConfig` | Disk config (path must match). |
| `display` | `DisplayConfig` | Display config. |
| `gpu` | `GpuConfig` | GPU config. |
| `network` | `NetworkConfig` | Network config. |
| `extra_disks` | `repeated DiskConfig` | Hot-plugged extra disks; round-trips through get-edit-put like the rest of the config. |
| `extra_networks` | `repeated NetworkConfig` | Hot-plugged extra network devices; round-trips through get-edit-put like the rest of the config. |
| `firmware` | `FirmwareConfig` | Firmware config. |
| `audio` | `AudioConfig` | Audio config. |
| `input` | `InputConfig` | Input config. |

### `AttachDiskRequest`

Request to hot-plug an extra disk into a running/paused instance.

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Instance ID (full or prefix). |
| `path` | `string` | Image path. Empty = `disk-extraN.qcow2` inside the instance directory; a plain file name is also placed there; an absolute path is used as-is. |
| `size_bytes` | `uint64` | Virtual size for a *new* image (required by the CLI when the path does not exist). Ignored for existing images — the actual virtual size is read from the file. `0` for a new image is rejected (`INVALID_ARGUMENT`). |

### `AttachDiskResponse`

| Field | Type | Description |
|-------|------|-------------|
| `path` | `string` | Resolved image path. |
| `index` | `uint32` | Index in the `extra_disks` list (also the QEMU device index). |

### `DetachDiskRequest`

Request to hot-unplug an extra disk.

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Instance ID (full or prefix). |
| `path` | `string` | Path of the attached disk, as shown by `andler config <id>`. The image file itself is never deleted. |

### `AttachNetworkRequest`

Request to hot-plug an extra network device into a running/paused instance.

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Instance ID (full or prefix). |
| `network` | `NetworkConfig` | Full network config (mode, device model, NAT backend). Missing → `INVALID_ARGUMENT`. |

### `AttachNetworkResponse`

| Field | Type | Description |
|-------|------|-------------|
| `index` | `uint32` | Index in the `extra_networks` list (also the QEMU netdev/device index). |

### `DetachNetworkRequest`

Request to hot-unplug an extra network device.

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Instance ID (full or prefix). |
| `index` | `uint32` | Index in the `extra_networks` list; out-of-range → `NOT_FOUND`. |

### `LogStreamSource`

Source of a log line.

| Value | Description |
|-------|-------------|
| `LOG_STREAM_SOURCE_UNSPECIFIED` (0) | Unspecified (server fills this automatically). |
| `STDOUT` (1) | Standard output. |
| `STDERR` (2) | Standard error. |

### `LogLineResponse`

A single log line from the hypervisor process.

| Field | Type | Description |
|-------|------|-------------|
| `source` | `LogStreamSource` | Log source. |
| `line` | `string` | Log message. |

### `CloneMode`

Cloning strategy.

| Value | Description |
|-------|-------------|
| `CLONE_MODE_UNSPECIFIED` (0) | Unspecified (invalid). |
| `LINKED` (1) | Linked clone (overlay with backing file). |
| `FULL_STANDALONE` (2) | Full standalone clone (flattened). |
| `SHARED_BASE` (3) | Shared base (byte-copy of source disk). |

### `CloneInstanceRequest`

Request to clone an instance.

| Field | Type | Description |
|-------|------|-------------|
| `source_instance_id` | `string` | Source instance ID. |
| `new_name` | `string` | Name for the clone. |
| `instances_root` | `string` | Root directory for new instance files. |
| `mode` | `CloneMode` | Cloning mode. |

### `ExportInstanceDiskRequest`

Request to export an Android VM disk to a standalone file.

| Field | Type | Description |
|-------|------|-------------|
| `source_instance_id` | `string` | Source instance ID. |
| `dest_path` | `string` | Destination file path. |

### `ExportInstanceDiskResponse`

Response confirming export.

| Field | Type | Description |
|-------|------|-------------|
| `dest_path` | `string` | Path to the exported disk. |

### `CreateSnapshotRequest`

Request to create a snapshot.

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Instance ID. |
| `tag` | `string` | Snapshot tag (unique identifier). |
| `description` | `string` | Human-readable description. |
| `timeout_secs` | `optional uint64` | Accepted for compatibility; unused — snapshot operations are synchronous. |

### `CreateSnapshotResponse`

Response after snapshot creation.

| Field | Type | Description |
|-------|------|-------------|
| `snapshot_id` | `string` | Snapshot UUID. |
| `tag` | `string` | Snapshot tag. |
| `created_at` | `string` | Timestamp. |

### `RestoreSnapshotRequest`

Request to restore a snapshot.

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Instance ID. |
| `tag` | `string` | Snapshot tag. |
| `timeout_secs` | `optional uint64` | Operation timeout. |

### `DeleteSnapshotRequest`

Request to delete a snapshot.

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Instance ID. |
| `tag` | `string` | Snapshot tag. |
| `timeout_secs` | `optional uint64` | Operation timeout. |

### `SnapshotEntry`

Entry in the snapshot list.

| Field | Type | Description |
|-------|------|-------------|
| `snapshot_id` | `string` | Snapshot UUID. |
| `tag` | `string` | Snapshot tag. |
| `description` | `string` | Description. |
| `created_at` | `string` | Timestamp. |

### `ListSnapshotsResponse`

Response containing a list of snapshots.

| Field | Type | Description |
|-------|------|-------------|
| `snapshots` | `repeated SnapshotEntry` | List of snapshots. |

### `ResourceMetricsResponse`

Resource metrics snapshot (all fields optional).

| Field | Type | Description |
|-------|------|-------------|
| `cpu_percent` | `optional float` | CPU usage percentage. |
| `memory_used_bytes` | `optional uint64` | RAM used bytes. |
| `disk_read_bytes_per_sec` | `optional uint64` | Disk read rate. |
| `disk_write_bytes_per_sec` | `optional uint64` | Disk write rate. |
| `net_rx_bytes_per_sec` | `optional uint64` | Network receive rate. |
| `net_tx_bytes_per_sec` | `optional uint64` | Network transmit rate. |
| `vram_used_bytes` | `optional uint64` | VRAM used (AMD GPU only). |
| `vram_total_bytes` | `optional uint64` | Total VRAM (AMD GPU only). |
| `gpu_load_percent` | `optional float` | GPU load percentage (AMD GPU only). |

### `InstallGuestAgentRequest`

Request to install a package in the guest OS.

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Instance ID. |
| `package` | `string` | Package name (e.g., `spice-vdagent`). |

### `RemoveGuestAgentRequest`

Request to remove a package from the guest OS.

| Field | Type | Description |
|-------|------|-------------|
| `instance_id` | `string` | Instance ID. |
| `package` | `string` | Package name. |

### `ListGuestPackagesResponse`

Response containing a list of packages.

| Field | Type | Description |
|-------|------|-------------|
| `packages` | `repeated GuestPackageEntry` | List of packages. |

### `SwitchArmTranslatorRequest`

Request to switch ARM translator offline.

| Field | Type | Description |
|-------|------|-------------|
| `instance_ref` | `string` | Instance ID (full or prefix). |
| `translator` | `ArmTranslator` | Target translator. |
| `translator_dir` | `string` | Local directory with translator files (optional). |

### `SetInstanceConfigRequest`

Request for a partial configuration update by key/value.

| Field | Type | Description |
|-------|------|-------------|
| `instance_ref` | `string` | Instance ID (full or prefix). |
| `key` | `string` | Configuration key. Supported: `display.resolution` (any state, applied live to a running guest via the guest agent and persisted via fw_cfg), `name` (stopped instance), `arm_translator` (Android, stopped instance). |
| `value` | `string` | New value. |

### `GuestPackageEntry`

Single package entry.

| Field | Type | Description |
|-------|------|-------------|
| `name` | `string` | Package name. |
| `description` | `string` | Package description. |
| `status` | `string` | Status: `installed`, `not_installed`, or `unknown`. |

---

## Error Codes

| gRPC Status | Daemon Error | When |
|-------------|--------------|------|
| `NOT_FOUND` | `InstanceNotFound`, `SnapshotNotFound`, `InstanceRefNotFound`, `DiskNotAttached`, `NetworkNotAttached` | Unknown instance/snapshot/ref, or detaching a device that is not attached. |
| `UNIMPLEMENTED` | `NoBackendRegistered`, `Backend(NotImplemented)` | Backend kind not available. |
| `FAILED_PRECONDITION` | `InvalidTransition`, `InstanceNotRemovable`, `InstanceNotClonable`, `InstanceAlreadyStopped`, `SharedBaseNotSupportedForLinuxVm`, `InstanceHasLiveClones`, `SnapshotOperationRequiresRunningInstance`, `SnapshotLimitExceeded`, `GuestAgentUnavailable`, `NotAndroid`, `InstanceMustBeStopped`, `HotplugRequiresRunningInstance`, `Backend(HandleNotFound)`, `Backend(ProcessNotRunning)` | Wrong lifecycle state, resource limit, guest agent unavailable, wrong instance kind. |
| `ALREADY_EXISTS` | `SnapshotAlreadyExists`, `DiskAlreadyAttached` | Duplicate snapshot tag, or attaching a disk image that is already attached (including the primary disk). |
| `INVALID_ARGUMENT` | `ConvertError`, `EmptyInstanceRef`, `MalformedInstanceRef`, `AmbiguousInstanceId`, `ConfigIdMismatch`, `ConfigKindChanged`, `ConfigDiskPathChanged`, `InvalidConfig`, `InvalidConfigKey`, `MissingOvmfVarsTemplate` | Malformed request or invalid arguments |
| `RESOURCE_EXHAUSTED` | `InsufficientDiskSpace` | Not enough free space for a snapshot operation |
| `INTERNAL` | Other `Backend`/`Disk`/`Io`/`Store`/`Firmware` errors — including all `andler-disk` errors except `InsufficientDiskSpace` (`AgentNotInstalled`, `AgentAlreadyInstalled`, `PackageManagerNotFound`, ...) | Backend/disk/store failures |

**Message truncation**: every error passes through `status_message()` before becoming a `grpc-message` header — control characters other than tab are replaced with spaces, and the message is truncated to 384 characters. Package-manager stderr can be multi-KB; sending it untruncated makes h2 clients fail with "h2 protocol error" instead of showing the real error.

---

## Design Notes

- **`CdromBus`** must be set explicitly in requests — `UNSPECIFIED` is rejected with `MissingField` instead of a silent default; only *requests* must name the bus.
- **`NetworkConfig.nat_backend`** defaults to `SLIRP` for backward compatibility.
- **`InputConfig.tablet_mode`** is deprecated; `pointer_mode` should be used instead.
- **`CreateInstanceRequest`** omits `id` and `backend` because they are generated by the daemon; `UpdateInstanceConfigRequest` mirrors `GetInstanceConfigResponse` to allow a "get-edit-put" workflow. `UpdateInstanceConfig` additionally requires the instance to be stopped (`InstanceMustBeStopped` otherwise) and never mutates `id`/`kind`/`disk.path` (see Error Codes).
- **`CreateAndroidInstanceRequest`** with an empty `base_image_path` triggers daemon-side auto-discovery: the freshest matching image in `~/.andler/cache/base-images/` (flat root or `android<version>-<variant>/` subdirectories, by `android_major` + variant) is used; no match → `NOT_FOUND` with the exact `docker/images/build.sh` invocation to produce one.
- **`RemoveInstanceRequest`** has a separate `purge` field to avoid dragging it into all other request types that use `InstanceIdRequest`.
- **`LogStreamSource`** includes `UNSPECIFIED` for consistency with other enums, though the server always fills it.
- **`ResourceMetricsResponse`** fields are all optional because metrics may be unavailable (e.g., GPU on non-AMD hardware).
- **`CloneInstanceRequest`** supports three modes with different cost/independence trade-offs.
- **`ExportInstanceDisk`** is a separate RPC from cloning because it does not create a new instance.
- **Attach/Detach** require the instance to be `RUNNING` or `PAUSED` (`HotplugRequiresRunningInstance` → `FAILED_PRECONDITION` otherwise). Attached devices are appended to `extra_disks`/`extra_networks`, persisted in `instance.toml`, and re-created from the command line at the next `StartInstance` — no `UpdateInstanceConfig` needed. Detach identifies disks by path and networks by list index; a detach never deletes the disk image file.

---

*This documentation is derived from the protocol buffer definition and is maintained in sync with the source.*
