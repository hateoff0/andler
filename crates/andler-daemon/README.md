# andler-daemon

Бинарник `andlerd`. Ядро — `daemon.rs`: структура `Daemon`, которая держит
реестр backend'ов (`HashMap<BackendKind, Arc<dyn HypervisorBackend>>`) и
in-memory состояние инстансов, без сети и без персистентности.

## Что здесь есть

- `daemon.rs` — **реализовано**. `Daemon::create_instance`/`start_instance`/
  `stop_instance`/`pause_instance`/`resume_instance`/`status`/
  `list_instances`/`remove_instance`/`get_instance_config`/
  `stream_instance_logs`/`clone_instance`/`export_instance_disk` — методы,
  которые соответствуют будущим gRPC-методам один-к-одному по смыслу, но
  пока вызываются прямо как обычные async-методы Rust. Каждый метод
  применяет переходы `andler_core::fsm` и делегирует нужному backend'у
  через `HypervisorBackend`. `Daemon::new()` регистрирует только
  `BackendKind::Qemu -> QemuBackend` — `Vmm` не регистрируется, пока
  `andler-vmm` остаётся пустым каркасом (см. его README).
  - **`list_instances`** — возвращает `Vec<InstanceSummary>`
    (`id`/`name`/`state` записи демона, не полный `InstanceConfig` и не
    live backend-статус). Единственный способ узнать, какие `InstanceId`
    вообще существуют, без необходимости заранее их знать — до этого
    метода созданный инстанс, чей `instance_id` потерян (закрыт терминал
    до того, как его записали), был физически жив, но недостижим ни для
    одной другой команды.
  - **`remove_instance`** — отвергает нетерминальные состояния
    (`Starting`/`Running`/`Paused`/`Stopping`) как
    `DaemonError::InstanceNotRemovable`, не пытается сама остановить
    инстанс перед удалением (явная последовательность `stop` → `remove`,
    не скрытый побочный эффект). Принимает `purge: bool` — по умолчанию
    `false`, файлы инстанса на диске не трогаются (`InstanceConfig.disk.path`
    может указывать на путь, который указал сам пользователь через
    `andler create --file`, автоматическое удаление произвольного
    пользовательского файла не должно быть неявным следствием удаления
    записи). `purge: true` (`andler remove --purge`) дополнительно
    удаляет `disk.path` и `firmware.ovmf_vars_path` (никогда
    `base_image`/`ovmf_code_path` — общие для нескольких инстансов файлы),
    затем best-effort `remove_dir` (не `remove_dir_all`) родительского
    каталога `disk.path` — убирает каталог только если он уже пуст
    (типичный случай для `AndroidVm::instance_dir`); непустой каталог
    (типичный случай для `LinuxVm` с пользовательским путём) остаётся на
    месте. Удаляет из `store`, если персистентность включена; ошибка
    удаления из `store` (как и любая ошибка purge-файлов) логируется, но
    не проваливает операцию (та же семантика, что у `persist_state`).
    `purge: true` дополнительно отказывает целиком (ничего не удаляется
    — ни запись, ни файлы), если у инстанса есть живые
    `CloneMode::Linked`-клоны (`Daemon::find_live_clones`, см. ниже) —
    удаление `disk.path` сломало бы их `backing_file`.
  - **`get_instance_config`** — возвращает полный `InstanceConfig` одного
    инстанса (клон, не ссылку — read-lock `instances` отпускается до
    конвертации в proto на стороне `service.rs`). Существует отдельно от
    `list_instances` — той не нужен весь объём данных каждой записи на
    каждый вызов, а здесь это точечный запрос по одному `InstanceId`.
  - **`stream_instance_logs`** — live-tail stdout/stderr процесса
    гипервизора инстанса (`andler_core::LogLine`), без истории — только
    строки, появившиеся после подписки. Для инстанса без запущенного
    backend'а (`record.handle == None`) возвращает немедленно
    завершающийся пустой поток, не `Err` — наблюдать за процессом,
    которого сейчас нет, не невалидный запрос (в отличие от
    `pause_instance`/`resume_instance` с тем же отсутствующим хэндлом —
    те запрашивают действие над процессом, который обязан существовать).
    Возвращаемый `BoxStream<'static, LogLine>` собран через
    `async_stream::stream!`, владеющий собственным клоном
    `Arc<dyn HypervisorBackend>` и `BackendHandle` — необходимо, так как
    `HypervisorBackend::log_stream` по сигнатуре возвращает поток,
    заимствующий `&self` backend'а; без этой обёртки результат был бы
    привязан к времени жизни вызова `stream_instance_logs`, а не мог бы
    переживать его (что обязательно — gRPC-хендлер в `service.rs`
    поллит этот поток уже после возврата из самого вызова).
  - **`clone_instance`** — клонирует `AndroidVm`-инстанс (только
    `AndroidVm` — `LinuxVm` не имеет управляемого `instances_root`,
    куда детерминированно положить файлы клона; попытка возвращает
    `DaemonError::CloneNotSupportedForKind`) в новый `InstanceId`, в
    одном из трёх `andler_core::CloneMode` (см. его документацию за
    полным обоснованием каждого варианта — `Linked`/`FullStandalone`/
    `SharedBase`). Источник должен быть в терминальном состоянии
    (`Created`/`Stopped`/`Error`, как и у `remove_instance`) —
    `DaemonError::InstanceNotClonable` иначе. `OVMF_VARS` всегда
    копируется (содержимое, не чистый шаблон — снапшот EFI-состояния
    источника), независимо от режима диска. Клонирование уже
    существующего клона разрешено — для этого метода исходный инстанс
    это просто запись с `InstanceConfig`, то, что она сама была создана
    как клон чего-то ещё, не требует особого случая. При сбое
    посередине — тот же `InstanceDirGuard`, что и у
    `create_android_instance`.
  - **`export_instance_disk`** — экспортирует диск `AndroidVm`-инстанса
    в самостоятельный файл по указанному пути, не создаёт инстанс (в
    отличие от `clone_instance` с `CloneMode::FullStandalone`, который
    создаёт) — переиспользует тот же `andler_disk::clone::full_standalone_clone`
    без дублирования кода, просто без шагов "создать каталог
    инстанса"/"скопировать OVMF_VARS"/"зарегистрировать". Те же условия
    на источник, что у `clone_instance`.
  - **`find_live_clones`** (приватный) — находит `InstanceId` всех
    инстансов, чей `disk.base_image` указывает прямо на диск инстанса
    `id` — то есть живых `CloneMode::Linked`-клонов. Сравнение по
    `disk.path`, не по `InstanceId` (`base_image` хранит путь к файлу,
    не идентификатор) — O(количество инстансов) сканирование на каждый
    `remove_instance(purge=true)`, не отдельный индекс (на ожидаемых
    масштабах — десятки, не тысячи инстансов на один `andlerd` —
    заводить индекс ради этого было бы преждевременной оптимизацией).
