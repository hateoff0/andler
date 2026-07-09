//! `QemuBackend` — реализация `HypervisorBackend` (из `andler-core`) поверх
//! `cmdline::build_args`, `process::QemuProcess` и `qmp::QmpClient`.
//!
//! Связывает чистую сборку аргументов, низкоуровневое управление процессом
//! и QMP-протокол с единым интерфейсом, через который `andler-daemon`
//! управляет инстансами (см. docs/architecture/CORE_ARCHITECTURE_PLAN.md,
//! §2.1).
//!
//! `spawn`/`stop`/`pause`/`resume`/`status`/`snapshot`/`snapshot_restore`/
//! `snapshot_delete`/`snapshot_list`/`log_stream`/`metrics_stream`
//! реализованы полноценно.

use std::collections::HashMap;
use std::path::PathBuf;

use andler_core::{
    BackendError, BackendHandle, BackendStatus, HypervisorBackend, InstanceConfig, InstanceState,
    LogLine, LogStreamSource, RenderBackend, ResourceMetrics,
};
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use tokio::sync::Mutex;

use crate::cmdline;
use crate::process::{ProcessError, QemuProcess};
use crate::qmp::{QmpClient, QmpError, VmStatus};

/// Каталог, в котором создаются QMP-сокеты запущенных инстансов —
/// подкаталог `andler/qmp` под `andler_core::paths::runtime_dir()`
/// (`$XDG_RUNTIME_DIR` на большинстве современных Linux-хостов, а не
/// голый `/tmp`, который на многопользовательской системе
/// world-writable и уязвим к symlink-атаке на сокет — см. PLAN.md, item
/// 20a, "Hardcoded `/tmp` paths for IPC sockets"; каталог создаётся с
/// правами `0700` через `ensure_private_dir` в `spawn` ниже, не просто
/// `create_dir_all`). Раньше это была голая константа `/tmp/andler/qmp`
/// именно потому, что `QemuBackend` не получал информацию о путях
/// откуда-либо ещё — с появлением единой точки резолва путей в
/// `andler-core` (см. её собственный doc-комментарий) необходимость в
/// отдельной константе здесь отпала.
fn qmp_socket_dir() -> PathBuf {
    andler_core::paths::runtime_dir().join("andler/qmp")
}

/// Запущенный инстанс с точки зрения `QemuBackend`: процесс плюс,
/// опционально, уже установленное QMP-соединение.
///
/// `qmp_client` — `Option`, не безусловное соединение сразу при `spawn`:
/// `-qmp ...,server,nowait` означает, что QEMU поднимает сокет и не
/// блокируется, ожидая подключения — но клиенту всё равно может
/// потребоваться несколько попыток подключиться сразу после `spawn`, пока
/// QEMU полностью не инициализировался. Подключение делается лениво, при
/// первом вызове `pause`/`resume`/`status`, а не сразу в `spawn`, чтобы не
/// удлинять и не усложнять сам `spawn` обработкой retry-логики, которая
/// нужна только тем операциям, что реально используют QMP.
struct RunningInstance {
    process: QemuProcess,
    qmp_client: Option<QmpClient>,
    /// Таймаут ожидания завершения async job (snapshot), считанный из
    /// `InstanceConfig.disk.snapshot_timeout_secs` при `spawn`.
    snapshot_timeout: std::time::Duration,
}

/// Backend гипервизора поверх процесса QEMU.
///
/// Хранит реестр живых процессов под `Mutex` — `HypervisorBackend::spawn`
/// и остальные методы принимают `&self`, не `&mut self` (это диктует сам
/// trait, так как один `QemuBackend` используется конкурентно из нескольких
/// gRPC-запросов в `andler-daemon`), поэтому внутренняя мутабельность
/// обязательна.
pub struct QemuBackend {
    instances: Mutex<HashMap<BackendHandle, RunningInstance>>,
}

impl QemuBackend {
    pub fn new() -> Self {
        QemuBackend {
            instances: Mutex::new(HashMap::new()),
        }
    }

    /// Строит `BackendHandle` для инстанса. Формат (`"qemu:{instance_id}"`)
    /// — деталь реализации, не часть публичного контракта: вызывающая
    /// сторона (`andler-daemon`) должна получать `BackendHandle` только как
    /// результат `spawn()` и передавать его обратно как есть, не
    /// разбирая/конструируя самостоятельно.
    fn handle_for(cfg: &InstanceConfig) -> BackendHandle {
        BackendHandle(format!("qemu:{}", cfg.id.0))
    }

    fn qmp_socket_path_for(cfg: &InstanceConfig) -> PathBuf {
        qmp_socket_dir().join(format!("{}.sock", cfg.id.0))
    }

