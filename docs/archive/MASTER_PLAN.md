# ANDLER — Общий план разработки проекта (мастер-план)

> Это верхнеуровневый план **всего проекта**: из каких частей он состоит, на чём писать каждую часть, как всё это собирается в дистрибутивы (AppImage/deb/rpm и т.д.). Подробности по конкретной части смотрите в соответствующем документе:
> - Продуктовая концепция и весь функционал → `PRODUCT_CONCEPT.md`
> - Подробная архитектура **core** (структура крейтов, backend-абстракция QEMU/rust-vmm, тесты, Docker) → `CORE_ARCHITECTURE_PLAN.md`
> - Этот документ — общая карта проекта и то, что не покрыто двумя предыдущими: frontend, упаковка, дистрибуция, репозиторий целиком.

---

## 1. Из каких частей состоит проект

```
ANDLER
├── core           — демон + CLI, вся логика виртуализации, дисков, сети, ресурсов (Rust)
├── frontend        — графический менеджер инстансов, GUI-клиент над core (см. FRONTEND_PLAN.md)
├── guest-image     — сборка гостевых образов (минимальный Linux + Android-слой)
├── presets  — каталог пресетов под игры/приложения (ANDLER Presets, см. концепцию)
├── companion-app   — мобильное приложение-компаньон (гироскоп, см. концепцию)
└── packaging       — сборка дистрибутивов: AppImage, .deb, .rpm, Flatpak, AUR
```

Это пять-шесть отдельных, слабо связанных частей репозитория, которые общаются между собой только через чётко определённые интерфейсы (gRPC API core, формат образов guest-image, формат пресетов). Это намеренно — каждая часть может разрабатываться, тестироваться и даже релизиться отдельным циклом.

### 1.1 Что в этом плане расписано подробно, а что — только обзорно
- **Core** — только ссылка на отдельный документ, здесь не повторяется.
- **Frontend** — ссылка на отдельный документ, здесь не повторяется.
- **Guest-image, proton-presets, companion-app** — даны как обзорные блоки (они и так описаны в продуктовой концепции функционально); подробные техпланы по ним будут отдельными документами позже, по мере приближения к их реализации.
- **Packaging/дистрибуция** — расписана подробно, так как это сквозной вопрос для всего проекта.

---

## 2. Структура монорепозитория

Рекомендация — **один монорепозиторий**, а не разные репо на каждую часть. Причины: core и frontend версионируются и релизятся синхронно (frontend жёстко зависит от gRPC-схемы core), а guest-image/presets развиваются медленнее и могут даже жить в отдельных репо-сабмодулях каталога (см. продуктовую концепцию: "каталог образов сообщества" — это по сути отдельный git-репозиторий с контентом, не код).

```
andler/                          # монорепозиторий
├── core/                        # Rust workspace — см. CORE_ARCHITECTURE_PLAN.md
│   ├── crates/...
│   ├── tests/
│   └── docker/
├── frontend/                    # GUI-клиент (см. FRONTEND_PLAN.md)
│   ├── src/
│   ├── src-tauri/                 # если выбран Tauri — нативная обвязка
│   └── ...
├── guest-image/                 # пайплайн сборки гостевых образов (см. §4)
│   ├── build/
│   └── provisioning/
├── presets/               # пресеты игр — данные + схема валидации (см. §5)
│   ├── schema/
│   └── presets/
├── packaging/                    # дистрибутивы (см. §7)
│   ├── appimage/
│   ├── deb/
│   ├── rpm/
│   ├── flatpak/
│   └── aur/
├── docs/
│   ├── adr/                      # architecture decision records, сквозные по проекту
│   └── *.md                      # этот файл и остальные планы
└── .github/workflows/             # CI: отдельные джобы на core, frontend, packaging
```

---

## 3. Frontend — архитектурный план

Подробная архитектура frontend-части описана в отдельном документе:
- **FRONTEND_PLAN.md** — выбор технологий, структура, взаимодействие с core, видео-рендеринг, компоненты интерфейса, упаковка

Краткое описание:
- Frontend — графический менеджер инстансов, GUI-клиент над core
- Выбор: Tauri (Rust) + веб-фронтенд (TypeScript/React/Svelte)
- Видео-рендеринг через WebSocket/gRPC-стриминг
- Тонкий клиент над gRPC API core, без собственной логики виртуализации

---

## 4. Guest-image (обзорно — подробный план отдельным документом позже)

Краткое напоминание сути (полностью описано в дополнении к core-архитектуре):
- Заранее собранный (в CI), версионированный, подписанный минимальный гостевой Linux-образ с уже встроенными трансляторами архитектур и сетевым provisioning (nftables) — без `waydroid script`.
- Технологии сборки образа — не Rust-специфичны: обычно это shell/Makefile-пайплайн поверх существующих инструментов сборки образов (например, `mkosi`, `debootstrap`-подобные инструменты или Alpine `mkimage`), запускаемый в CI-контейнере.
- Результат — артефакт (`.qcow2`/`.img` шаблон), который скачивается core при первом создании Android-инстанса, а не собирается на машине пользователя.