- `Daemon::create_android_instance` — **реализовано**. Связывает
  `AndroidProfile::resolve()` (`andler-core`, чистая функция) с реальным
  созданием на диске: каталог инстанса, персональная копия `OVMF_VARS`
  (копирование системного шаблона) и overlay-диск через
  `andler-disk::overlay::create_overlay`. Получившийся `InstanceConfig`
  проходит через тот же `create_instance`, что и при прямом вызове — то
  есть та же валидация backend'а и тот же реестр. Это первый сквозной
  путь от Android-профиля до зарегистрированного инстанса.
  - **Очистка при частичном сбое** — `InstanceDirGuard` (RAII, `Drop`)
    удаляет `instance_dir` целиком при любом раннем возврате метода
    (сбой `create_dir_all`/`copy`/`create_overlay`/финального
    `create_instance`), если последовательность не дошла до конца.
    Разряжается (`disarm()`) только после успешной регистрации в
    `Daemon` — до этого момента любой `?` оставляет каталог помеченным
    на удаление. `Drop::drop` синхронный, поэтому сама очистка —
    `std::fs::remove_dir_all`, не `tokio::fs`; см. подробности в
    docstring `InstanceDirGuard`.

## Что здесь НЕ реализовано (следующие слои поверх `Daemon`)

- **Сеть/gRPC** — теперь **реализовано**: `service.rs` (`DaemonService`) —
  тонкая обёртка `andler_rpc::proto::andler_service_server::AndlerService`
  поверх методов `Daemon`, по одному методу трейта на один метод `Daemon`,
  без дополнительной логики. `main.rs` поднимает её через
  `tonic::transport::Server` на `ANDLERD_LISTEN_ADDR`
  (по умолчанию `127.0.0.1:50051`, loopback осознанно — нет
  аутентификации/TLS, см. комментарий в `main.rs`). Набор методов — намеренное
  подмножество §5.1 архитектурного плана; см. README `andler-rpc` почему.
  Отображение `DaemonError -> tonic::Status` — в `service.rs`
  (`impl From<DaemonError> for Status`).
  - **`CreateInstance` (generic, `LinuxVm`)** — теперь **реализовано**:
    принимает полный `InstanceConfig` через `CreateInstanceRequest`
    (все 9 под-конфигураций), без промежуточного резолва — конвертация
    `andler_rpc::convert::TryFrom<CreateInstanceRequest> for InstanceConfig`
    уже даёт готовый конфиг, который идёт прямо в `Daemon::create_instance`.
    До этого единственным сквозным путём создания инстанса по сети был
    `CreateAndroidInstance` — теперь `LinuxVm` тоже доступен клиенту, не
    только напрямую через Rust-вызов `Daemon::create_instance` в тестах.
