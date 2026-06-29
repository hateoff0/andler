//! Клиент QEMU Machine Protocol (QMP) поверх unix-сокета.
//!
//! Реализует команды управления инстансами: `stop`/`cont`/`query-status`
//! (pause/resume/status) и `snapshot-save`/`snapshot-load`/`snapshot-delete`/
//! `query-block`/`query-jobs`/`job-dismiss` (снапшоты). Snapshot-команды
//! используют job-based API QEMU (доступен с QEMU 6.0+, см. README этого
//! крейта за обоснованием выбора в пользу него, не
//! `human-monitor-command`+`savevm`): `snapshot-save`/`-load` принимают
//! `vmstate`+`devices` (массив node-name, **не** `device` единственного
//! числа — реальная QAPI-схема, см. `qemu-qmp-ref`), `snapshot-delete`
//! принимает только `devices`, без `vmstate`. Все три запускают
//! асинхронный job, завершение которого ожидается через
//! `wait_job_completion` (polling `query-jobs` до статуса `"concluded"` —
//! единственного реального терминального статуса job в QEMU; успех/провал
//! различается по полю `error`, не отдельным значением `status`) с
//! последующим обязательным `job-dismiss`.
//!
//! Протокол: newline-delimited JSON поверх unix-сокета. При подключении
//! QEMU сразу присылает greeting (`{"QMP": {...}}`); клиент обязан
//! отправить `{"execute": "qmp_capabilities"}` прежде чем слать любые
//! другие команды — без этого QEMU отвечает ошибкой на всё, кроме
//! `qmp_capabilities` (режим "capabilities negotiation").
//!
//! Snapshot-job'ы асинхронно шлют события `JOB_STATUS_CHANGE` по тому же
//! сокету, перемежая их с обычными `return`/`error`-ответами на команды
//! во время поллинга `query-jobs`. `execute_raw` пропускает любое
//! сообщение без `return`/`error` (то есть событие) через
//! `read_reply_skipping_events`, не считает его ошибкой парсинга — это не
//! полноценная подписка на события (нет очереди, нет публичного API для
//! их чтения), только минимальный "пропускатель шума" для синхронного
//! query/response цикла.

use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, Error)]
pub enum QmpError {
    /// Не удалось подключиться к unix-сокету (QEMU ещё не успел поднять
    /// `-qmp unix:...,server,nowait`, либо процесс уже завершился).
    #[error("failed to connect to QMP socket {path}: {source}")]
    ConnectFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// Ошибка чтения/записи на уже установленном соединении.
    #[error("QMP I/O error: {0}")]
    Io(std::io::Error),

    /// Соединение закрылось раньше, чем пришёл ожидаемый ответ (например,
    /// QEMU-процесс завершился во время диалога).
    #[error("QMP connection closed unexpectedly")]
    ConnectionClosed,

    /// Полученная строка не является валидным JSON, либо не соответствует
    /// ожидаемой структуре ответа QMP.
    #[error("failed to parse QMP message: {0}")]
    ParseError(serde_json::Error),

    /// QEMU вернул `{"error": {...}}` на выполненную команду — это
    /// штатный механизм QMP сообщать об ошибках выполнения (например,
    /// `cont` на уже работающей машине), не баг клиента.
    #[error("QMP command `{command}` failed: class={class}, desc={desc}")]
    CommandFailed {
        command: String,
        class: String,
        desc: String,
    },
}

/// `{"error": {"class": "...", "desc": "..."}}` — структура ошибки QMP.
#[derive(Debug, Deserialize)]
struct QmpErrorPayload {
    class: String,
    desc: String,
}

/// Минимальная структура ответа на любую QMP-команду: либо `return`, либо
/// `error`, не оба одновременно — это гарантируется самим протоколом QMP,
/// не проверяется здесь дополнительно (если QEMU вернёт оба поля сразу,
/// это уже не контракт, который имеет смысл валидировать на стороне
/// клиента).
#[derive(Debug, Deserialize)]
struct QmpReply {
    #[serde(rename = "return")]
    return_value: Option<Value>,
    error: Option<QmpErrorPayload>,
}

