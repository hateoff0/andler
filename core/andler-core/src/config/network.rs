//! Конфигурация сети инстанса.
//!
//! Источник истины — `scripts/start.sh`: `-nic user,model=virtio-net-pci`
//! (режим `Nat`/user-networking). `Bridge`/`Isolated` — варианты, на
//! которые ссылается §4.1 архитектурного плана, конкретная реализация
//! (настройка bridge-интерфейсов/nftables на хосте) — задача `andler-net`
//! и пока не реализована, см. README этого крейта.

use serde::{Deserialize, Serialize};

/// Режим сети инстанса.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkMode {
    /// QEMU user-mode networking (`-nic user,...`) — NAT силами самого
    /// QEMU, без участия хоста. Простой, не требует прав на хосте,
    /// но с ограничениями (например, не все протоколы проксируются).
    /// Это режим `start.sh`.
    Nat,
    /// Подключение к существующему bridge-интерфейсу на хосте — даёт
    /// инстансу собственный IP в локальной сети хоста. Требует
    /// предварительной настройки bridge'а (вне ответственности
    /// `andler-core`).
    Bridge { interface: String },
    /// Сеть полностью отключена — инстанс не имеет доступа ни к хосту,
    /// ни к внешней сети.
    Isolated,
}

/// Конфигурация сети инстанса.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub mode: NetworkMode,
    /// Модель virtio-устройства сети (`model=virtio-net-pci` в
    /// `start.sh`). Декларативное поле — нет смысла заводить под это
    /// отдельный enum на текущем этапе, так как альтернатив
    /// `virtio-net-pci` в плане не предусмотрено.
    pub device_model: String,
}

impl NetworkConfig {
    /// Конфигурация, соответствующая `start.sh`: user-mode NAT,
    /// `virtio-net-pci`.
    pub fn reference_default() -> Self {
        NetworkConfig {
            mode: NetworkMode::Nat,
            device_model: "virtio-net-pci".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_default_matches_start_sh() {
        let cfg = NetworkConfig::reference_default();
        assert_eq!(cfg.mode, NetworkMode::Nat);
        assert_eq!(cfg.device_model, "virtio-net-pci");
    }
}
