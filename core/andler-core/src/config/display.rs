//! Конфигурация дисплея инстанса.
//!
//! Источник истины — `scripts/start.sh`: `-display sdl,gl=on,show-cursor=off`.
//! `Spice`/`Dbus` — варианты `DisplayEngine` из §4.3 архитектурного плана,
//! нужны в первую очередь для стриминга в GUI-клиент (`frontend/`, см. его
//! README про риск инпут-лага) — не используются текущим CLI-путём.

use serde::{Deserialize, Serialize};

/// Разрешение экрана инстанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub const fn new(width: u32, height: u32) -> Self {
        Resolution { width, height }
    }
}

/// Способ вывода изображения наружу из QEMU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisplayEngine {
    /// `-display sdl,...` — прямое окно SDL на хосте. Подходит для CLI
    /// и раннего тестирования; не подходит для GUI-стриминга на удалённый
    /// клиент. Дефолт на NVIDIA-хостах — `-display gtk,gl=on` на части
    /// NVIDIA-конфигураций даёт чёрный экран, тогда как SDL там же
    /// работает (см. `DisplayEngine::Gtk`).
    Sdl,
    /// `-display gtk,...` — окно со встроенным меню/UI (снапшоты, монитор
    /// и т.д. прямо из интерфейса QEMU), собственный `clipboard=on`-параметр
    /// без необходимости в vdagent. Дефолт для desktop-хостов, КРОМЕ
    /// NVIDIA — подтверждённая на практике проблема: `gtk,gl=on` даёт
    /// чёрный экран на части NVIDIA-конфигураций (см. PLAN.md, раздел
    /// "Настройки дисплея"). Выбор между `Sdl`/`Gtk` — по GPU-вендору
    /// хоста, не статичный дефолт.
    Gtk,
    /// SPICE-сервер — поток, который может потреблять GUI-клиент
    /// (см. риск инпут-лага в `frontend/README.md`).
    Spice,
    /// Вывод через D-Bus (для интеграции с десктопным окружением хоста,
    /// например встраивание окна инстанса в композитор) — наименее
    /// проработанный вариант на данный момент.
    Dbus,
    /// `-display none` — без вывода вообще, без обращения к какому-либо
    /// X11/Wayland-серверу хоста. Нужен для headless-сценариев: серверы
    /// без дисплея, CI/Docker-окружения для smoke-проверок (см.
    /// `docker/e2e_smoke.sh`, где раньше для `DisplayEngine::Sdl`
    /// требовался виртуальный `Xvfb` только чтобы у SDL было куда
    /// присоединиться — с `None` эта зависимость отсутствует). Не
    /// тождественен `Spice`/`Dbus`: те создают поток/канал для удалённого
    /// клиента, `None` не создаёт вообще никакого визуального вывода.
    None,
}

/// Конфигурация дисплея инстанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayConfig {
    /// Хранится и сериализуется, но пока **не влияет** на реальный вывод
    /// для `DisplayEngine::Sdl`/`Gtk` — QEMU не принимает `width=`/
    /// `height=` в строке `-display sdl,...`/`gtk,...`, разрешение
    /// зависит от гостя (его собственные настройки экрана) или от
    /// EDID-инъекции на уровне `virtio-gpu`, которая пока не
    /// реализована. См. PLAN.md, item 4, "Cannot pre-set resolution in
    /// SDL" — до её реализации это поле работает только как
    /// пожелание/документация выбора пользователя, не как гарантия.
    pub resolution: Resolution,
    pub dpi: u32,
    /// `0` означает "без ограничения" (`unlimited` в терминах §4.3 плана) —
    /// отдельного варианта enum не вводим, чтобы не усложнять сравнения и
    /// сериализацию для частого случая "обычное число".
    pub fps_limit: u32,
    pub display_engine: DisplayEngine,
    pub fullscreen: bool,
}

impl DisplayConfig {
    /// Без ограничения FPS — см. описание поля `fps_limit`.
    pub const FPS_UNLIMITED: u32 = 0;

    /// Конфигурация, соответствующая `start.sh`: SDL с OpenGL,
    /// без явного разрешения/DPI/FPS-лимита в самом скрипте — поэтому
    /// здесь взяты практичные дефолты (1920x1080, 96 DPI, без лимита FPS),
    /// а не значения, прямо вычитанные из `start.sh` (он этот вопрос не
    /// решает явно).
    pub fn reference_default() -> Self {
        DisplayConfig {
            resolution: Resolution::new(1920, 1080),
            dpi: 96,
            fps_limit: Self::FPS_UNLIMITED,
            display_engine: DisplayEngine::Sdl,
            fullscreen: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_default_uses_sdl() {
        let cfg = DisplayConfig::reference_default();
        assert_eq!(cfg.display_engine, DisplayEngine::Sdl);
        assert_eq!(cfg.fps_limit, DisplayConfig::FPS_UNLIMITED);
        assert!(!cfg.fullscreen);
    }

    #[test]
    fn none_display_engine_round_trips_through_serde_json() {
        // DisplayEngine::None — добавлен для headless-сценариев (см. его
        // docstring) после того, как остальные варианты уже были
        // покрыты косвенно через config_round_trips_through_serde_json
        // в instance.rs; отдельный тест здесь явно фиксирует, что новый
        // вариант не сломал (де)сериализацию.
        let cfg = DisplayConfig {
            display_engine: DisplayEngine::None,
            ..DisplayConfig::reference_default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: DisplayConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }
}