Подробный технический план этой части — отдельный документ, когда дойдёт очередь (см. порядок реализации в core-плане, шаг 11).

---

## 5. ANDLER Presets (обзорно)

- По сути — структурированные данные (TOML/JSON) + JSON Schema для валидации, не код в традиционном смысле.
- Хранятся в репозитории как каталог файлов `presets/<game-id>.toml`, содержащих: маппинг клавиш, рекомендуемый рендер-backend, флаги транслятора, опциональные твики античит-совместимости.
- CLI/Frontend умеют просто применить пресет к `InstanceConfig` целиком или частично — это работа core (`andler-core`), не отдельный сервис.
- В будущем — отдельный небольшой веб-каталог сообщества для публикации пресетов (вне скоупа этого плана).

---

## 6. Упаковка и дистрибуция

### 6.1 Общий принцип
Core (`andlerd` + `andler`) и Frontend — **разные пакеты**, которые могут устанавливаться вместе или раздельно (например, headless-сервер ставит только core). Guest-images и ANDLER Presets — не часть установочного пакета, скачиваются демоном по запросу (иначе пакет распухнет до гигабайт).

### 6.2 AppImage
- Самый простой способ дать пользователю "скачал — запустил" без вопроса прав/зависимостей дистрибутива.
- Упаковывает frontend (Tauri-бинарь + WebView зависимости) и/или core в один файл.
- Важный нюанс: **QEMU как системная зависимость не входит в AppImage** (см. core-план, §13: вшивать QEMU не нужно и не стоит) — AppImage должен при первом запуске проверить наличие `qemu-system-x86_64` в системе и подсказать команду установки, если его нет, а не пытаться его упаковать.
- Инструмент сборки: `linuxdeploy` + `appimagetool` для Tauri-бинаря.

### 6.3 .deb (Debian/Ubuntu)
- Два пакета: `andler-core` (демон + CLI, с зависимостью `Depends: qemu-system-x86, qemu-utils`) и `andler-gui` (frontend, `Depends: andler-core`).
- Системный сервис `andlerd` — через systemd unit-файл, ставящийся пакетом `andler-core` (`/etc/systemd/system/andlerd.service`), с возможностью работать и как user-сервис (systemd `--user`) для desktop-сценария без root.
- Сборка через `cargo-deb` либо вручную через `dpkg-deb` в CI.

### 6.4 .rpm (Fedora/openSUSE и т.д.)
- Та же логика разделения на `andler-core`/`andler-gui` через `Requires: qemu-kvm`.
- Сборка через `cargo-generate-rpm` либо `.spec`-файл вручную в CI (`rpmbuild`).

### 6.5 Flatpak
- Для дистрибуции frontend конечным пользователям через Flathub — самый дружелюбный для обычного пользователя способ обновлений.
- Сложность: Flatpak-сэндбоксинг и доступ к `/dev/kvm`/системному QEMU нетривиален (нужны правильные `--device=kvm` permissions и доступ к D-Bus/сокету демона снаружи песочницы) — требует отдельного исследования перед релизом в этом формате; вероятно, в Flatpak имеет смысл паковать **только frontend**, а core ставить отдельно через deb/rpm/AppImage как системный компонент.

### 6.6 AUR (Arch/CachyOS — ваш собственный дистрибутив, логично иметь с первого дня)
- `PKGBUILD` для `andler-core` (собирает из исходников или берёт релизный бинарь) и `andler-gui`.
- Самый быстрый способ дать рабочий пакет для вашей же машины разработки и для Arch-сообщества, которое статистически много пересекается с целевой аудиторией продукта (NVIDIA + Linux + желание играть в мобильные игры).

### 6.7 Сводная таблица

| Формат | Что упаковывает | Системные зависимости пользователя | Приоритет |
|---|---|---|---|
| AUR (PKGBUILD) | core + gui (раздельные пакеты) | qemu, через pacman | Высокий (ваша платформа разработки) |
| .deb | core + gui (раздельные пакеты) | qemu-system-x86, qemu-utils | Высокий |
| .rpm | core + gui (раздельные пакеты) | qemu-kvm | Средний |
| AppImage | gui (+ опционально core для портативного запуска) | qemu — проверяется при первом запуске | Средний |
| Flatpak | только gui | требует отдельного решения по доступу к KVM/демону | Низкий/будущий |

