# packaging

**Не начато.** Скрипты дистрибуции: AUR (`PKGBUILD`), `.deb` (`cargo-deb`),
`.rpm` (`cargo-generate-rpm`), AppImage (`linuxdeploy`).

Не начинать до стабильной связки `andler-core` + CLI + хотя бы одного формата
пакета (рекомендация: начать с AUR как самого простого для Arch-based
дистрибутивов, на которых разработка скорее всего и ведётся).

Полный план: `docs/architecture/PACKAGING_PLAN.md`. Важно зафиксировать здесь
шаг проверки/добавления пользователя в группу `kvm` post-install — см. §10
`CORE_ARCHITECTURE_PLAN.md`.
