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
- `clone.rs` — **реализовано**. Три режима клонирования диска *существующего
  инстанса* в новый диск (не от общего `base_image`, как `overlay.rs` — отдельный
  модуль именно из-за этого различия в семантике backing chain, см. модульную
  документацию `clone.rs` за полным обоснованием): `linked_clone` (overlay с
  `backing_file` = диск источника — дёшево, но создаёт зависимость от него),
  `full_standalone_clone` (тонкая обёртка над `qcow2::clone_full` — разворачивает
  всю backing chain в самостоятельный файл), `shared_base_clone` (побайтовая копия
  самого overlay-файла источника — `tokio::fs::copy`, не `qemu-img`, остаётся
  тонким относительно общего `base_image`, но не зависит от источника физически).
  Используется `Daemon::clone_instance`/`Daemon::export_instance_disk`
  (`andler-daemon`) — см. `andler_core::CloneMode` за доменным именем каждого
  режима.

## Что здесь НЕ реализовано

- Provisioning root/Magisk перед первым запуском гостя (offline-монтирование overlay,
  копирование модулей) — требует работы с loop-устройствами или аналогичного доступа
  внутрь файловой системы образа, заведомо отдельная задача. `create_overlay` сейчас
  создаёт overlay, готовый только для `RootMode::None`.

## Тесты

- Без `qemu-img`/`/dev/kvm`: парсинг JSON-вывода `qemu-img info` (текстовый экстрактор
  полей в `qcow2.rs`, без полноценного JSON-парсера — см. документацию
  `virtual_size_bytes`), `clone.rs::shared_base_clone_reports_missing_source_as_io_error`
  (не требует `qemu-img` — `tokio::fs::copy` сам возвращает `ENOENT` до вызова
  любого внешнего процесса).
- С `qemu-img` (требует `qemu-utils` в окружении — `integration-test` Docker-таргет,
  не `unit-test`): все операции с реальными файлами, помечены `#[ignore]` с указанием
  причины — включая по одному тесту на каждый режим `clone.rs`
  (`linked_clone_points_at_source_instance_disk`/
  `full_standalone_clone_has_no_backing_file`/
  `shared_base_clone_does_not_depend_on_source_after_copy`, последний явно удаляет
  каталог источника после копирования и проверяет, что клон остался читаемым). См.
  `docker/README.md`.

## Связанная документация

`docs/architecture/CORE_ARCHITECTURE_PLAN.md`, §4.4.4 (таблица ответственности по
крейтам), `docs/adr/0001-overlay-disk-for-android-instances.md`.