/// Статус виртуальной машины, как его возвращает `query-status`.
///
/// `serde(other)` на последнем варианте — QMP может вернуть статусы, не
/// перечисленные явно (например, промежуточные состояния миграции), и
/// падать с ошибкой парсинга на незнакомом статусе хуже, чем явно
/// признать его неизвестным на уровне типа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VmStatus {
    Running,
    Paused,
    Shutdown,
    #[serde(other)]
    Other,
}

/// Тонкая обёртка над JSON-полем `status` в ответе `query-status`
/// (`{"return": {"status": "running", "running": true, ...}}`).
#[derive(Debug, Deserialize)]
struct QueryStatusReturn {
    status: VmStatus,
}

/// Подключение к QMP-сокету одного запущенного инстанса QEMU.
///
/// Каждый вызов команды (`pause`/`resume`/`query_status`) переиспользует
/// одно и то же соединение — оно не пересоздаётся на каждый вызов. Если
/// соединение разорвётся (например, процесс QEMU завершился), вызывающая
/// сторона (`backend.rs`) должна создать новый `QmpClient::connect`, а не
/// пытаться восстановить разорванное соединение здесь — на этом уровне
/// нет логики реконнекта.
pub struct QmpClient {
    stream: BufReader<UnixStream>,
}

impl QmpClient {
    /// Подключается к QMP-сокету и выполняет handshake
    /// (`qmp_capabilities`). Возвращает готовый к использованию клиент —
    /// после успешного `connect` можно сразу слать `pause`/`resume`/
    /// `query_status`, повторный handshake не требуется.
    pub async fn connect(socket_path: &Path) -> Result<Self, QmpError> {
        let raw_stream = UnixStream::connect(socket_path)
            .await
            .map_err(|source| QmpError::ConnectFailed {
                path: socket_path.display().to_string(),
                source,
            })?;

        let mut client = QmpClient {
            stream: BufReader::new(raw_stream),
        };

        // QEMU присылает greeting первым, без запроса с нашей стороны —
        // прочитываем и отбрасываем его: версия QEMU/список доступных
        // команд в greeting не нужны для текущего скоупа (pause/resume/
        // status), это не часть протокола negotiation как такового.
        // Явно указываем Value (не строгую структуру) — нам важно только
        // убедиться, что это валидный JSON, а не разбирать его поля.
        let _greeting: Value = client.read_line_as_json().await?;

        client.execute_raw("qmp_capabilities", None).await?;

        Ok(client)
    }

    /// `stop` — приостанавливает выполнение виртуальной машины (vCPU и
    /// устройства). Соответствует `HypervisorBackend::pause`.
    pub async fn pause(&mut self) -> Result<(), QmpError> {
        self.execute_raw("stop", None).await?;
        Ok(())
    }

    /// `cont` — возобновляет выполнение после `pause`. Соответствует
    /// `HypervisorBackend::resume`.
    pub async fn resume(&mut self) -> Result<(), QmpError> {
        self.execute_raw("cont", None).await?;
        Ok(())
    }

    /// `query-status` — текущий статус виртуальной машины с точки зрения
    /// QEMU (в отличие от `process::QemuProcess::is_alive`, которое знает
    /// только "процесс жив/не жив", не различая `Running`/`Paused`).
    pub async fn query_status(&mut self) -> Result<VmStatus, QmpError> {
        let value = self.execute_raw("query-status", None).await?;
        let parsed: QueryStatusReturn =
            serde_json::from_value(value).map_err(QmpError::ParseError)?;
        Ok(parsed.status)
    }

