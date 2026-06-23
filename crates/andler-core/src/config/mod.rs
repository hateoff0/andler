//! Конфигурация инстанса: `InstanceConfig` и все составляющие его типы.
//!
//! Разнесено по файлам так же, как описано в
//! docs/architecture/CORE_ARCHITECTURE_PLAN.md, §4.3:
//! `cpu.rs`, `memory.rs`, `gpu.rs`, `disk.rs`, `network.rs`, `display.rs` —
//! по одному файлу на каждый блок ресурсов. `firmware.rs`, `audio.rs`,
//! `input.rs` добавлены дополнительно при реализации `andler-qemu::cmdline`
//! — `start.sh` содержит OVMF/audio/clipboard-параметры, для которых план
//! не выделял отдельный тип в §4.3. `instance.rs` собирает всё в единый
//! `InstanceConfig` вместе с `InstanceId`/`InstanceKind`.

mod audio;
mod cpu;
mod disk;
mod display;
mod firmware;
mod gpu;
mod input;
mod instance;
mod memory;
mod network;

pub use audio::{AudioBackend, AudioConfig};
pub use cpu::{CpuConfig, CpuPriority};
pub use disk::{DiskConfig, DiskFormat};
pub use display::{DisplayConfig, DisplayEngine, Resolution};
pub use firmware::FirmwareConfig;
pub use gpu::{GpuConfig, RenderBackend};
pub use input::InputConfig;
pub use instance::{BackendKind, InstanceConfig, InstanceId, InstanceKind};
pub use memory::MemoryConfig;
pub use network::{NetworkConfig, NetworkMode};