- **Персистентность** — теперь **реализовано**: `Daemon` опционально несёт
  `andler_store::Store` (`Daemon::new()` — без персистентности, как
  раньше; `Daemon::with_store(store)` — с персистентностью без
  восстановления; `Daemon::restore(store)` — восстанавливает все ранее
  сохранённые инстансы при старте, используется в `main.rs`).
  `create_instance` сохраняет новую запись, `start_instance`/
  `stop_instance` сохраняют каждый промежуточный и финальный переход FSM
  (включая `Starting`/`Stopping` — не только конечные `Running`/`Stopped`/
  `Error`). Ошибка записи в `store` логируется (`tracing::error!`) и не
  проваливает саму операцию — in-memory состояние остаётся источником
  истины для текущей сессии демона; см. подробное обоснование в
  комментарии `Daemon::persist_state`/`persist_new_instance`.
  `pause_instance`/`resume_instance` не персистируются отдельно — как и
  раньше, они не меняют `InstanceState` записи демона напрямую (см.
  раздел "Важные решения" ниже), поэтому нет нового состояния, которое
  нужно было бы сохранить.
  - **Восстановление и потерянные backend-хэндлы** — `Daemon::restore`
    принудительно переводит любую запись, восстановленную в
    нетерминальном состоянии (`Starting`/`Running`/`Paused`/`Stopping`),
    в `InstanceState::Error` — backend-хэндл (дескриптор живого QEMU-
    процесса) никогда не персистируется и не может быть восстановлен
    после перезапуска `andlerd`, так что оставлять такую запись как
    `Running` было бы ложью о текущем состоянии. См. подробности в
    docstring `Daemon::restore`.
  - Путь к sqlite-файлу — `ANDLERD_STORE_PATH` (по умолчанию
    `andlerd-state.db` в рабочем каталоге процесса).
- **Получение `base_image_path` для `create_android_instance`** — проверка
  кэша базового образа и скачивание (см. §4.4.2 архитектурного плана и
  `GUEST_IMAGE_PLAN.md`) остаются вне `Daemon`: метод принимает уже готовый
  путь к базовому образу, не качает его сам. Через gRPC это значит, что
  `base_image_path`/`ovmf_vars_template` в `CreateAndroidInstanceRequest` —
  пути на файловой системе хоста `andlerd`, не клиента (см. `docs/api/grpc.md`).

## Важные решения, зафиксированные в `daemon.rs`

- Ошибка `HypervisorBackend::spawn`/`stop` переводит запись инстанса в
  `InstanceState::Error { message }`, а не оставляет её в промежуточном
  `Starting`/`Stopping` — иначе запись «застревала» бы в состоянии, из
  которого FSM не разрешает дальнейших переходов, кроме `Fail`.
- `pause_instance`/`resume_instance` не меняют `InstanceState` записи
  демона напрямую при успехе — реальное состояние гостя (`Running` vs
  `Paused`) синхронизируется через `status()`, который спрашивает backend
  (а тот — QMP `query-status`), а не предполагается на основании одной
  лишь успешной отправки команды.

## Тесты