    /// `snapshot-save` — job-based snapshot API (QEMU 6.0+): сохраняет
    /// состояние гостя (CPU/RAM, поле `vmstate`) и состояние диска
    /// (поле `devices`, список node-name/id блочных узлов) под тегом
    /// `tag`. **Не** старый `human-monitor-command`+`savevm` — решение
    /// принято явно (см. README этого крейта).
    ///
    /// `device` — имя устройства (например, `"drive-disk0"`, как в
    /// `cmdline.rs`). И `vmstate`, и единственный элемент `devices`
    /// сейчас указывают на этот же узел — у инстанса один диск, второго
    /// места для хранения vmstate нет (см. doc-comment `DISK_DEVICE` в
    /// `backend.rs`: VARS-pflash сознательно не участвует в снапшоте).
    ///
    /// После вызова нужно ждать завершения через `wait_job_completion` —
    /// это асинхронный job, не синхронная команда: сам `snapshot-save`
    /// возвращает `{"return": {}}` сразу после постановки job в очередь,
    /// не после его реального завершения.
    pub async fn snapshot_save(
        &mut self,
        device: &str,
        tag: &str,
    ) -> Result<String, QmpError> {
        let job_id = format!("snap-{tag}");
        let args = json!({
            "job-id": &job_id,
            "tag": tag,
            "vmstate": device,
            "devices": [device],
        });
        self.execute_raw("snapshot-save", Some(args)).await?;
        Ok(job_id)
    }

    /// `snapshot-load` — восстанавливает состояние гостя и диска из
    /// снапшота `tag` (async job, та же схема аргументов, что у
    /// `snapshot-save`: `vmstate`+`devices`, не `device`).
    ///
    /// После вызова нужно ждать завершения через `wait_job_completion`.
    pub async fn snapshot_load(
        &mut self,
        device: &str,
        tag: &str,
    ) -> Result<String, QmpError> {
        let job_id = format!("load-{tag}");
        let args = json!({
            "job-id": &job_id,
            "tag": tag,
            "vmstate": device,
            "devices": [device],
        });
        self.execute_raw("snapshot-load", Some(args)).await?;
        Ok(job_id)
    }

    /// `snapshot-delete` — удаляет снапшот `tag` с перечисленных
    /// `devices` (async job). В отличие от `snapshot-save`/`-load`, эта
    /// команда не принимает `vmstate` — снапшот стирается только с
    /// блочных узлов, нет отдельного состояния ОЗУ/CPU для удаления.
    ///
    /// После вызова нужно ждать завершения через `wait_job_completion`.
    pub async fn snapshot_delete(
        &mut self,
        device: &str,
        tag: &str,
    ) -> Result<String, QmpError> {
        let job_id = format!("del-{tag}");
        let args = json!({
            "job-id": &job_id,
            "tag": tag,
            "devices": [device],
        });
        self.execute_raw("snapshot-delete", Some(args)).await?;
        Ok(job_id)
    }