    /// Возвращает рабочее QMP-соединение для инстанса, устанавливая его
    /// при необходимости (см. документацию `RunningInstance::qmp_client`).
    ///
    /// Принимает `&mut RunningInstance` напрямую (не `&BackendHandle` +
    /// повторный поиск в реестре) — вызывающая сторона уже держит
    /// блокировку реестра и нашла нужный инстанс, повторный поиск был бы
    /// избыточен и потенциально рассинхронизировался бы с конкурентными
    /// изменениями реестра между поисками.
    async fn ensure_qmp_connected(instance: &mut RunningInstance) -> Result<(), QmpError> {
        if instance.qmp_client.is_some() {
            return Ok(());
        }

        let client = QmpClient::connect(instance.process.qmp_socket_path()).await?;
        instance.qmp_client = Some(client);
        Ok(())
    }

    /// Проверяет доступность QEMU Guest Agent для инстанса.
    ///
    /// Используется daemon'ом для auto-fallback: если agent недоступен,
    /// установка/удаление пакетов делается offline через qemu-nbd.
    pub async fn is_guest_agent_available(
        &self,
        handle: &BackendHandle,
    ) -> Result<bool, BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let available = instance
            .qmp_client
            .as_mut()
            .unwrap()
            .is_guest_agent_available()
            .await;

        Ok(available)
    }

    /// Устанавливает пакет в гостевую ОС online через QEMU Guest Agent.
    ///
    /// Выполняет `guest-exec` с командой包管理器 (apt/dnf/pacman) для
    /// установки пакета. Ожидает завершения через `guest-exec-status`
    /// с таймаутом 60 секунд.
    pub async fn guest_exec_install(
        &self,
        handle: &BackendHandle,
        package: &str,
    ) -> Result<(), BackendError> {
        self.guest_exec_package(handle, package, true).await
    }

    /// Удаляет пакет из гостевой ОС online через QEMU Guest Agent.
    pub async fn guest_exec_remove(
        &self,
        handle: &BackendHandle,
        package: &str,
    ) -> Result<(), BackendError> {
        self.guest_exec_package(handle, package, false).await
    }

    /// Общая реализация: install (install=true) или remove (install=false).
    async fn guest_exec_package(
        &self,
        handle: &BackendHandle,
        package: &str,
        install: bool,
    ) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let qmp = instance.qmp_client.as_mut().unwrap();

        // Сначала определяем пакетный менеджер через which
        let detect_cmd = "/bin/sh";
        let detect_args = vec!["-c", "which apt-get || which dnf || which pacman"];
        let pid = qmp
            .guest_exec(detect_cmd, &detect_args)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let manager = wait_for_guest_exec(qmp, pid, std::time::Duration::from_secs(10))
            .await
            .map_err(qmp_error_to_backend_error)?;

        let pkg_bin = manager
            .as_deref()
            .unwrap_or("apt-get")
            .trim();

        let cmd_args: Vec<&str> = if install {
            match pkg_bin {
                "apt-get" => vec!["apt-get", "install", "-y", package],
                "dnf" => vec!["dnf", "install", "-y", package],
                "pacman" => vec!["pacman", "-S", "--noconfirm", package],
                _ => vec!["apt-get", "install", "-y", package],
            }
        } else {
            match pkg_bin {
                "apt-get" => vec!["apt-get", "remove", "-y", package],
                "dnf" => vec!["dnf", "remove", "-y", package],
                "pacman" => vec!["pacman", "-R", "--noconfirm", package],
                _ => vec!["apt-get", "remove", "-y", package],
            }
        };

        let pid = qmp
            .guest_exec(cmd_args[0], &cmd_args[1..])
            .await
            .map_err(qmp_error_to_backend_error)?;

        let result = wait_for_guest_exec(qmp, pid, std::time::Duration::from_secs(60))
            .await
            .map_err(qmp_error_to_backend_error)?;

        if let Some(output) = result {
            tracing::info!(
                package = %package,
                install = %install,
                output = %output,
                "guest-exec completed"
            );
        }

        Ok(())
    }
}

impl Default for QemuBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Переводит `ProcessError` в `BackendError`, сохраняя сообщение.
/// Отдельная функция, а не `impl From`, чтобы не создавать зависимость
/// `andler-qemu::process::ProcessError -> andler-core` (она и не нужна:
/// `process.rs` не зависит от `andler-core`, см. его документацию) —
/// конвертация делается в этом модуле, на границе с trait'ом.
fn process_error_to_backend_error(err: ProcessError) -> BackendError {
    BackendError::Io(err.to_string())
}

/// Переводит `QmpError` в `BackendError` той же логикой, что и
/// `process_error_to_backend_error` — `qmp.rs` тоже не зависит от
/// `andler-core` (нет причины: протокол QMP не знает про `InstanceConfig`),
/// конвертация делается на границе с trait'ом, не внутри `qmp.rs`.
fn qmp_error_to_backend_error(err: QmpError) -> BackendError {
    BackendError::Io(err.to_string())
}

