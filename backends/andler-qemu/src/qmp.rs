//! Клиент QEMU Machine Protocol (QMP) поверх unix-сокета.
//!
//! Реализует команды управления инстансами: `stop`/`cont`/`query-status`
//! (pause/resume/status) и `snapshot-save`/`snapshot-load`/`snapshot-delete`/
//! `query-block`/`query-jobs` (снапшоты). Snapshot-команды используют
//! async job API QEMU: `snapshot-save/load/delete` запускают асинхронный job,
//! который завершается через `wait_job_completion` (polling `query-jobs`).
//!
//! Протокол: newline-delimited JSON поверх unix-сокета. При подключении
//! QEMU сразу присылает greeting (`{"QMP": {...}}`); клиент обязан
//! отправить `{"execute": "qmp_capabilities"}` прежде чем слать любые
//! другие команды — без этого QEMU отвечает ошибкой на всё, кроме
//! `qmp_capabilities` (режим "capabilities negotiation").

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

    /// `snapshot-save` — создаёт внутренний снапшот qcow2-диска (async job).
    ///
    /// После вызова нужно ждать завершения через `wait_job_completion`.
    /// `device` — имя устройства (например, `"drive-disk0"`, как в cmdline).
    /// `tag` — пользовательский идентификатор снапшота.
    pub async fn snapshot_save(
        &mut self,
        device: &str,
        tag: &str,
    ) -> Result<String, QmpError> {
        let job_id = format!("snap-{tag}");
        let args = json!({
            "job-id": &job_id,
            "device": device,
            "tag": tag,
        });
        self.execute_raw("snapshot-save", Some(args)).await?;
        Ok(job_id)
    }

    /// `snapshot-load` — восстанавливает инстанс из внутреннего снапшота (async job).
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
            "device": device,
            "tag": tag,
        });
        self.execute_raw("snapshot-load", Some(args)).await?;
        Ok(job_id)
    }

    /// `snapshot-delete` — удаляет внутренний снапшот qcow2-диска (async job).
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
            "device": device,
            "tag": tag,
        });
        self.execute_raw("snapshot-delete", Some(args)).await?;
        Ok(job_id)
    }

    /// Ожидает завершения async job по `job_id`, poll-я через `query-jobs`.
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
                match job.status.as_deref() {
                    Some("completed") => return Ok(()),
                    Some("failed") | Some("aborted") => {
                        return Err(QmpError::CommandFailed {
                            command: format!("job {job_id}"),
                            class: job.error.clone().unwrap_or_default(),
                            desc: job.error.clone().unwrap_or_else(|| "job failed".to_string()),
                        });
                    }
                    _ => {} // running, pending, etc. — keep polling
                }
            }

            if start.elapsed() > timeout {
                return Err(QmpError::CommandFailed {
                    command: format!("wait for job {job_id}"),
                    class: "Timeout".to_string(),
                    desc: format!(
                        "job {job_id} did not complete within {:?}",
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

        let reply: QmpReply = self.read_line_as_json().await?;

        match (reply.return_value, reply.error) {
            (Some(value), _) => Ok(value),
            (None, Some(err)) => Err(QmpError::CommandFailed {
                command: command.to_string(),
                class: err.class,
                desc: err.desc,
            }),
            // QMP-сообщение без return и без error — это либо событие
            // (asynchronous event, например VNC_CONNECTED), не ответ на
            // команду, либо нарушение протокола. На этом этапе клиент не
            // различает события от ответов на команды (нет очереди
            // событий) — такое сообщение считается ошибкой парсинга,
            // а не тихо игнорируется, чтобы не маскировать реальные
            // протокольные баги.
            (None, None) => {
                use serde::de::Error as _;
                Err(QmpError::ParseError(serde_json::Error::custom(format!(
                    "QMP reply to `{command}` has neither `return` nor `error` field; \
                     possibly an asynchronous event, not yet supported by this client"
                ))))
            }
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
        // Сам QmpReply парсится без ошибки в этом случае (оба поля Option) —
        // именно поэтому проверка "ни return, ни error" происходит в
        // execute_raw после парсинга, не на уровне serde. Этот тест
        // фиксирует то, что приходит в execute_raw для построения
        // QmpError::ParseError через serde::de::Error::custom.
        let json = r#"{"event": "VNC_CONNECTED"}"#;
        let parsed: QmpReply = serde_json::from_str(json).unwrap();
        assert!(parsed.return_value.is_none());
        assert!(parsed.error.is_none());
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
    fn query_job_info_parses_completed() {
        let json = r#"[{"id": "snap-backup1", "status": "completed"}]"#;
        let jobs: Vec<QueryJobInfo> = serde_json::from_str(json).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "snap-backup1");
        assert_eq!(jobs[0].status.as_deref(), Some("completed"));
    }

    #[test]
    fn query_job_info_parses_failed_with_error() {
        let json = r#"[{"id": "snap-backup1", "status": "failed", "error": "device is in use"}]"#;
        let jobs: Vec<QueryJobInfo> = serde_json::from_str(json).unwrap();
        assert_eq!(jobs[0].status.as_deref(), Some("failed"));
        assert_eq!(jobs[0].error.as_deref(), Some("device is in use"));
    }
}