    /// Ожидает завершения async job по `job_id`, poll-я через `query-jobs`.
    ///
    /// QEMU job-модель знает единственный терминальный статус —
    /// `"concluded"` (`created`/`running`/`paused`/`ready`/`standby`/
    /// `waiting`/`pending`/`aborting` — все промежуточные, не `"completed"`/
    /// `"failed"`/`"aborted"`, которых в реальном протоколе не существует).
    /// Успех/неудача неконкluded-job различается по полю `error`: оно
    /// отсутствует при успехе, содержит текст ошибки при провале job'а.
    /// После того как job дошёл до `"concluded"` (в любом исходе), нужно
    /// явно вызвать `job-dismiss` — иначе он навечно останется в
    /// `query-jobs`, заняв слот и оставшись видимым там же при следующем
    /// поллинге другого job'а.
    ///
    /// Максимальное время ожидания — `timeout`. Возвращает ошибку при
    /// таймауте или если job завершился с ошибкой.
    pub async fn wait_job_completion(
        &mut self,
        job_id: &str,
        timeout: std::time::Duration,
    ) -> Result<(), QmpError> {
        use std::time::Instant;
        let start = Instant::now();

        loop {
            let value = self.execute_raw("query-jobs", None).await?;
            let jobs: Vec<QueryJobInfo> =
                serde_json::from_value(value).map_err(QmpError::ParseError)?;

            if let Some(job) = jobs.iter().find(|j| j.id == job_id) {
                if job.status.as_deref() == Some("concluded") {
                    self.execute_raw("job-dismiss", Some(json!({ "id": job_id })))
                        .await?;

                    return match &job.error {
                        Some(error) => Err(QmpError::CommandFailed {
                            command: format!("job {job_id}"),
                            class: "GenericError".to_string(),
                            desc: error.clone(),
                        }),
                        None => Ok(()),
                    };
                }
                // created/running/paused/ready/standby/waiting/pending/
                // aborting — все промежуточные, продолжаем поллинг.
            }

            if start.elapsed() > timeout {
                return Err(QmpError::CommandFailed {
                    command: format!("wait for job {job_id}"),
                    class: "Timeout".to_string(),
                    desc: format!(
                        "job {job_id} did not reach status \"concluded\" within {:?}",
                        timeout
                    ),
                });
            }

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// `query-block` — возвращает список снапшотов для указанного устройства.
    pub async fn query_block_snapshots(
        &mut self,
        device: &str,
    ) -> Result<Vec<SnapshotInfo>, QmpError> {
        let value = self.execute_raw("query-block", None).await?;
        let blocks: Vec<BlockDeviceInfo> =
            serde_json::from_value(value).map_err(QmpError::ParseError)?;

        for block in blocks {
            if block.device.as_deref() == Some(device) {
                return Ok(block.snapshots.unwrap_or_default());
            }
        }

        Ok(Vec::new())
    }

    /// Отправляет команду `{"execute": command, "arguments": arguments?}`
    /// и возвращает содержимое поля `return` при успехе, либо
    /// `QmpError::CommandFailed` при `{"error": ...}` в ответе.
    async fn execute_raw(
        &mut self,
        command: &str,
        arguments: Option<Value>,
    ) -> Result<Value, QmpError> {
        let mut request = json!({ "execute": command });
        if let Some(args) = arguments {
            request["arguments"] = args;
        }

        let mut line = serde_json::to_string(&request).map_err(QmpError::ParseError)?;
        line.push('\n');

        self.stream
            .write_all(line.as_bytes())
            .await
            .map_err(QmpError::Io)?;
        self.stream.flush().await.map_err(QmpError::Io)?;

        let reply: QmpReply = self.read_reply_skipping_events().await?;

        match (reply.return_value, reply.error) {
            (Some(value), _) => Ok(value),
            (None, Some(err)) => Err(QmpError::CommandFailed {
                command: command.to_string(),
                class: err.class,
                desc: err.desc,
            }),
            (None, None) => {
                use serde::de::Error as _;
                Err(QmpError::ParseError(serde_json::Error::custom(format!(
                    "QMP reply to `{command}` has neither `return` nor `error` field"
                ))))
            }
        }
    }

    /// Читает строки до тех пор, пока не встретит сообщение, являющееся
    /// настоящим ответом на команду (`return`/`error`), пропуская любые
    /// промежуточные события (`{"event": ..., ...}`, например
    /// `JOB_STATUS_CHANGE`, которые QEMU присылает асинхронно по тому же
    /// сокету во время `snapshot-save`/`snapshot-load`/`snapshot-delete`
    /// job'ов — единственное место в этом клиенте, где события и ответы
    /// на команды могут перемежаться на одном соединении). Это не
    /// полноценная подписка на события — пропущенные события просто
    /// отбрасываются, недоступны вызывающей стороне; достаточно для
    /// синхронного query/response цикла с поллингом в
    /// `wait_job_completion`.
    async fn read_reply_skipping_events(&mut self) -> Result<QmpReply, QmpError> {
        loop {
            let raw: Value = self.read_line_as_json().await?;
            if raw.get("event").is_some() {
                continue;
            }
            let reply: QmpReply = serde_json::from_value(raw).map_err(QmpError::ParseError)?;
            return Ok(reply);
        }
    }

    /// Читает одну строку из сокета и парсит её как JSON указанного типа.
    async fn read_line_as_json<T: for<'de> Deserialize<'de>>(
        &mut self,
    ) -> Result<T, QmpError> {
        let mut line = String::new();
        let bytes_read = self
            .stream
            .read_line(&mut line)
            .await
            .map_err(QmpError::Io)?;

        if bytes_read == 0 {
            return Err(QmpError::ConnectionClosed);
        }

        serde_json::from_str(&line).map_err(QmpError::ParseError)
    }
}

/// Метаданные одного снапшота, полученные из `query-block`.
#[derive(Debug, Clone, Deserialize)]
pub struct SnapshotInfo {
    pub tag: String,
    pub id: String,
    #[serde(default)]
    pub vm_clock_nsec: Option<u64>,
    #[serde(default)]
    pub datetime: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlockDeviceInfo {
    #[serde(default)]
    device: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    removable: bool,
    #[serde(default, rename = "snapshot")]
    snapshots: Option<Vec<SnapshotInfo>>,
}

/// Структура ответа `query-jobs`.
#[derive(Debug, Deserialize)]
struct QueryJobInfo {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_status_parses_known_variants() {
        let running: VmStatus = serde_json::from_str("\"running\"").unwrap();
        assert_eq!(running, VmStatus::Running);

        let paused: VmStatus = serde_json::from_str("\"paused\"").unwrap();
        assert_eq!(paused, VmStatus::Paused);
    }

    #[test]
    fn vm_status_falls_back_to_other_on_unknown_variant() {
        let unknown: VmStatus = serde_json::from_str("\"inmigrate\"").unwrap();
        assert_eq!(unknown, VmStatus::Other);
    }

    #[test]
    fn query_status_return_parses_nested_status_field() {
        let json = r#"{"status": "paused", "running": false, "singlestep": false}"#;
        let parsed: QueryStatusReturn = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.status, VmStatus::Paused);
    }

    #[test]
    fn qmp_reply_parses_return_variant() {
        let json = r#"{"return": {}}"#;
        let parsed: QmpReply = serde_json::from_str(json).unwrap();
        assert!(parsed.return_value.is_some());
        assert!(parsed.error.is_none());
    }

    #[test]
    fn qmp_reply_parses_error_variant() {
        let json = r#"{"error": {"class": "GenericError", "desc": "something went wrong"}}"#;
        let parsed: QmpReply = serde_json::from_str(json).unwrap();
        assert!(parsed.return_value.is_none());
        let err = parsed.error.unwrap();
        assert_eq!(err.class, "GenericError");
        assert_eq!(err.desc, "something went wrong");
    }

    #[test]
    fn qmp_reply_with_neither_return_nor_error_parses_as_valid_struct() {
        // Сам QmpReply парсится без ошибки в этом случае (оба поля Option).
        // Раньше execute_raw трактовал такое сообщение как ошибку парсинга
        // (считая, что событий не бывает); теперь read_reply_skipping_events
        // (см. execute_raw_skips_async_events_before_the_real_reply ниже)
        // отфильтровывает события до парсинга в QmpReply вообще — этот тест
        // фиксирует только то, что сам тип не паникует/не падает на таком
        // JSON, если до него всё-таки дойдёт парсинг.
        let json = r#"{"event": "VNC_CONNECTED"}"#;
        let parsed: QmpReply = serde_json::from_str(json).unwrap();
        assert!(parsed.return_value.is_none());
        assert!(parsed.error.is_none());
    }

    /// Создаёт пару `QmpClient`+"фейковый QEMU" поверх `UnixStream::pair`
    /// — без реального `qemu-system-x86_64`, но с настоящим wire-форматом
    /// QMP (newline-delimited JSON). Пропускает greeting/`qmp_capabilities`
    /// handshake — конструирует `QmpClient` напрямую из готового сокета
    /// (доступно из `tests`-модуля того же файла), не через `connect`,
    /// так что фейковому концу не нужно изображать greeting.
    fn fake_qmp_pair() -> (QmpClient, UnixStream) {
        let (client_side, server_side) = UnixStream::pair().expect("unix socket pair");
        let client = QmpClient {
            stream: BufReader::new(client_side),
        };
        (client, server_side)
    }

    #[tokio::test]
    async fn snapshot_save_sends_devices_array_and_vmstate_not_singular_device() {
        // Регрессионный тест на реальный баг: первая версия этого кода
        // слала `"device": "drive-disk0"` (единственное число, без
        // `vmstate`) — реальный QEMU QMP `snapshot-save` (job-based API,
        // QEMU 6.0+) ожидает `"devices": [...]` (массив node-name) и
        // обязательный `"vmstate"`, иначе сразу отвечает `{"error": ...}`,
        // не запуская job вообще.
        let (mut client, server) = fake_qmp_pair();
        let (mut server_read, mut server_write) = tokio::io::split(server);

        let request_fut = tokio::spawn(async move {
            let mut buf = BufReader::new(&mut server_read);
            let mut line = String::new();
            buf.read_line(&mut line).await.expect("read request");
            let req: Value = serde_json::from_str(&line).expect("valid JSON");
            server_write
                .write_all(b"{\"return\": {}}\n")
                .await
                .expect("write reply");
            req
        });

        let job_id = client
            .snapshot_save("drive-disk0", "my-tag")
            .await
            .expect("snapshot_save should send the request and parse the reply");
        assert_eq!(job_id, "snap-my-tag");

        let req = request_fut.await.expect("server task did not panic");
        assert_eq!(req["execute"], "snapshot-save");
        let args = &req["arguments"];
        assert_eq!(args["job-id"], "snap-my-tag");
        assert_eq!(args["tag"], "my-tag");
        assert_eq!(args["vmstate"], "drive-disk0");
        assert_eq!(args["devices"], serde_json::json!(["drive-disk0"]));
        assert!(
            args.get("device").is_none(),
            "must not send the old singular `device` field"
        );
    }

    #[tokio::test]
    async fn snapshot_delete_sends_devices_array_without_vmstate() {
        let (mut client, server) = fake_qmp_pair();
        let (mut server_read, mut server_write) = tokio::io::split(server);

        let request_fut = tokio::spawn(async move {
            let mut buf = BufReader::new(&mut server_read);
            let mut line = String::new();
            buf.read_line(&mut line).await.expect("read request");
            let req: Value = serde_json::from_str(&line).expect("valid JSON");
            server_write
                .write_all(b"{\"return\": {}}\n")
                .await
                .expect("write reply");
            req
        });

        client
            .snapshot_delete("drive-disk0", "old-tag")
            .await
            .expect("snapshot_delete should succeed");

        let req = request_fut.await.expect("server task did not panic");
        assert_eq!(req["execute"], "snapshot-delete");
        let args = &req["arguments"];
        assert_eq!(args["devices"], serde_json::json!(["drive-disk0"]));
        assert!(
            args.get("vmstate").is_none(),
            "snapshot-delete has no vmstate parameter, unlike snapshot-save/-load"
        );
    }

    #[tokio::test]
    async fn wait_job_completion_treats_concluded_without_error_as_success() {
        // Регрессионный тест: первая версия проверяла статусы
        // "completed"/"failed"/"aborted", которых не существует в
        // реальной job-модели QEMU — единственный терминальный статус
        // "concluded", успех/провал различается по полю `error`.
        let (mut client, server) = fake_qmp_pair();
        let (mut server_read, mut server_write) = tokio::io::split(server);

        tokio::spawn(async move {
            let mut buf = BufReader::new(&mut server_read);

            // Первый запрос — query-jobs, отвечаем "concluded" без error.
            let mut line = String::new();
            buf.read_line(&mut line).await.unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(req["execute"], "query-jobs");
            server_write
                .write_all(
                    b"{\"return\": [{\"id\": \"snap-tag\", \"status\": \"concluded\"}]}\n",
                )
                .await
                .unwrap();

            // Второй запрос — job-dismiss, обязателен после concluded.
            let mut line = String::new();
            buf.read_line(&mut line).await.unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(req["execute"], "job-dismiss");
            assert_eq!(req["arguments"]["id"], "snap-tag");
            server_write
                .write_all(b"{\"return\": {}}\n")
                .await
                .unwrap();
        });

        client
            .wait_job_completion("snap-tag", std::time::Duration::from_secs(5))
            .await
            .expect("concluded job without error must be reported as success");
    }

    #[tokio::test]
    async fn wait_job_completion_treats_concluded_with_error_as_failure() {
        let (mut client, server) = fake_qmp_pair();
        let (mut server_read, mut server_write) = tokio::io::split(server);

        tokio::spawn(async move {
            let mut buf = BufReader::new(&mut server_read);

            let mut line = String::new();
            buf.read_line(&mut line).await.unwrap();
            server_write
                .write_all(
                    b"{\"return\": [{\"id\": \"snap-tag\", \"status\": \"concluded\", \"error\": \"device is in use\"}]}\n",
                )
                .await
                .unwrap();

            // job-dismiss всё равно должен быть вызван, даже при ошибке.
            let mut line = String::new();
            buf.read_line(&mut line).await.unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(req["execute"], "job-dismiss");
            server_write
                .write_all(b"{\"return\": {}}\n")
                .await
                .unwrap();
        });

        let err = client
            .wait_job_completion("snap-tag", std::time::Duration::from_secs(5))
            .await
            .expect_err("concluded job with `error` field must be reported as failure");
        assert!(matches!(err, QmpError::CommandFailed { .. }));
    }

    #[tokio::test]
    async fn wait_job_completion_keeps_polling_through_non_terminal_statuses() {
        let (mut client, server) = fake_qmp_pair();
        let (mut server_read, mut server_write) = tokio::io::split(server);

        tokio::spawn(async move {
            let mut buf = BufReader::new(&mut server_read);

            for status in ["created", "running", "pending"] {
                let mut line = String::new();
                buf.read_line(&mut line).await.unwrap();
                let reply = format!(
                    "{{\"return\": [{{\"id\": \"snap-tag\", \"status\": \"{status}\"}}]}}\n"
                );
                server_write.write_all(reply.as_bytes()).await.unwrap();
            }

            let mut line = String::new();
            buf.read_line(&mut line).await.unwrap();
            server_write
                .write_all(b"{\"return\": [{\"id\": \"snap-tag\", \"status\": \"concluded\"}]}\n")
                .await
                .unwrap();

            let mut line = String::new();
            buf.read_line(&mut line).await.unwrap();
            server_write.write_all(b"{\"return\": {}}\n").await.unwrap();
        });

        client
            .wait_job_completion("snap-tag", std::time::Duration::from_secs(5))
            .await
            .expect("must keep polling through non-terminal statuses and conclude eventually");
    }

    #[tokio::test]
    async fn execute_raw_skips_async_events_before_the_real_reply() {
        // Регрессионный тест: первая версия `execute_raw` трактовала любое
        // сообщение без `return`/`error` как ошибку парсинга — включая
        // легитимные асинхронные события (`JOB_STATUS_CHANGE` и другие),
        // которые QEMU может прислать по тому же сокету между отправкой
        // команды и получением её ответа, особенно во время
        // snapshot-job-поллинга.
        let (mut client, server) = fake_qmp_pair();
        let (mut server_read, mut server_write) = tokio::io::split(server);

        tokio::spawn(async move {
            let mut buf = BufReader::new(&mut server_read);
            let mut line = String::new();
            buf.read_line(&mut line).await.unwrap();

            // Шлём событие, затем ещё одно, затем настоящий ответ.
            server_write
                .write_all(b"{\"event\": \"JOB_STATUS_CHANGE\", \"data\": {}}\n")
                .await
                .unwrap();
            server_write
                .write_all(b"{\"event\": \"STOP\"}\n")
                .await
                .unwrap();
            server_write
                .write_all(b"{\"return\": {\"status\": \"running\"}}\n")
                .await
                .unwrap();
        });

        let value = client
            .execute_raw("query-status", None)
            .await
            .expect("events before the real reply must be skipped, not treated as errors");
        assert_eq!(value["status"], "running");
    }

    // Тесты, которым нужно реальное соединение с живым QMP-сокетом
    // (то есть запущенный qemu-system-x86_64) — недоступны в unit-test,
    // см. process.rs/backend.rs про конвенцию #[ignore] в этом крейте.

    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary with a live QMP socket, see docker/README.md integration-test target"]
    async fn connect_then_pause_then_resume_round_trip() {
        // Сценарий: запустить QemuProcess (см. process.rs), подключиться
        // QmpClient к её qmp_socket_path(), pause -> query_status == Paused,
        // resume -> query_status == Running. Не реализован как
        // самостоятельный тест здесь, чтобы не дублировать spawn-логику
        // из backend.rs/process.rs — полноценный сценарий покрывается
        // интеграционным тестом на уровне backend.rs
        // (spawn_then_status_then_stop_round_trip), куда естественно
        // добавить pause/resume-шаги при следующей итерации.
    }

    #[test]
    fn snapshot_info_parses_from_query_block() {
        let json = r#"[
            {
                "device": "drive0",
                "removable": false,
                "snapshot": [
                    {"tag": "backup1", "id": "1", "vm-clock-nsec": 12345, "datetime": "2024-01-15T10:30:00"},
                    {"tag": "backup2", "id": "2"}
                ]
            }
        ]"#;
        let blocks: Vec<BlockDeviceInfo> = serde_json::from_str(json).unwrap();
        assert_eq!(blocks.len(), 1);
        let snapshots = blocks[0].snapshots.as_ref().unwrap();
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].tag, "backup1");
        assert_eq!(snapshots[0].id, "1");
        assert_eq!(snapshots[0].datetime.as_deref(), Some("2024-01-15T10:30:00"));
        assert_eq!(snapshots[1].tag, "backup2");
        assert_eq!(snapshots[1].id, "2");
        assert!(snapshots[1].datetime.is_none());
    }

    #[test]
    fn snapshot_info_empty_list() {
        let json = r#"[
            {
                "device": "drive0",
                "removable": false,
                "snapshot": []
            }
        ]"#;
        let blocks: Vec<BlockDeviceInfo> = serde_json::from_str(json).unwrap();
        let snapshots = blocks[0].snapshots.as_ref().unwrap();
        assert!(snapshots.is_empty());
    }

    #[test]
    fn snapshot_info_missing_snapshot_field() {
        let json = r#"[
            {
                "device": "drive0",
                "removable": false
            }
        ]"#;
        let blocks: Vec<BlockDeviceInfo> = serde_json::from_str(json).unwrap();
        assert!(blocks[0].snapshots.is_none());
    }

    #[test]
    fn query_job_info_parses_concluded_without_error() {
        // "concluded" — единственный реальный терминальный статус job в
        // QEMU QMP; "completed"/"failed"/"aborted" (прошлая версия этого
        // теста) не существуют в протоколе.
        let json = r#"[{"id": "snap-backup1", "status": "concluded"}]"#;
        let jobs: Vec<QueryJobInfo> = serde_json::from_str(json).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "snap-backup1");
        assert_eq!(jobs[0].status.as_deref(), Some("concluded"));
        assert!(jobs[0].error.is_none());
    }

    #[test]
    fn query_job_info_parses_concluded_with_error() {
        // Провал job'а различается по присутствию `error`, не по
        // отдельному значению `status` — "concluded" одинаков для успеха
        // и провала.
        let json = r#"[{"id": "snap-backup1", "status": "concluded", "error": "device is in use"}]"#;
        let jobs: Vec<QueryJobInfo> = serde_json::from_str(json).unwrap();
        assert_eq!(jobs[0].status.as_deref(), Some("concluded"));
        assert_eq!(jobs[0].error.as_deref(), Some("device is in use"));
    }

    #[test]
    fn query_job_info_parses_non_terminal_status() {
        let json = r#"[{"id": "snap-backup1", "status": "running"}]"#;
        let jobs: Vec<QueryJobInfo> = serde_json::from_str(json).unwrap();
        assert_eq!(jobs[0].status.as_deref(), Some("running"));
    }
}
