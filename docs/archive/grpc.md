# gRPC API (andler-rpc)

Человекочитаемое описание методов `AndlerService` — источник истины по
полям и типам остаётся `crates/andler-rpc/proto/andler.proto`, этот файл
только объясняет смысл и держится в синхроне при изменении `.proto`.

Текущий набор — намеренное подмножество §5.1
`docs/architecture/CORE_ARCHITECTURE_PLAN.md` (см. `crates/andler-rpc/README.md`
зачем остальное пока не объявлено).

## CreateAndroidInstance

Резолвит `AndroidProfile` в `InstanceConfig` и регистрирует инстанс —
gRPC-обёртка вокруг `Daemon::create_android_instance`. Создаёт на диске
каталог инстанса, overlay-диск (`andler-disk`) и личную копию `OVMF_VARS`
из переданного шаблона.

- `base_image_path`/`ovmf_vars_template` — пути на файловой системе **хоста,
  на котором работает `andlerd`**, не клиента. CLI и демон сейчас
  предполагаются на одной машине; если это перестанет быть так — нужен
  отдельный механизм передачи образов, не часть этого метода.
- `overlay_size_bytes` — размер overlay-диска, не базового образа.
- Возвращает `instance_id` (UUID), которым нужно сопровождать все
  следующие вызовы.

## StartInstance / PauseInstance / ResumeInstance / StopInstance

Принимают только `instance_id`. `StopInstance` дополнительно принимает
`graceful` — см. ограничение в `andler-qemu/README.md` про то, что
graceful shutdown пока не полноценный ACPI-сигнал.

Все четыре — gRPC-обёртки вокруг одноимённых методов `Daemon` без
дополнительной логики; ошибки транслируются в gRPC-статусы по правилам,
описанным в `crates/andler-daemon/src/service.rs` (`impl From<DaemonError>
for Status`).

## GetInstanceStatus

Возвращает `InstanceStateKind` (соответствует `andler_core::InstanceState`
один-к-одному) плюс `error_message` (заполнено только при `state == ERROR`)
и `detail` (диагностика backend'а, если есть — `BackendStatus::detail`).