### 6.8 Релизный пайплайн (CI)
Один общий CI-процесс при тэге релиза:
1. Сборка и тест core (см. core-план, §15–16).
2. Сборка frontend (Tauri build для Linux-таргета).
3. Параллельные джобы упаковки: AUR `PKGBUILD` (обновление через `makepkg --printsrcinfo`), `.deb` через `cargo-deb`, `.rpm` через `cargo-generate-rpm`, AppImage через `linuxdeploy`.
4. Публикация артефактов в GitHub Releases + (опционально) собственный APT/DNF-репозиторий и AUR-git push.

---

## 7. Сводная таблица технологий по частям проекта

| Часть | Язык/стек | Статус плана |
|---|---|---|
| Core (демон, CLI) | Rust (workspace) | Подробно — `ANDLER_CORE_ARCHITECTURE_PLAN.md` |
| Frontend (GUI) | Rust (Tauri) + TypeScript (React/Svelte) | Подробно — `ANDLER_FRONTEND_PLAN.md` |
| Guest-image | shell/Makefile + образ-тулинг (mkosi/debootstrap-подобное) | Обзорно — этот документ, §4; подробный план позже |
| Proton-presets | TOML/JSON + JSON Schema (данные, не код) | Обзорно — этот документ, §5 |
| Companion-app | Kotlin (нативный Android) или Flutter — решение позже | Обзорно — этот документ, §6 |
| Packaging | AUR/PKGBUILD, cargo-deb, cargo-generate-rpm, AppImage-тулинг, (Flatpak позже) | Подробно — этот документ, §7 |

---

## 8. Порядок работы над проектом в целом (верхний уровень)

1. **Core** — по детальному плану в `ANDLER_CORE_ARCHITECTURE_PLAN.md`, шаги 1–10 (до рабочего demo через CLI на Linux-VM).
2. **AUR-пакет для core+CLI** — сразу после первого рабочего демона, чтобы у вас и ранних тестеров был простой способ установки без сборки руками.
3. **Guest-image пайплайн** (core-план, шаг 11) — параллельно или сразу после.
4. **Frontend MVP** (Tauri) — список инстансов, мастер создания, базовые действия (start/stop/pause), живой просмотр экрана — после того, как core стабилен и gRPC API не меняется на каждый коммит.
5. **.deb/.rpm пакеты** — когда core+frontend MVP достаточно стабильны для пользователей вне Arch-экосистемы.
6. **AppImage** — для самого широкого охвата "просто скачал и запустил".
7. **Proton-presets** как формат и пара примеров пресетов — параллельно с frontend MVP, не блокирует ничего.
8. **Companion-app** и **Уровень B guest-image** (свой android-runner) — поздние этапы, после того как основной продукт уже используем.
9. **Flatpak** и публикация в более широких каталогах — после накопления стабильности и отзывов с этапов 2–6.

---

## 9 Открытые вопросы этого уровня плана

- Tauri vs egui — финальное решение лучше подтвердить коротким прототипом (живой видеопоток в WebKitGTK-webview) до начала серьёзной разработки frontend, см. `ANDLER_FRONTEND_PLAN.md`
- React vs Svelte (если Tauri) — не принципиально для архитектуры, можно решить по предпочтениям контрибьюторов на старте, см. `ANDLER_FRONTEND_PLAN.md`
- Формат и протокол companion-app (UDP/WebSocket, авторизация по локальной сети) — отдельный мини-план перед началом этой части.
- Нужен ли собственный APT/DNF-репозиторий с самого начала, или на старте достаточно GitHub Releases + AUR — влияет на пункт 7.8, но не блокирует core/frontend разработку.

---

## 10. Рабочая конфигурация (на основе start.sh)

### 10.1 Полный скрипт запуска (start.sh)