- `daemon::tests` — `Daemon` напрямую, без сети (включая
  `create_android_instance`, плюс
  `create_android_instance_cleans_up_instance_dir_on_missing_ovmf_template`
  и обновлённый `create_android_instance_fails_when_base_image_missing`
  — оба теперь проверяют не только код ошибки, но и то, что
  `InstanceDirGuard` реально удалил `instance_dir` с диска после сбоя;
  успешный путь `create_android_instance_resolves_profile_and_creates_overlay`,
  `#[ignore]` как требующий `qemu-img`, неявно подтверждает обратное —
  что guard не удаляет каталог при успехе, проверяя
  `instance_dir.join("disk.qcow2").exists()`), плюс блок персистентности:
  `with_store_persists_created_instance`,
  `with_store_persists_failed_start_as_error_state`,
  `daemon_without_store_does_not_panic_on_state_transitions` (режим без
  `Store` остаётся валидным no-op путём), `restore_*` — восстановление из
  `Store` (терминальные состояния переживают restore как есть,
  нетерминальные принудительно становятся `Error`, пустой `Store` даёт
  `Daemon` без инстансов, восстановленный `Daemon` продолжает
  персистировать новые операции), плюс `list_instances_*` — пустой
  список на свежем `Daemon`, одна запись на каждый созданный инстанс,
  актуальное (не застывшее) состояние после неудачного `start_instance`,
  и видимость восстановленных через `Daemon::restore` инстансов. Плюс
  `remove_instance_*`: успех из `Created`/`Error`/`Stopped`, отказ для
  каждого нетерминального состояния (инъектированного напрямую в
  `Daemon::instances` — тестовый модуль того же файла имеет доступ к
  приватным `InstanceRecord`/`instances`, что избавляет от необходимости
  реального `qemu-system-x86_64` для проверки именно этой ветки), а
  также то, что удаление действительно убирает запись из `store` и из
  `list_instances`. Плюс пять тестов на `purge` (каждый с настоящими
  файлами во временном каталоге, не моками): `purge: false` не трогает
  ни `disk.path`, ни `firmware.ovmf_vars_path`; `purge: true` удаляет оба
  файла и убирает опустевший родительский каталог; `purge: true` с
  посторонним пользовательским файлом в том же каталоге удаляет только
  свои два файла и оставляет каталог + посторонний файл на месте;
  `purge: true` никогда не трогает `base_image`/`ovmf_code_path`, даже
  если они физически существуют рядом; `purge: true` не проваливает
  операцию, если файлы уже были удалены руками до вызова. Плюс
  `get_instance_config_*`: `InstanceNotFound` для
  неизвестного `id`, точное соответствие возвращённого конфига тому, что
  было передано в `create_instance`, и то, что метод видит актуальную
  запись, а не застывший снимок на момент создания. Плюс
  `stream_logs_before_start_returns_empty_stream_not_error` (в отличие
  от `pause`/`resume` с тем же отсутствующим хэндлом — `Ok` с пустым
  потоком, не `Err`) и
  `stream_logs_on_unknown_instance_returns_instance_not_found`. Плюс блок
  `clone_instance`/`export_instance_disk`/`find_live_clones` —
  error-пути без реального `qemu-img` через `sample_android_config`
  (создаёт `AndroidVm`-запись напрямую через `create_instance`, минуя
  `create_android_instance`, поэтому не требует настоящего overlay-файла
  на диске для проверки логики `Daemon`, не файловой системы):
  `InstanceNotFound`/`CloneNotSupportedForKind` (для `LinuxVm`)/
  `InstanceNotClonable` (для `Running`, состояние инъектировано
  напрямую, тот же приём, что у `remove_instance`-тестов) для обоих
  методов; `find_live_clones` — пустой результат для инстанса без
  клонов, находит клон по совпадению `disk.base_image` с `disk.path`
  источника, не путает обычные overlay одного профиля (общий
  `base_image`, не клон-источник) с настоящими клонами;
  `remove_instance_with_purge_rejects_when_live_linked_clone_exists`/
  `remove_instance_with_purge_succeeds_when_clone_is_full_standalone` —
  `Linked`-клон блокирует purge источника, `FullStandalone`-клон не
  блокирует. Плюс сквозные `#[ignore]`-тесты (требуют `qemu-img`,
  начинаются от настоящего `create_android_instance`): по одному на
  каждый `CloneMode` (`clone_instance_with_linked_mode_creates_overlay_pointing_at_source_disk`
  дополнительно проверяет end-to-end, что purge источника с живым
  Linked-клоном отказывает уже через реальный диск, не только
  изолированно; `clone_instance_with_shared_base_mode_survives_source_purge`
  явно удаляет источник через purge и проверяет, что файл клона
  физически выжил), `clone_instance_of_a_clone_is_allowed`
  (двухуровневая `Linked`-цепочка source → clone_a → clone_b, обе
  ссылки проверяются явно), и `export_instance_disk_creates_standalone_file_without_registering_instance`
  (`list_instances` после экспорта видит ровно одну запись — саму
  изначальную, не появившуюся новую).
