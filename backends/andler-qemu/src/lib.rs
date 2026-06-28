//! Реализация `HypervisorBackend` (из `andler-core`) поверх процесса QEMU.
//!
//! - `cmdline` — чистая сборка аргументов командной строки из `InstanceConfig`.
//! - `process` — низкоуровневый spawn/is_alive/terminate/force_kill процесса
//!   QEMU, без знания про `InstanceConfig`/`HypervisorBackend`.
//! - `qmp` — клиент QEMU Machine Protocol: handshake, `pause`/`resume`/
//!   `query_status`. Snapshot — за пределами текущего скоупа, см. README.md
//!   этой папки.
//! - `backend` — `QemuBackend`, связывающий `cmdline`+`process`+`qmp` с
//!   `HypervisorBackend`.

pub mod backend;
pub mod cmdline;
pub mod gpu_metrics;
pub mod metrics;
pub mod process;
pub mod qmp;

pub use backend::QemuBackend;
pub use cmdline::build_args;
pub use process::{ProcessError, QemuProcess};
pub use qmp::{QmpClient, QmpError, VmStatus};

