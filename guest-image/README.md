# guest-image

**Не начато.** Пайплайны сборки гостевых Linux-образов с Waydroid (Уровень A).
Не Rust-код — отдельные shell/CI-пайплайны.

Начинать после того, как `andler-core` + `andler-qemu` стабильно запускают
обычный `InstanceKind::LinuxVm` с 3D-ускорением через `andler-cli` — Android-
инстанс на этом этапе превращается просто в подмену диска (см. §4.4
`docs/architecture/CORE_ARCHITECTURE_PLAN.md`).

Полный план: `docs/architecture/GUEST_IMAGE_PLAN.md`. Для MVP — сократить
матрицу сборки до одной комбинации (см. рекомендацию по сокращению скоупа).
