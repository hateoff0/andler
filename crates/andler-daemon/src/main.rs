//! Бинарник `andlerd`.
//!
//! На этом этапе `Daemon` (см. `daemon.rs`) не обёрнут в gRPC-сервис —
//! `andler-rpc`/сетевой слой ещё не реализованы (см.
//! docs/architecture/CORE_ARCHITECTURE_PLAN.md, §9). `main` здесь —
//! минимальный entrypoint, подтверждающий, что `Daemon` собирается и
//! инициализируется; полноценный долгоживущий процесс с gRPC-сервером
//! появится, когда будет готов `andler-rpc`.

mod daemon;

use daemon::Daemon;

#[tokio::main]
async fn main() {
    let _daemon = Daemon::new();
    println!(
        "andlerd: core daemon logic ready (Daemon + QemuBackend); \
         gRPC service not yet implemented, see crates/andler-rpc"
    );
}