```bash
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(dirname "$(readlink -f "$0")")"

VM_NAME="linux"
RAM_SIZE="8G"
VRAM_SIZE="4096M"
DISK_SIZE="40G"

DISK_IMG="$SCRIPT_DIR/disk.qcow2"
ISO_IMG="$SCRIPT_DIR/cachyos-desktop-linux-260426.iso"

OVMF_CODE="/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd"
OVMF_VARS_TEMPLATE="/usr/share/edk2-ovmf/x64/OVMF_VARS.4m.fd"
OVMF_VARS="$SCRIPT_DIR/${VM_NAME}_VARS.fd"

RESET_BOOT=false
ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --reset-boot) RESET_BOOT=true; shift ;;
        *) ARGS+=("$1"); shift ;;
    esac
done

if [ ! -f "$OVMF_CODE" ]; then
    exit 1
fi

if [ "$RESET_BOOT" = true ] && [ -f "$OVMF_VARS" ]; then
    rm -f "$OVMF_VARS"
fi

if [ ! -f "$OVMF_VARS" ]; then
    cp "$OVMF_VARS_TEMPLATE" "$OVMF_VARS"
fi

if [ ! -f "$DISK_IMG" ]; then
    qemu-img create -f qcow2 "$DISK_IMG" "$DISK_SIZE"
fi

exec qemu-system-x86_64 \
  -name "$VM_NAME",process="$VM_NAME" \
  -machine q35,accel=kvm,usb=on \
  -cpu host,kvm=on,+topoext,migratable=no \
  -smp cpus=4,sockets=1,dies=1,cores=4,threads=1 \
  -m "$RAM_SIZE" \
  -object memory-backend-memfd,id=mem1,size="$RAM_SIZE",share=on \
  -machine memory-backend=mem1 \
  -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
  -drive if=pflash,format=raw,file="$OVMF_VARS" \
  -vga none \
  -device virtio-gpu-gl,hostmem="$VRAM_SIZE",blob=true,venus=true \
  -display sdl,gl=on,show-cursor=off \
  -drive file="$DISK_IMG",format=qcow2,if=none,id=drive-disk0,discard=on,detect-zeroes=on,aio=threads \
  -device virtio-blk-pci,drive=drive-disk0,id=disk0,bootindex=1,num-queues=4 \
  -drive file="$ISO_IMG",media=cdrom,if=none,id=drive-cd0 \
  -device ide-cd,drive=drive-cd0,id=cd0,bootindex=2 \
  -device virtio-tablet-pci,id=tablet0 \
  -device virtio-serial-pci \
  -device virtserialport,chardev=ch1,id=ch1,name=com.redhat.spice.0 \
  -chardev qemu-vdagent,id=ch1,name=vdagent,clipboard=on,mouse=on \
  -nic user,model=virtio-net-pci \
  -audiodev pipewire,id=snd0 \
  -device ich9-intel-hda -device hda-output,audiodev=snd0 \
  -boot menu=on "${ARGS[@]}"
```

### 10.2 Ключевые параметры запуска

**Базовая конфигурация:**
```bash
VM_NAME="linux"
RAM_SIZE="8G"
VRAM_SIZE="4096M"
DISK_SIZE="40G"
```

**GPU-конфигурация (Venus):**
```bash
-device virtio-gpu-gl,hostmem="$VRAM_SIZE",blob=true,venus=true
-display sdl,gl=on,show-cursor=off
```

**CPU и память:**
```bash
-cpu host,kvm=on,+topoext,migratable=no
-smp cpus=4,sockets=1,dies=1,cores=4,threads=1
-object memory-backend-memfd,id=mem1,size="$RAM_SIZE",share=on
-machine memory-backend=mem1
```

**Диск:**
```bash
-drive file="$DISK_IMG",format=qcow2,if=none,id=drive-disk0,discard=on,detect-zeroes=on,aio=threads
-device virtio-blk-pci,drive=drive-disk0,id=disk0,bootindex=1,num-queues=4
```

**Сеть и аудио:**
```bash
-nic user,model=virtio-net-pci
-audiodev pipewire,id=snd0
-device ich9-intel-hda -device hda-output,audiodev=snd0
```

**Ввод:**
```bash
-device virtio-tablet-pci,id=tablet0
-device virtio-serial-pci
-device virtserialport,chardev=ch1,id=ch1,name=com.redhat.spice.0
-chardev qemu-vdagent,id=ch1,name=vdagent,clipboard=on,mouse=on
```

### 10.3 Управление запуском

**Reset boot:**
```bash
--reset-boot  # Удаляет OVMF_VARS для сброса UEFI-настроек
```

**Управление памятью:**
- `memory-backend-memfd` — разделяемая память для KSM
- `share=on` — позволяет объединять одинаковые страницы между инстансами

**Управление диском:**
- `discard=on` — поддержка TRIM для эффективной очистки
- `detect-zeroes=on` — оптимизация записи нулей
- `aio=threads` — потоковый ввод-вывод для производительности

### 10.4 Рекомендации по конфигурации

**Для игр:**
- RAM: 8G минимум, 16G для комфортной работы
- VRAM: 4096M для Venus-контекста
- CPU: 4 ядра минимум, 8 для тяжёлых игр
- Disk: 40G+ для Android-инстансов

**Для разработки:**
- RAM: 4G достаточно
- VRAM: 2048M для VirtIO-GPU
- CPU: 2-4 ядра
- Disk: 20G достаточно

**Для headless-сервера:**
- RAM: 2G минимум
- VRAM: 0 (CPU-рендеринг)
- CPU: 2 ядра
- Disk: 10G достаточно

---

## 11. Связанные документы

- **ANDLER_CORE_ARCHITECTURE_PLAN.md** — детальная архитектура core, gRPC API, backend'ы
- **ANDLER_FRONTEND_PLAN.md** — архитектура frontend, видео-рендеринг, компоненты
- **ANDLER_PRODUCT_CONCEPT.md** — продуктовая концепция, функционал, пользовательские сценарии
