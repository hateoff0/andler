# andler-rpc

gRPC-схема (`proto/andler.proto`) и сгенерированный код сервера/клиента
(через `tonic`/`prost`, см. `build.rs`), плюс конвертации между
сгенерированными типами и доменными типами `andler-core` (`src/convert.rs`).
Используется одновременно `andler-daemon` (сервер) и `andler-cli` (клиент) —
единая точка правды по протоколу.

## Статус: намеренное подмножество §5.1 архитектурного плана

`andler.proto` объявляет только методы, для которых уже есть реализация в
`andler_daemon::Daemon`:

- `CreateInstance` — соответствует `Daemon::create_instance` для
  `InstanceKind::LinuxVm`. Принимает явный `InstanceConfig` целиком (все
  9 под-конфигураций: CPU/память/диск/дисплей/GPU/сеть/firmware/audio/
  input), без промежуточного резолва — в отличие от
  `CreateAndroidInstance`. Сознательно ограничен на `LinuxVm`, не
  `oneof kind` с веткой Android — см. подробное обоснование в комментарии
  у `rpc CreateInstance` в `andler.proto`.
- `CreateAndroidInstance` — соответствует `Daemon::create_android_instance`.
- `StartInstance`/`StopInstance`/`PauseInstance`/`ResumeInstance`/
  `GetInstanceStatus` — соответствуют одноимённым методам `Daemon`.

**Чего здесь нет и почему:**

- `CloneInstance`/`RemoveInstance`/`ListInstances`/`StreamInstanceLogs`/
  `StreamResourceMetrics` из §5.1 — у `Daemon` пока нет соответствующих
  методов. Объявлять rpc-метод раньше метода `Daemon`, который он должен
  вызывать, означало бы проектировать протокол вслепую — ровно то, чего
  избегали при выборе порядка `daemon.rs` перед `andler-rpc` изначально.

## Структура

- `proto/andler.proto` — схема.
- `build.rs` — компилирует `.proto` в `OUT_DIR` через `tonic_build::compile_protos`.
  Требует системный `protoc` (пакет `protobuf-compiler` в окружении сборки,
  см. `docker/Dockerfile.dev`, стадия `builder`).
- `src/lib.rs` — `pub mod proto { tonic::include_proto!("andler"); }`.
- `src/convert.rs` — `TryFrom`/`From` между proto-сообщениями и доменными
  типами `andler-core`: `AndroidProfile`/`AndroidVersion`/`RootMode`, и
  каждый под-тип `InstanceConfig` (`CpuConfig`, `MemoryConfig`,
  `DiskConfig`, `DisplayConfig`/`Resolution`, `GpuConfig`/`RenderBackend`,
  `NetworkConfig`/`NetworkMode`, `FirmwareConfig`, `AudioConfig`,
  `InputConfig`) для `CreateInstanceRequest`. `RenderBackend`/`NetworkMode`
  — `oneof` в proto (не C-style enum), так как их доменные эквиваленты
  несут данные в отдельных вариантах (`Passthrough { gpu_pci_id }`,
  `Bridge { interface }`). Плюс `parse_instance_id` (строка из
  gRPC-запроса -> `InstanceId`) и `instance_state_to_proto`
  (`InstanceState` -> `(InstanceStateKind, error_message)`).

## Версионирование

Открытый вопрос политики совместимости `.proto`-схемы зафиксирован в
`docs/architecture/CORE_ARCHITECTURE_PLAN.md`, §10 — до его решения избегайте
breaking changes полей без крайней необходимости (переименование/удаление
существующих полей, смена номеров полей).

## Человекочитаемое описание методов

См. `docs/api/grpc.md` — держите его в синхроне при изменении `.proto`
(сейчас файл ещё описывает целевой набор §5.1 целиком — нужно обновить, чтобы
явно отделить реализованное подмножество от плана).
