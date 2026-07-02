//! Конфигурация сети инстанса.
//!
//! Источник истины — `scripts/start.sh`: `-nic user,model=virtio-net-pci`
//! (режим `Nat`/user-networking). `Bridge`/`Isolated` — варианты, на
//! которые ссылается §4.1 архитектурного плана, конкретная реализация
//! (настройка bridge-интерфейсов/nftables на хосте) — задача `andler-net`
//! и пока не реализована, см. README этого крейта.

use serde::{Deserialize, Serialize};

/// Реализация NAT — только применимо при `NetworkMode::Nat`, игнорируется
/// для `Bridge`/`Isolated`. Независимая ось от [`NetworkMode`]: обе
/// реализации дают гостю выход в интернет через хост без дополнительной
/// настройки, различие — в производительности/поддержке IPv6/безопасности.
/// См. PLAN.md, раздел "NAT — `passt` вместо классического SLIRP".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NatBackend {
    /// Классический встроенный в QEMU user-mode networking (`-nic
    /// user,...`). Не требует ничего, кроме самого QEMU — это режим
    /// `start.sh` и универсальный fallback. Ограничения: нет ICMP/ICMPv6,
    /// нет IPv6 port forwarding.
    Slirp,
    /// `passt` (`-netdev passt,... -device virtio-net-pci,...`) — более
    /// новая альтернатива SLIRP: выше производительность, полная
    /// поддержка IPv6, работает как непривилегированный демон вне
    /// процесса QEMU. Требует установленный бинарь `passt` на хосте —
    /// если не найден, ANDLER предлагает fallback на `Slirp`, а не
    /// падает молча (см. `andler-firmware::detect::network`).
    Passt,
}

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
    /// Только для `NetworkMode::Nat` — см. [`NatBackend`].
    /// `#[serde(default)]` — старые сериализованные конфиги без этого
    /// поля читаются как `Slirp` (поведение `start.sh` до появления
    /// `passt`-детекта), не падают на десериализации.
    #[serde(default = "default_nat_backend")]
    pub nat_backend: NatBackend,
}

fn default_nat_backend() -> NatBackend {
    NatBackend::Slirp
}

impl NetworkConfig {
    /// Конфигурация, соответствующая `start.sh`: user-mode NAT (SLIRP),
    /// `virtio-net-pci`. `passt` — не дефолт здесь: он выбирается только
    /// при явном auto-detect (`andler-firmware`) или явном выборе
    /// пользователя, а не как безусловный дефолт этого конструктора.
    pub fn reference_default() -> Self {
        NetworkConfig {
            mode: NetworkMode::Nat,
            device_model: "virtio-net-pci".to_string(),
            nat_backend: NatBackend::Slirp,
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
        assert_eq!(cfg.nat_backend, NatBackend::Slirp);
    }

    #[test]
    fn nat_backend_deserializes_with_default_when_missing() {
        let json = r#"{"mode":"Nat","device_model":"virtio-net-pci"}"#;
        let cfg: NetworkConfig = serde_json::from_str(json).expect("must deserialize");
        assert_eq!(cfg.nat_backend, NatBackend::Slirp);
    }
}
