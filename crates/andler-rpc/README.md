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
- `ListInstances` — соответствует `Daemon::list_instances`. Возвращает
  только `instance_id`/`name`/грубое `InstanceStateKind` на запись
  (`InstanceListEntry`), не полный `InstanceConfig` и не live
  backend-статус — см. комментарий у `Daemon::list_instances` за тем,
  почему список не дёргает `backend.status()` по каждому инстансу.
- `RemoveInstance` — соответствует `Daemon::remove_instance`. Отвергает
  запросы для нетерминальных состояний (`Starting`/`Running`/`Paused`/
  `Stopping`) как `FAILED_PRECONDITION` — не останавливает инстанс
  сама, требует явного `StopInstance` сначала. По умолчанию (`purge:
  false` в `RemoveInstanceRequest`) не удаляет файлы инстанса с диска;
  `purge: true` дополнительно удаляет `disk.path` и
  `firmware.ovmf_vars_path` (никогда `base_image`/`ovmf_code_path` —
  они общие для нескольких инстансов) — см. подробное обоснование в
  комментарии у `Daemon::remove_instance`.
- `GetInstanceConfig` — соответствует `Daemon::get_instance_config`.
  Возвращает полный `InstanceConfig` целиком (все 9 секций + `id`/
  `backend`/`kind`), в отличие от `ListInstances` — точечный запрос по
  одному `InstanceId`, не сводка по всем. Конвертация
  `InstanceConfig -> GetInstanceConfigResponse` — `From`, не `TryFrom`
  (доменный тип уже полон, конвертация в proto не может провалиться) —
  в отличие от направления `CreateInstanceRequest -> InstanceConfig`.

**Чего здесь нет и почему:**

- `CloneInstance`/`StreamInstanceLogs`/`StreamResourceMetrics` из §5.1 —
  у `Daemon` пока нет соответствующих методов. Объявлять rpc-метод
  раньше метода `Daemon`, который он должен вызывать, означало бы
  проектировать протокол вслепую — ровно то, чего избегали при выборе
  порядка `daemon.rs` перед `andler-rpc` изначально.

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
