# andler-disk

Работа с дисками виртуальных машин: создание, клонирование, изменение размера,
сжатие — обёртка над `qemu-img`, плюс отдельная логика overlay-дисков для
Android-инстансов.

## Что здесь есть

- `qcow2.rs` — **реализовано**. `create`/`create_with_backing_file`/`clone_full`/
  `resize`/`compact`/`virtual_size_bytes`/`disk_usage_bytes`, все асинхронные, через
  `tokio::process::Command` + `qemu-img`. Не зависит от `andler-core` — работает с
  `&Path` напрямую, ничего не знает про `InstanceConfig`.
- `overlay.rs` — **реализовано**. `create_overlay`/`factory_reset` — доменно-осмысленный
  слой поверх `qcow2::create_with_backing_file` специально для Android-инстансов
  (см. §4.4.2 архитектурного плана и ADR 0001). `factory_reset` — удаление + повторное
  создание overlay, не "очистка" существующего файла.

## Что здесь НЕ реализовано

- Provisioning root/Magisk перед первым запуском гостя (offline-монтирование overlay,
  копирование модулей) — требует работы с loop-устройствами или аналогичного доступа
  внутрь файловой системы образа, заведомо отдельная задача. `create_overlay` сейчас
  создаёт overlay, готовый только для `RootMode::None`.

## Тесты

- Без `qemu-img`/`/dev/kvm`: парсинг JSON-вывода `qemu-img info` (текстовый экстрактор
  полей в `qcow2.rs`, без полноценного JSON-парсера — см. документацию
  `virtual_size_bytes`).
- С `qemu-img` (требует `qemu-utils` в окружении — `integration-test` Docker-таргет,
  не `unit-test`): все операции с реальными файлами, помечены `#[ignore]` с указанием
  причины. См. `docker/README.md`.

## Связанная документация

`docs/architecture/CORE_ARCHITECTURE_PLAN.md`, §4.4.4 (таблица ответственности по
крейтам), `docs/adr/0001-overlay-disk-for-android-instances.md`.