- `grpc_roundtrip_test` (`#[cfg(test)]`, не помечен `#[ignore]`) — реальный
  `tonic::transport::Server` на эфемерном `127.0.0.1`-порту + реальный
  `AndlerServiceClient` через настоящий TCP. Проверяет то, что unit-тесты
  `andler-rpc::convert` не могут: что весь путь "клиент сериализует -> сервер
  роутит на метод трейда -> зовёт `Daemon` -> сериализует ответ обратно"
  реально работает по сети, а не только компилируется. Не требует
  `qemu-img`/`/dev/kvm` — только TCP-loopback, поэтому не в
  `integration-test` Docker-таргете. Использует `Daemon::new()` (без
  персистентности) — этот тест про gRPC-слой, не про `andler-store`.
  Включает три теста на `CreateInstance`:
  `create_instance_round_trips_over_real_grpc_and_status_reports_created`
  (полный happy path, плюс `oneof`-вариант `RenderBackend::Venus`),
  `create_instance_with_bridge_network_round_trips_over_real_grpc`
  (`NetworkMode::Bridge` — `oneof`-вариант с данными, не просто
  пустой message-маркер, единственный надёжный способ проверить, что
  `prost` реально переживает protobuf-сериализацию этой ветки по TCP, а
  не только в памяти, как unit-тесты `convert.rs`), и
  `create_instance_missing_cpu_field_round_trips_as_invalid_argument`
  (отсутствие обязательного `Option`-поля -> `INVALID_ARGUMENT`, не паника
  сервиса). Плюс три теста на `ListInstances`: пустой результат на свежем
  сервере, точное соответствие `instance_id`/`name`/`state` тому, что
  вернул предшествующий `CreateInstance` (не просто "список не пуст" —
  явная проверка, что список не рассинхронизирован с тем, что реально
  создано), и видимость нескольких созданных инстансов одновременно.
  Плюс два теста на `RemoveInstance`: полный happy path
  (`CreateInstance` -> `StartInstance` с `RenderBackend::Passthrough`,
  гарантированно проваливающим `spawn` синхронно без реального QEMU,
  -> `Error` -> успешное удаление -> `GetInstanceStatus` возвращает
  `NOT_FOUND`), и удаление незарегистрированного `instance_id` ->
  `NOT_FOUND`. Отказ для `Running`/нетерминальных состояний по сети
  отдельно не тестируется (нет лёгкого способа детерминированно
  получить такое состояние через настоящий gRPC-вызов без
  `qemu-system-x86_64`) — эта ветка покрыта `daemon::tests` напрямую,
  через инъекцию состояния в приватные поля `Daemon`. Плюс два теста на
  `GetInstanceConfig`: точное соответствие всех полей (включая
  `oneof`-вариант `RenderBackend::Venus` и `InstanceKind::LinuxVm`) тому,
  что было передано в предшествующий `CreateInstance`, и `NOT_FOUND` для
  незарегистрированного `instance_id`. Плюс три теста на
  `StreamInstanceLogs`: для только что созданного, никогда не
  запускавшегося инстанса — настоящий `tonic::Streaming<LogLineResponse>`
  завершается на первом `.message()` сразу `Ok(None)`, не зависает и не
  ошибается (это то, что unit-тест с `BoxStream` напрямую в `daemon.rs`
  не может подтвердить — там нет настоящей gRPC-сериализации/жизненного
  цикла стрима); `NOT_FOUND` для незарегистрированного `instance_id`
  (ошибка приходит из самого вызова, до получения `Streaming`, как и для
  остальных методов); `INVALID_ARGUMENT` для `instance_id`, не
  являющегося валидным UUID. Что не покрыто здесь: реальные строки
  лога живого QEMU-процесса по сети — для этого нужен бы реальный
  бинарник, такая проверка осталась бы для `integration-test`/e2e, не
  для этого файла (требование "без `qemu-img`/`/dev/kvm`" в начале этой
  секции).

## Требования к окружению

Доступ к `/dev/kvm` (пользователь в группе `kvm`) — без root и без
`CAP_SYS_ADMIN`. См. §10 архитектурного плана.

## E2E-проверка (не cargo test)

`docker/e2e_smoke.sh` (таргет `e2e` в `docker/Dockerfile.dev`/
`docker-compose.yml`) — ручная сквозная проверка `andlerd`+`andler` как
двух настоящих процессов: реальный TCP, реальный sqlite-файл, реальный
`qemu-system-x86_64`, реальный перезапуск процесса демона (kill + новый
процесс, не `Daemon::restore()` в памяти теста). Использует
`DisplayEngine::None` ("-display none") для headless-запуска — без
этого варианта раньше требовался виртуальный `Xvfb` только чтобы у SDL
было куда присоединиться.

```
docker compose -f docker/docker-compose.yml run --rm e2e
```

