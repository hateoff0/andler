use std::path::Path;

use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, Error)]
pub enum QmpError {
    #[error("failed to connect to QMP socket {path}: {source}")]
    ConnectFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("QMP I/O error: {0}")]
    Io(std::io::Error),

    #[error("QMP connection closed unexpectedly")]
    ConnectionClosed,

    #[error("failed to parse QMP message: {0}")]
    ParseError(serde_json::Error),

    #[error("QMP command `{command}` failed: class={class}, desc={desc}")]
    CommandFailed {
        command: String,
        class: String,
        desc: String,
    },
}

#[derive(Debug, Deserialize)]
struct QmpErrorPayload {
    class: String,
    desc: String,
}

#[derive(Debug, Deserialize)]
struct QmpReply {
    #[serde(rename = "return")]
    return_value: Option<Value>,
    error: Option<QmpErrorPayload>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VmStatus {
    Running,
    Paused,
    Shutdown,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct QueryStatusReturn {
    status: VmStatus,
}

pub struct QmpClient {
    stream: BufReader<UnixStream>,
    read_timeout: Option<std::time::Duration>,
}

impl QmpClient {
    pub async fn connect(socket_path: &Path) -> Result<Self, QmpError> {
        let raw_stream =
            UnixStream::connect(socket_path)
                .await
                .map_err(|source| QmpError::ConnectFailed {
                    path: socket_path.display().to_string(),
                    source,
                })?;

        let mut client = QmpClient {
            stream: BufReader::new(raw_stream),
            read_timeout: None,
        };

        let _greeting: Value = client.read_line_as_json().await?;

        client.execute_raw("qmp_capabilities", None).await?;

        Ok(client)
    }

    pub async fn connect_agent(socket_path: &Path) -> Result<Self, QmpError> {
        // QEMU >=9 moved the guest agent protocol out of QMP: guest-* commands
        // are sent straight to the agent chardev socket (no greeting, no
        // capabilities negotiation) — QMP answers "has not been found" for them.
        let raw_stream =
            UnixStream::connect(socket_path)
                .await
                .map_err(|source| QmpError::ConnectFailed {
                    path: socket_path.display().to_string(),
                    source,
                })?;

        Ok(QmpClient {
            stream: BufReader::new(raw_stream),
            read_timeout: Some(std::time::Duration::from_secs(15)),
        })
    }

    pub async fn pause(&mut self) -> Result<(), QmpError> {
        self.execute_raw("stop", None).await?;
        Ok(())
    }

    pub async fn resume(&mut self) -> Result<(), QmpError> {
        self.execute_raw("cont", None).await?;
        Ok(())
    }

    pub async fn query_status(&mut self) -> Result<VmStatus, QmpError> {
        let value = self.execute_raw("query-status", None).await?;
        let parsed: QueryStatusReturn =
            serde_json::from_value(value).map_err(QmpError::ParseError)?;
        Ok(parsed.status)
    }

    pub async fn snapshot_save(&mut self, device: &str, tag: &str) -> Result<String, QmpError> {
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

    pub async fn snapshot_load(&mut self, device: &str, tag: &str) -> Result<String, QmpError> {
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

    pub async fn snapshot_delete(&mut self, device: &str, tag: &str) -> Result<String, QmpError> {
        let job_id = format!("del-{tag}");
        let args = json!({
            "job-id": &job_id,
            "tag": tag,
            "devices": [device],
        });
        self.execute_raw("snapshot-delete", Some(args)).await?;
        Ok(job_id)
    }

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

    pub async fn guest_ping(&mut self) -> Result<(), QmpError> {
        self.execute_raw("guest-ping", None).await?;
        Ok(())
    }

    pub async fn guest_exec(&mut self, path: &str, args: &[&str]) -> Result<u64, QmpError> {
        let qemu_args: Vec<Value> = args.iter().map(|a| json!(a)).collect();
        let input_data = json!({
            "path": path,
            "arg": qemu_args,
            "capture-output": true,
        });
        let value = self.execute_raw("guest-exec", Some(input_data)).await?;
        let pid =
            value
                .get("pid")
                .and_then(|p| p.as_u64())
                .ok_or_else(|| QmpError::CommandFailed {
                    command: "guest-exec".to_string(),
                    class: "ParseError".to_string(),
                    desc: "response missing 'pid' field".to_string(),
                })?;
        Ok(pid)
    }

    pub async fn guest_exec_status(&mut self, pid: u64) -> Result<GuestExecStatus, QmpError> {
        let value = self
            .execute_raw("guest-exec-status", Some(json!({ "pid": pid })))
            .await?;
        serde_json::from_value(value).map_err(QmpError::ParseError)
    }

    pub async fn guest_file_write(&mut self, path: &str, content: &str) -> Result<(), QmpError> {
        let handle_value = self
            .execute_raw(
                "guest-file-open",
                Some(json!({ "path": path, "mode": "w" })),
            )
            .await?;
        let handle = handle_value
            .as_i64()
            .ok_or_else(|| QmpError::CommandFailed {
                command: "guest-file-open".to_string(),
                class: "ParseError".to_string(),
                desc: "response missing 'handle' field".to_string(),
            })?;

        let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
        let write_value = self
            .execute_raw(
                "guest-file-write",
                Some(json!({ "handle": handle, "buf-b64": encoded })),
            )
            .await?;
        let count = write_value.get("count").and_then(|c| c.as_u64());
        if count != Some(content.len() as u64) {
            return Err(QmpError::CommandFailed {
                command: "guest-file-write".to_string(),
                class: "ParseError".to_string(),
                desc: format!("wrote {} of {} bytes", count.unwrap_or(0), content.len()),
            });
        }

        self.execute_raw("guest-file-close", Some(json!({ "handle": handle })))
            .await?;
        Ok(())
    }

    pub async fn is_guest_agent_available(&mut self) -> bool {
        self.guest_ping().await.is_ok()
    }

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

        let reply: QmpReply = match self.read_timeout {
            Some(timeout) => {
                match tokio::time::timeout(timeout, self.read_reply_skipping_events()).await {
                    Ok(result) => result?,
                    Err(_) => {
                        return Err(QmpError::CommandFailed {
                            command: command.to_string(),
                            class: "Timeout".to_string(),
                            desc: format!("no reply from QEMU within {timeout:?}"),
                        })
                    }
                }
            }
            None => self.read_reply_skipping_events().await?,
        };

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

    async fn read_line_as_json<T: for<'de> Deserialize<'de>>(&mut self) -> Result<T, QmpError> {
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
    #[allow(dead_code)] // deserialized from query-block but not read; kept for structural completeness
    #[serde(default)]
    removable: bool,
    #[serde(default, rename = "snapshot")]
    snapshots: Option<Vec<SnapshotInfo>>,
}

#[derive(Debug, Deserialize)]
struct QueryJobInfo {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuestExecStatus {
    pub exitcode: Option<i64>,

    pub exited: bool,

    #[serde(default, rename = "out-data")]
    pub out_data: Option<String>,

    #[serde(default, rename = "err-data")]
    pub err_data: Option<String>,
}

/// QMP guest-exec-status's out-data/err-data are base64-encoded (binary-safe) per the
/// QEMU Guest Agent protocol — this decodes one, falling back to the raw string if it
/// somehow isn't valid base64 rather than silently dropping it.
pub fn decode_guest_exec_data(data: Option<String>) -> Option<String> {
    use base64::Engine;
    data.map(|raw| {
        base64::engine::general_purpose::STANDARD
            .decode(raw.trim())
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .unwrap_or(raw)
    })
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
    fn guest_exec_status_deserializes_kebab_case_out_and_err_data() {
        // real QMP wire format: kebab-case, base64-encoded payload
        let json = r#"{"exitcode":0,"exited":true,"out-data":"aGVsbG8=","err-data":"b29wcw=="}"#;
        let status: GuestExecStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.out_data.as_deref(), Some("aGVsbG8="));
        assert_eq!(status.err_data.as_deref(), Some("b29wcw=="));
    }

    #[test]
    fn guest_exec_status_missing_data_fields_default_to_none() {
        let json = r#"{"exitcode":0,"exited":true}"#;
        let status: GuestExecStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.out_data, None);
        assert_eq!(status.err_data, None);
    }

    #[test]
    fn guest_exec_status_running_process_omits_exitcode() {
        let json = r#"{"exited":false}"#;
        let status: GuestExecStatus = serde_json::from_str(json).unwrap();
        assert!(!status.exited);
        assert_eq!(status.exitcode, None);
    }

    #[test]
    fn decode_guest_exec_data_decodes_base64() {
        assert_eq!(
            decode_guest_exec_data(Some("aGVsbG8=".to_string())),
            Some("hello".to_string())
        );
    }

    #[test]
    fn decode_guest_exec_data_falls_back_to_raw_on_invalid_base64() {
        assert_eq!(
            decode_guest_exec_data(Some("not valid base64!!".to_string())),
            Some("not valid base64!!".to_string())
        );
    }

    #[test]
    fn decode_guest_exec_data_passes_through_none() {
        assert_eq!(decode_guest_exec_data(None), None);
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
        let json = r#"{"event": "VNC_CONNECTED"}"#;
        let parsed: QmpReply = serde_json::from_str(json).unwrap();
        assert!(parsed.return_value.is_none());
        assert!(parsed.error.is_none());
    }

    fn fake_qmp_pair() -> (QmpClient, UnixStream) {
        let (client_side, server_side) = UnixStream::pair().expect("unix socket pair");
        let client = QmpClient {
            stream: BufReader::new(client_side),
            read_timeout: None,
        };
        (client, server_side)
    }

    #[tokio::test]
    async fn snapshot_save_sends_devices_array_and_vmstate_not_singular_device() {
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
        let (mut client, server) = fake_qmp_pair();
        let (mut server_read, mut server_write) = tokio::io::split(server);

        tokio::spawn(async move {
            let mut buf = BufReader::new(&mut server_read);

            let mut line = String::new();
            buf.read_line(&mut line).await.unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(req["execute"], "query-jobs");
            server_write
                .write_all(b"{\"return\": [{\"id\": \"snap-tag\", \"status\": \"concluded\"}]}\n")
                .await
                .unwrap();

            let mut line = String::new();
            buf.read_line(&mut line).await.unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(req["execute"], "job-dismiss");
            assert_eq!(req["arguments"]["id"], "snap-tag");
            server_write.write_all(b"{\"return\": {}}\n").await.unwrap();
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

            let mut line = String::new();
            buf.read_line(&mut line).await.unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(req["execute"], "job-dismiss");
            server_write.write_all(b"{\"return\": {}}\n").await.unwrap();
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
        let (mut client, server) = fake_qmp_pair();
        let (mut server_read, mut server_write) = tokio::io::split(server);

        tokio::spawn(async move {
            let mut buf = BufReader::new(&mut server_read);
            let mut line = String::new();
            buf.read_line(&mut line).await.unwrap();

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

    #[tokio::test]
    #[ignore = "requires qemu-system-x86_64 binary with a live QMP socket, see docker/README.md integration-test target"]
    async fn connect_then_pause_then_resume_round_trip() {}

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
        assert_eq!(
            snapshots[0].datetime.as_deref(),
            Some("2024-01-15T10:30:00")
        );
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
        let json = r#"[{"id": "snap-backup1", "status": "concluded"}]"#;
        let jobs: Vec<QueryJobInfo> = serde_json::from_str(json).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "snap-backup1");
        assert_eq!(jobs[0].status.as_deref(), Some("concluded"));
        assert!(jobs[0].error.is_none());
    }

    #[test]
    fn query_job_info_parses_concluded_with_error() {
        let json =
            r#"[{"id": "snap-backup1", "status": "concluded", "error": "device is in use"}]"#;
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

    #[tokio::test]
    async fn connect_agent_sends_commands_without_qmp_handshake() {
        use tokio::net::UnixListener;

        let sock =
            std::env::temp_dir().join(format!("andler-qga-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = BufReader::new(&mut stream);
            let mut line = String::new();
            buf.read_line(&mut line).await.unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut stream, b"{\"return\": {}}\n")
                .await
                .unwrap();
            line
        });

        let mut client = QmpClient::connect_agent(&sock).await.unwrap();
        client.guest_ping().await.unwrap();

        let first_line = server.await.unwrap();
        let req: Value = serde_json::from_str(&first_line).unwrap();
        assert_eq!(
            req["execute"], "guest-ping",
            "first message on the agent channel must be the command itself, \
             with no greeting or qmp_capabilities in between"
        );
    }

    #[tokio::test]
    async fn guest_file_write_uses_buf_b64_wire_parameter() {
        let (mut client, server) = fake_qmp_pair();
        let (mut server_read, mut server_write) = tokio::io::split(server);

        let server_task = tokio::spawn(async move {
            let mut buf = BufReader::new(&mut server_read);
            let mut requests = Vec::new();
            let replies: [&[u8]; 3] = [
                b"{\"return\": 5}\n",
                b"{\"return\": {\"count\": 5}}\n",
                b"{\"return\": {}}\n",
            ];
            for reply in replies {
                let mut line = String::new();
                buf.read_line(&mut line).await.unwrap();
                requests.push(serde_json::from_str::<Value>(&line).unwrap());
                server_write.write_all(reply).await.unwrap();
            }
            requests
        });

        client
            .guest_file_write("/etc/andler/display.conf", "hello")
            .await
            .unwrap();

        let requests = server_task.await.unwrap();
        assert_eq!(requests[0]["execute"], "guest-file-open");
        assert_eq!(requests[1]["execute"], "guest-file-write");
        assert_eq!(
            requests[1]["arguments"]["buf-b64"],
            base64::engine::general_purpose::STANDARD.encode("hello")
        );
        assert!(
            requests[1]["arguments"].get("data-b64").is_none(),
            "wire parameter is buf-b64, not data-b64 (qga rejects the latter)"
        );
        assert_eq!(requests[2]["execute"], "guest-file-close");
    }

    #[tokio::test]
    async fn connect_agent_times_out_when_agent_is_silent() {
        let (mut client, server) = fake_qmp_pair();
        let (server_read, _server_write) = tokio::io::split(server);
        // keep the socket open but never reply
        drop(_server_write);
        let _alive_reader = server_read;

        client.read_timeout = Some(std::time::Duration::from_millis(100));
        let err = client.guest_ping().await.unwrap_err();
        assert!(
            err.to_string().contains("Timeout"),
            "expected timeout error, got: {err}"
        );
    }
}
