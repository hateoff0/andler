# andler-rpc

gRPC-схема (`proto/andler.proto`) и сгенерированный код сервера/клиента
(через `tonic`/`prost`, см. `build.rs`), плюс конвертации между
сгенерированными типами и доменными типами `andler-core` (`src/convert.rs`).
Используется одновременно `andler-daemon` (сервер) и `andler-cli` (клиент) —
единая точка правды по протоколу.

## Статус: намеренное подмножество §5.1 архитектурного плана

`andler.proto` объявляет только методы, для которых уже есть реализация в
`andler_daemon::Daemon`:

- `CreateAndroidInstance` — соответствует `Daemon::create_android_instance`.
- `StartInstance`/`StopInstance`/`PauseInstance`/`ResumeInstance`/
  `GetInstanceStatus` — соответствуют одноимённым методам `Daemon`.

**Чего здесь нет и почему:**

- `CreateInstance` для произвольного `InstanceConfig` (не через
  `AndroidProfile`, например `LinuxVm`) — сериализация всего
  `InstanceConfig` (CPU/память/диск/GPU/сеть/firmware/audio/input) через
  protobuf — отдельная по объёму задача, которую сознательно не стали
  делать вместе с первым gRPC-слоем.
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
- `src/convert.rs` — `TryFrom`/`From` между proto-сообщениями
  (`proto::AndroidProfile`, `proto::AndroidVersion`, `proto::RootMode`) и
  `andler_core::{AndroidProfile, AndroidVersion, RootMode}`, плюс
  `parse_instance_id` (строка из gRPC-запроса -> `InstanceId`) и
  `instance_state_to_proto` (`InstanceState` -> `(InstanceStateKind, error_message)`).

## Версионирование

Открытый вопрос политики совместимости `.proto`-схемы зафиксирован в
`docs/architecture/CORE_ARCHITECTURE_PLAN.md`, §10 — до его решения избегайте
breaking changes полей без крайней необходимости (переименование/удаление
существующих полей, смена номеров полей).

## Человекочитаемое описание методов

См. `docs/api/grpc.md` — держите его в синхроне при изменении `.proto`
(сейчас файл ещё описывает целевой набор §5.1 целиком — нужно обновить, чтобы
явно отделить реализованное подмножество от плана).