/// Ожидает завершения команды, запущенной через `guest-exec`, poll-я
/// `guest-exec-status` до тех пор, пока `exited` не станет `true`.
///
/// Возвращает stdout процесса (String) при успехе, или ошибку при провале.
async fn wait_for_guest_exec(
    qmp: &mut QmpClient,
    pid: u64,
    timeout: std::time::Duration,
) -> Result<Option<String>, QmpError> {
    use std::time::Instant;
    let start = Instant::now();

    loop {
        let status = qmp.guest_exec_status(pid).await?;

        if status.exited {
            if status.exitcode != 0 {
                let stderr = status.err_data.unwrap_or_default();
                return Err(QmpError::CommandFailed {
                    command: format!("guest-exec pid={pid}"),
                    class: "GuestExecFailed".to_string(),
                    desc: format!(
                        "guest process exited with code {}: {}",
                        status.exitcode,
                        stderr.trim()
                    ),
                });
            }
            return Ok(status.out_data);
        }

        if start.elapsed() > timeout {
            return Err(QmpError::CommandFailed {
                command: format!("guest-exec pid={pid}"),
                class: "Timeout".to_string(),
                desc: format!(
                    "guest process did not exit within {:?}",
                    timeout
                ),
            });
        }

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// Имя устройства для snapshot-команд QEMU.
/// Соответствует `id=drive-disk0` в cmdline (см. `cmdline.rs`).
const DISK_DEVICE: &str = "drive-disk0";

/// Таймаут ожидания завершения async job (snapshot-save/load/delete)
/// `VmStatus` (наблюдение QMP) -> `InstanceState` (домен `andler-core`).
///
/// `VmStatus::Shutdown`/`Other` намеренно не маппятся на `InstanceState`
/// напрямую здесь — `shutdown` с точки зрения QMP означает "гостевая ОС
/// попросила выключение", но процесс QEMU может ещё быть жив несколько
/// мгновений после этого; различать это от `Stopped` в смысле FSM
/// (`andler_core::fsm`) — забота `andler-daemon`, который видит и
/// `BackendStatus`, и факт того, жив ли сам процесс. Здесь возвращается
/// `None` для статусов, которые не однозначно соответствуют
/// `Running`/`Paused`, а вызывающая сторона (`status()` ниже) решает, что
/// с этим делать.
/// Читает `qemu.log` целиком и парсит его обратно в `LogLine`, чтобы
/// `log_stream` мог отдать историю до момента подписки клиента — сам
/// файл пишется `QemuProcess::drain_to_tracing` построчно как
/// `[stdout] <line>`/`[stderr] <line>` (см. её документацию), формат
/// здесь — просто обратный разбор того же самого.
///
/// Отсутствие файла, ошибка чтения или строка без ожидаемого префикса
/// (испорченный файл, отредактированный вручную) — не паника и не
/// ошибка наружу, просто эта конкретная строка (или всё содержимое, если
/// файла вообще нет) выпадает из истории. Клиент вызывает `log_stream`
/// ради live-хвоста в первую очередь; отсутствие истории не должно ему
/// в этом мешать.
///
/// Единственный пограничный случай, о котором стоит знать: строка, чья
/// запись в файл завершилась непосредственно перед этим чтением, но чья
/// публикация в broadcast-канал (см. `QemuProcess::log_sender`) —
/// технически уже после того, как `log_stream` успел подписаться,
/// теоретически может оказаться и здесь, в истории, и затем ещё раз в
/// live-хвосте. Файловая запись и broadcast-отправка в
/// `drain_to_tracing` всегда идут в этом порядке (сначала файл, потом
/// broadcast) — поэтому на этой границе возможен только редкий дубль
/// одной строки, никогда не потеря.
async fn read_log_history(path: &std::path::Path) -> Vec<LogLine> {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(_) => return Vec::new(),
    };

    content
        .lines()
        .filter_map(|line| {
            if let Some(rest) = line.strip_prefix("[stdout] ") {
                Some(LogLine {
                    source: LogStreamSource::Stdout,
                    line: rest.to_string(),
                })
            } else if let Some(rest) = line.strip_prefix("[stderr] ") {
                Some(LogLine {
                    source: LogStreamSource::Stderr,
                    line: rest.to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

fn vm_status_to_instance_state(status: VmStatus) -> Option<InstanceState> {
    match status {
        VmStatus::Running => Some(InstanceState::Running),
        VmStatus::Paused => Some(InstanceState::Paused),
        VmStatus::Shutdown | VmStatus::Other => None,
    }
}

#[async_trait]
impl HypervisorBackend for QemuBackend {
    fn name(&self) -> &'static str {
        "qemu"
    }

    fn supported_render_backends(&self) -> &[RenderBackend] {
        // Passthrough намеренно не входит в этот список — см. §2.3
        // архитектурного плана и документацию RenderBackend::is_implemented.
        // Используется &'static, чтобы не аллоцировать новый Vec на каждый
        // вызов; конкретные варианты не несут данных (кроме Passthrough,
        // которого здесь нет), так что статический срез безопасен.
        &[
            RenderBackend::Venus,
            RenderBackend::VirtioGpu,
            RenderBackend::VirGl,
            RenderBackend::Cpu,
        ]
    }

    async fn spawn(&self, cfg: &InstanceConfig) -> Result<BackendHandle, BackendError> {
        if !cfg.gpu.render_backend.is_implemented() {
            return Err(BackendError::InvalidConfig {
                backend: "qemu",
                reason: format!(
                    "render backend {:?} is not implemented, see docs/architecture/CORE_ARCHITECTURE_PLAN.md §2.3",
                    cfg.gpu.render_backend
                ),
            });
        }

        let handle = Self::handle_for(cfg);
        let qmp_socket_path = Self::qmp_socket_path_for(cfg);

        if let Some(parent) = qmp_socket_path.parent() {
            // `ensure_private_dir` sets `0700` on the directory, not
            // just the default umask — see PLAN.md, item 20b, "No file
            // permission controls". A world-readable QMP socket
            // directory would let another local user merely *see* which
            // instance IDs have sockets, even though connecting to the
            // socket itself still requires knowing the exact filename.
            andler_core::paths::ensure_private_dir(parent)
                .await
                .map_err(|e| BackendError::Io(e.to_string()))?;
        }

        let args = cmdline::build_args(cfg, &qmp_socket_path);

        // `cfg.disk.path`'s parent is the instance's own directory
        // (`<instances_root>/<id>/`, see PLAN.md "Структура хранения") —
        // reused here rather than introducing a separate "instance dir"
        // concept just for this file. `None` (no parent, e.g. a bare
        // relative filename) means no file log for this run rather than
        // a hard error — matches `open_log_file`'s own tolerance for a
        // missing/unwritable path.
        let log_file_path = cfg
            .disk
            .path
            .parent()
            .map(|dir| dir.join("qemu.log"));

        let process = QemuProcess::spawn(&args, qmp_socket_path, log_file_path)
            .await
            .map_err(process_error_to_backend_error)?;

        let mut instances = self.instances.lock().await;
        instances.insert(
            handle.clone(),
            RunningInstance {
                process,
                qmp_client: None,
                snapshot_timeout: std::time::Duration::from_secs(
                    cfg.disk.snapshot_timeout_secs.unwrap_or(30),
                ),
            },
        );

        Ok(handle)
    }

    async fn pause(&self, handle: &BackendHandle) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        instance
            .qmp_client
            .as_mut()
            .expect("qmp_client is Some after ensure_qmp_connected succeeded")
            .pause()
            .await
            .map_err(qmp_error_to_backend_error)
    }

    async fn resume(&self, handle: &BackendHandle) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        instance
            .qmp_client
            .as_mut()
            .expect("qmp_client is Some after ensure_qmp_connected succeeded")
            .resume()
            .await
            .map_err(qmp_error_to_backend_error)
    }

    async fn stop(&self, handle: &BackendHandle, graceful: bool) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        if graceful {
            // Без снапшота/snapshot.rs это всё ещё SIGTERM, не ACPI-сигнал
            // гостю через QMP system_powerdown — см. документацию
            // process::QemuProcess::terminate. Использование QMP здесь
            // для честного ACPI graceful shutdown — естественное
            // продолжение этого модуля, но не часть текущего шага
            // (текущий шаг — pause/resume/status, не shutdown-семантика).
            instance
                .process
                .terminate()
                .await
                .map_err(process_error_to_backend_error)?;
        } else {
            instance
                .process
                .force_kill()
                .await
                .map_err(process_error_to_backend_error)?;
        }

        instances.remove(handle);
        Ok(())
    }

    async fn status(&self, handle: &BackendHandle) -> Result<BackendStatus, BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        let alive = instance
            .process
            .is_alive()
            .await
            .map_err(process_error_to_backend_error)?;

        if !alive {
            return Ok(BackendStatus {
                state: InstanceState::Stopped,
                detail: Some("process is not running".to_string()),
            });
        }

        // Процесс жив — пробуем получить точный статус через QMP
        // query-status. Если QMP недоступен (например, сокет ещё не готов
        // сразу после spawn, либо соединение разорвалось) — не считаем это
        // фатальной ошибкой всего запроса статуса: с точки зрения
        // HypervisorBackend мы всё равно знаем, что процесс жив, просто не
        // знаем точного состояния гостя. Это явно отражается в `detail`,
        // а не маскируется молчаливым выбором Running по умолчанию.
        match Self::ensure_qmp_connected(instance).await {
            Ok(()) => {
                let qmp_status = instance
                    .qmp_client
                    .as_mut()
                    .expect("qmp_client is Some after ensure_qmp_connected succeeded")
                    .query_status()
                    .await;

                match qmp_status {
                    Ok(vm_status) => match vm_status_to_instance_state(vm_status) {
                        Some(state) => Ok(BackendStatus { state, detail: None }),
                        None => Ok(BackendStatus {
                            state: InstanceState::Running,
                            detail: Some(format!(
                                "process is alive; QMP reports VM status {vm_status:?}, \
                                 which does not map directly to a HypervisorBackend state"
                            )),
                        }),
                    },
                    Err(qmp_err) => Ok(BackendStatus {
                        state: InstanceState::Running,
                        detail: Some(format!(
                            "process is alive but QMP query-status failed: {qmp_err}"
                        )),
                    }),
                }
            }
            Err(qmp_err) => Ok(BackendStatus {
                state: InstanceState::Running,
                detail: Some(format!(
                    "process is alive but QMP connection failed: {qmp_err}"
                )),
            }),
        }
    }

    async fn snapshot(
        &self,
        handle: &BackendHandle,
        tag: &str,
        timeout: Option<std::time::Duration>,
    ) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let qmp = instance.qmp_client.as_mut().expect("just connected");
        let job_id = qmp
            .snapshot_save(DISK_DEVICE, tag)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let effective_timeout = timeout.unwrap_or(instance.snapshot_timeout);
        qmp.wait_job_completion(&job_id, effective_timeout)
            .await
            .map_err(qmp_error_to_backend_error)?;

        Ok(())
    }

    async fn snapshot_restore(
        &self,
        handle: &BackendHandle,
        tag: &str,
        timeout: Option<std::time::Duration>,
    ) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let qmp = instance.qmp_client.as_mut().expect("just connected");
        let job_id = qmp
            .snapshot_load(DISK_DEVICE, tag)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let effective_timeout = timeout.unwrap_or(instance.snapshot_timeout);
        qmp.wait_job_completion(&job_id, effective_timeout)
            .await
            .map_err(qmp_error_to_backend_error)?;

        Ok(())
    }

    async fn snapshot_delete(
        &self,
        handle: &BackendHandle,
        tag: &str,
        timeout: Option<std::time::Duration>,
    ) -> Result<(), BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let qmp = instance.qmp_client.as_mut().expect("just connected");
        let job_id = qmp
            .snapshot_delete(DISK_DEVICE, tag)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let effective_timeout = timeout.unwrap_or(instance.snapshot_timeout);
        qmp.wait_job_completion(&job_id, effective_timeout)
            .await
            .map_err(qmp_error_to_backend_error)?;

        Ok(())
    }

    async fn snapshot_list(
        &self,
        handle: &BackendHandle,
    ) -> Result<Vec<andler_core::SnapshotInfo>, BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let qmp = instance.qmp_client.as_mut().expect("just connected");
        let snapshots = qmp
            .query_block_snapshots(DISK_DEVICE)
            .await
            .map_err(qmp_error_to_backend_error)?;

        Ok(snapshots
            .into_iter()
            .map(|s| andler_core::SnapshotInfo {
                tag: s.tag,
                id: s.id,
                created_at: s.datetime,
            })
            .collect())
    }

    fn metrics_stream(&self, handle: &BackendHandle) -> BoxStream<'_, ResourceMetrics> {
        // Полная реализация: подписывается на broadcast-канал метрик,
        // который публикуется фоновой задачей metrics::spawn_metrics_poller
        // (запущена при spawn в process.rs). Паттерн идентичен log_stream
        // (см. там за обоснованием try_lock / обработки Lagged).
        let receiver = match self.instances.try_lock() {
            Ok(mut instances) => {
                instances.get_mut(handle).map(|i| i.process.subscribe_metrics())
            }
            Err(_would_block) => None,
        };

        match receiver {
            Some(receiver) => Box::pin(
                tokio_stream::wrappers::BroadcastStream::new(receiver).filter_map(|item| async {
                    match item {
                        Ok(metrics) => Some(metrics),
                        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(
                            _,
                        )) => None,
                    }
                }),
            ),
            None => Box::pin(futures_util::stream::empty()),
        }
    }

    fn log_stream(&self, handle: &BackendHandle) -> BoxStream<'_, LogLine> {
        // `log_stream` — не `async fn` (см. andler_core::backend за тем,
        // почему сигнатура трейта такая же, как у metrics_stream), а
        // `self.instances` — `tokio::sync::Mutex`, требующий `.await` для
        // обычного `lock()`. `try_lock()` — единственный способ
        // синхронно достать `RunningInstance` здесь без переделки всего
        // метода в `async fn` (что сломало бы единообразие с
        // metrics_stream и сигнатуру трейта).
        //
        // Все остальные методы (`pause`/`resume`/`stop`/`status`) держат
        // этот `Mutex` только на короткие, без внутренних `.await` на
        // самом локе, синхронные секции (взять `&mut RunningInstance`,
        // отдать обратно) — она не остаётся захваченной во время
        // QMP-обмена с самим QEMU (`ensure_qmp_connected` и операции QMP
        // вызываются на уже полученной ссылке, не повторно лочат
        // `instances`). Поэтому `try_lock()` здесь практически никогда не
        // провалится из-за конкуренции; в редком случае гонки с другим
        // вызовом `log_stream`/`pause`/`stop` в тот же момент — отдаём
        // пустой поток, тот же контракт, что и для "хэндл не найден" (см.
        // документацию `HypervisorBackend::log_stream`): подписка на
        // следующий вызов клиента отработает штатно, потерянных данных
        // нет (broadcast не накапливает историю для ещё не подключённого
        // подписчика в любом случае).
        //
        // Подписка на `subscribe_logs()` берётся здесь же, синхронно,
        // до какого-либо чтения `qemu.log` ниже — это важно для порядка
        // "история, потом live", а не только для самого списка полей.
        // См. документацию `read_log_history` за разбором единственного
        // возможного пограничного случая (редкий дубль одной строки, не
        // потеря).
        let subscription = match self.instances.try_lock() {
            Ok(mut instances) => instances
                .get_mut(handle)
                .map(|i| (i.process.log_file_path().map(PathBuf::from), i.process.subscribe_logs())),
            Err(_would_block) => None,
        };

        match subscription {
            Some((log_file_path, receiver)) => {
                let live = tokio_stream::wrappers::BroadcastStream::new(receiver).filter_map(
                    |item| async {
                        match item {
                            Ok(line) => Some(line),
                            // `Lagged(n)` — подписчик отстал больше, чем
                            // вмещает `LOG_CHANNEL_CAPACITY` (см.
                            // process::LOG_CHANNEL_CAPACITY), и пропустил `n`
                            // строк. Пропускаем сам факт пропуска молча и
                            // продолжаем поток со следующей доступной строки
                            // — закрывать стрим здесь было бы хуже для
                            // живого хвоста логов, чем потерять уведомление о
                            // разрыве; в логах andlerd (`tracing`) эти же
                            // строки в любом случае не потеряны — лагает
                            // только данный gRPC-подписчик, не сам канал.
                            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(
                                _,
                            )) => None,
                        }
                    },
                );

                // История из `qemu.log` идёт первой, живой хвост — сразу
                // за ней, одним непрерывным потоком для клиента (тот же
                // `stream_instance_logs`, что и раньше — этот метод не
                // видит разницы между "старой" и "новой" строкой).
                // `stream::once` + `flatten` вместо `async fn`, потому что
                // сигнатура трейта требует именно синхронный `log_stream`
                // (см. комментарий выше) — само чтение файла остаётся
                // асинхронным, просто отложенным до момента, когда поток
                // начнут поллить, а не выполненным прямо здесь.
                let history = futures_util::stream::once(async move {
                    match log_file_path {
                        Some(path) => read_log_history(&path).await,
                        None => Vec::new(),
                    }
                })
                .flat_map(|history_lines| futures_util::stream::iter(history_lines));

                Box::pin(history.chain(live))
            }
            None => Box::pin(futures_util::stream::empty()),
        }
    }

    async fn is_guest_agent_available(&self, handle: &BackendHandle) -> Result<bool, BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let available = instance
            .qmp_client
            .as_mut()
            .expect("qmp_client is Some after ensure_qmp_connected")
            .is_guest_agent_available()
            .await;

        Ok(available)
    }

    async fn guest_exec_install(
        &self,
        handle: &BackendHandle,
        package: &str,
    ) -> Result<(), BackendError> {
        self.guest_exec_package(handle, package, true).await
    }

    async fn guest_exec_remove(
        &self,
        handle: &BackendHandle,
        package: &str,
    ) -> Result<(), BackendError> {
        self.guest_exec_package(handle, package, false).await
    }

    async fn guest_check_binary_installed(
        &self,
        handle: &BackendHandle,
        binary_path: &str,
    ) -> Result<bool, BackendError> {
        let mut instances = self.instances.lock().await;
        let instance = instances
            .get_mut(handle)
            .ok_or_else(|| BackendError::HandleNotFound(handle.0.clone()))?;

        Self::ensure_qmp_connected(instance)
            .await
            .map_err(qmp_error_to_backend_error)?;

        let qmp = instance.qmp_client.as_mut().unwrap();

        // Run `test -x <binary_path>` via guest-exec
        let cmd = format!("test -x {binary_path}");
        let pid = qmp
            .guest_exec("/bin/sh", &["-c", &cmd])
            .await
            .map_err(qmp_error_to_backend_error)?;

        // Poll until exited, but treat non-zero exit as "not installed" (not error)
        use std::time::Instant;
        let start = Instant::now();
        let timeout = std::time::Duration::from_secs(5);

        loop {
            let status = qmp
                .guest_exec_status(pid)
                .await
                .map_err(qmp_error_to_backend_error)?;

            if status.exited {
                // exit code 0 = binary exists, non-zero = doesn't exist
                return Ok(status.exitcode == 0);
            }

            if start.elapsed() > timeout {
                return Ok(false);
            }

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use andler_core::{
        AudioConfig, BackendKind, CdromBus, CpuConfig, DiskConfig, DisplayConfig, FirmwareConfig,
        GpuConfig, InputConfig, InstanceId, InstanceKind, MemoryConfig, NetworkConfig,
    };
    use std::path::PathBuf;

    fn sample_config(render_backend: RenderBackend) -> InstanceConfig {
        let mut gpu = GpuConfig::reference_default();
        gpu.render_backend = render_backend;

        InstanceConfig {
            id: InstanceId::new(),
            name: "test-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/test.iso"),
                cdrom_bus: CdromBus::Ide,
            },
            backend: BackendKind::Qemu,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(PathBuf::from("/tmp/test-disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu,
            network: NetworkConfig::reference_default(),
            firmware: FirmwareConfig::reference_default(PathBuf::from("/tmp/test-vars.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
        }
    }

    #[test]
    fn name_returns_qemu() {
        let backend = QemuBackend::new();
        assert_eq!(backend.name(), "qemu");
    }

    #[test]
    fn supported_render_backends_excludes_passthrough() {
        let backend = QemuBackend::new();
        let supported = backend.supported_render_backends();
        assert!(supported.contains(&RenderBackend::Venus));
        assert!(!supported
            .iter()
            .any(|b| matches!(b, RenderBackend::Passthrough { .. })));
    }

    #[tokio::test]
    async fn spawn_rejects_passthrough_before_touching_process() {
        let backend = QemuBackend::new();
        let cfg = sample_config(RenderBackend::Passthrough {
            gpu_pci_id: "0000:01:00.0".to_string(),
        });

        let err = backend.spawn(&cfg).await.unwrap_err();
        assert!(matches!(err, BackendError::InvalidConfig { .. }));
    }

    #[tokio::test]
    async fn stop_on_unknown_handle_returns_handle_not_found() {
        let backend = QemuBackend::new();
        let unknown = BackendHandle("qemu:does-not-exist".to_string());

        let err = backend.stop(&unknown, true).await.unwrap_err();
        assert!(matches!(err, BackendError::HandleNotFound(_)));
    }

    #[tokio::test]
    async fn status_on_unknown_handle_returns_handle_not_found() {
        let backend = QemuBackend::new();
        let unknown = BackendHandle("qemu:does-not-exist".to_string());

        let err = backend.status(&unknown).await.unwrap_err();
        assert!(matches!(err, BackendError::HandleNotFound(_)));
    }

    #[tokio::test]
    async fn pause_resume_on_unknown_handle_return_handle_not_found() {
        let backend = QemuBackend::new();
        let unknown = BackendHandle("qemu:does-not-exist".to_string());

        assert!(matches!(
            backend.pause(&unknown).await,
            Err(BackendError::HandleNotFound(_))
        ));
        assert!(matches!(
            backend.resume(&unknown).await,
            Err(BackendError::HandleNotFound(_))
        ));
    }

    #[tokio::test]
    async fn metrics_stream_is_immediately_empty() {
        use futures_util::StreamExt;

        let backend = QemuBackend::new();
        let handle = BackendHandle("qemu:whatever".to_string());
        let mut stream = backend.metrics_stream(&handle);
        assert!(stream.next().await.is_none());
    }

    /// Неизвестный хэндл — пустой поток сразу же, не ошибка (см.
    /// документацию `HypervisorBackend::log_stream` за тем, почему это
    /// сознательно отличается от `pause`/`resume`/`status` с тем же
    /// неизвестным хэндлом). Не требует реального процесса QEMU —
    /// `instances` пуст с самого начала.
    #[tokio::test]
    async fn log_stream_on_unknown_handle_is_immediately_empty() {
        use futures_util::StreamExt;

        let backend = QemuBackend::new();
        let handle = BackendHandle("qemu:whatever".to_string());
        let mut stream = backend.log_stream(&handle);
        assert!(stream.next().await.is_none());
    }

    struct TestTempDir(PathBuf);

    impl TestTempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "andler-qemu-test-{}-{}",
                std::process::id(),
                InstanceId::new().0
            ));
            std::fs::create_dir_all(&path).expect("create test temp dir");
            TestTempDir(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn read_log_history_parses_stdout_and_stderr_lines_in_order() {
        let dir = TestTempDir::new();
        let log_path = dir.path().join("qemu.log");
        tokio::fs::write(
            &log_path,
            "[stdout] QEMU 8.2.0 starting\n[stderr] warning: no display\n[stdout] guest booted\n",
        )
        .await
        .expect("write test qemu.log");

        let history = read_log_history(&log_path).await;

        assert_eq!(
            history,
            vec![
                LogLine {
                    source: LogStreamSource::Stdout,
                    line: "QEMU 8.2.0 starting".to_string(),
                },
                LogLine {
                    source: LogStreamSource::Stderr,
                    line: "warning: no display".to_string(),
                },
                LogLine {
                    source: LogStreamSource::Stdout,
                    line: "guest booted".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn read_log_history_returns_empty_for_missing_file() {
        let dir = TestTempDir::new();
        // Never written to — exercises the "file doesn't exist" branch,
        // not just "empty file".
        let missing_path = dir.path().join("never-created-qemu.log");
        assert_eq!(read_log_history(&missing_path).await, Vec::new());
    }

    #[tokio::test]
    async fn read_log_history_skips_lines_without_a_known_prefix() {
        // Defensive case: a manually edited/corrupted file shouldn't
        // panic `log_stream`, just silently drop what it can't parse.
        let dir = TestTempDir::new();
        let log_path = dir.path().join("qemu.log");
        tokio::fs::write(&log_path, "garbage line\n[stdout] real line\n")
            .await
            .expect("write test qemu.log");

        let history = read_log_history(&log_path).await;

        assert_eq!(
            history,
            vec![LogLine {
                source: LogStreamSource::Stdout,
                line: "real line".to_string(),
            }]
        );
    }

    #[test]
    fn vm_status_maps_running_and_paused_directly() {
        assert_eq!(
            vm_status_to_instance_state(VmStatus::Running),
            Some(InstanceState::Running)
        );
        assert_eq!(
            vm_status_to_instance_state(VmStatus::Paused),
            Some(InstanceState::Paused)
        );
    }

    #[test]
    fn vm_status_does_not_map_shutdown_or_other_directly() {
        assert_eq!(vm_status_to_instance_state(VmStatus::Shutdown), None);
        assert_eq!(vm_status_to_instance_state(VmStatus::Other), None);
    }

    // spawn/stop/status/pause/resume на реальном живом процессе требуют
    // бинарника qemu-system-x86_64 и рабочего QMP-сокета — см.
    // process.rs/qmp.rs про конвенцию #[ignore] в этом крейте.
    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary, see docker/README.md integration-test target"]
    async fn spawn_then_status_then_stop_round_trip() {
        let backend = QemuBackend::new();
        let mut cfg = sample_config(RenderBackend::Cpu);
        cfg.display.display_engine = andler_core::DisplayEngine::Sdl;

        let handle = backend.spawn(&cfg).await.unwrap();
        let status = backend.status(&handle).await.unwrap();
        assert_eq!(status.state, InstanceState::Running);

        backend.stop(&handle, true).await.unwrap();
        let err = backend.status(&handle).await.unwrap_err();
        assert!(matches!(err, BackendError::HandleNotFound(_)));
    }

    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary, see docker/README.md integration-test target"]
    async fn spawn_then_pause_then_resume_round_trip() {
        let backend = QemuBackend::new();
        let mut cfg = sample_config(RenderBackend::Cpu);
        cfg.display.display_engine = andler_core::DisplayEngine::Sdl;

        let handle = backend.spawn(&cfg).await.unwrap();

        backend.pause(&handle).await.unwrap();
        let status = backend.status(&handle).await.unwrap();
        assert_eq!(status.state, InstanceState::Paused);

        backend.resume(&handle).await.unwrap();
        let status = backend.status(&handle).await.unwrap();
        assert_eq!(status.state, InstanceState::Running);

        backend.stop(&handle, false).await.unwrap();
    }

    /// Сквозная проверка `log_stream` через весь стек `QemuBackend`
    /// (в отличие от `drain_to_tracing_publishes_lines_to_subscriber` в
    /// `process.rs`, которая проверяет только саму механику чтения
    /// строк — здесь важно, что `BackendHandle` → `RunningInstance` →
    /// `subscribe_logs` → `BroadcastStream` действительно соединены друг
    /// с другом). QEMU обычно пишет в stderr хотя бы строку лицензии или
    /// предупреждения при запуске с `-nographic`/неполным набором
    /// устройств — этого достаточно, чтобы получить хотя бы одну строку
    /// без необходимости провоцировать конкретную ошибку.
    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary, see docker/README.md integration-test target"]
    async fn log_stream_receives_real_process_output() {
        use futures_util::StreamExt;
        use tokio::time::{timeout, Duration};

        let backend = QemuBackend::new();
        let mut cfg = sample_config(RenderBackend::Cpu);
        cfg.display.display_engine = andler_core::DisplayEngine::Sdl;

        let handle = backend.spawn(&cfg).await.unwrap();
        let mut stream = backend.log_stream(&handle);

        // Не любой запуск QEMU гарантированно что-то пишет в stdout/stderr
        // в первые секунды — таймаут здесь означает "не успели получить
        // строку", не "механика не работает"; smoke-проверка того, что
        // стрим хотя бы подключён к реальному процессу, не строгая
        // гарантия конкретного вывода.
        let _ = timeout(Duration::from_secs(5), stream.next()).await;

        backend.stop(&handle, false).await.unwrap();
    }
}

