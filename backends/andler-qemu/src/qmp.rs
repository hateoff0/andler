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

    #[error("hot-unplug of `{device_id}` was not acknowledged by the guest within the timeout — unplug is guest-driven, so a running VM with a loaded virtio driver must release the device first; the backing file is untouched")]
    GuestUnplugTimeout { device_id: String },
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

#[derive(Debug, Deserialize)]
struct ProcessInfo {
    pid: u32,
}

#[derive(Debug, Deserialize)]
struct NamedBlockNode {
    #[serde(rename = "node-name")]
    node_name: String,
    #[serde(rename = "drv", default)]
    driver: Option<String>,
    /// QEMU ≥ 9 reports the backing file path as a plain string here; older
    /// QEMU nests `{"driver": "file", "filename": <path>}`.
    #[serde(default)]
    file: Option<serde_json::Value>,
}

fn node_file_name(file: &serde_json::Value) -> Option<String> {
    match file {
        serde_json::Value::String(path) => Some(path.clone()),
        serde_json::Value::Object(map) => map
            .get("filename")
            .and_then(|f| f.as_str())
            .map(str::to_string),
        _ => None,
    }
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

    /// Resolves the host PID of the QEMU process owning this QMP connection
    /// via `query-processes` (the QAPI ProcessInfo list; the first entry is
    /// the emulator process itself). Used by reconnect/adopt: the socket
    /// proves identity, this gives the pid to pidfd-wait on.
    pub async fn query_process_pid(&mut self) -> Result<u32, QmpError> {
        let value = self.execute_raw("query-processes", None).await?;
        let processes: Vec<ProcessInfo> =
            serde_json::from_value(value).map_err(QmpError::ParseError)?;
        processes
            .into_iter()
            .next()
            .map(|info| info.pid)
            .ok_or_else(|| {
                QmpError::ParseError(serde_json::Error::io(std::io::Error::other(
                    "query-processes returned an empty list",
                )))
            })
    }

    pub async fn snapshot_save(&mut self, device: &str, tag: &str) -> Result<(), QmpError> {
        let args = json!({
            "device": device,
            "name": tag,
        });
        self.execute_raw("blockdev-snapshot-internal-sync", Some(args))
            .await?;
        Ok(())
    }

    pub async fn snapshot_delete(&mut self, device: &str, tag: &str) -> Result<(), QmpError> {
        let args = json!({
            "device": device,
            "name": tag,
        });
        self.execute_raw("blockdev-snapshot-delete-internal-sync", Some(args))
            .await?;
        Ok(())
    }

    /// Attaches an already-created external overlay to the graph without
    /// opening its backing file (`backing: null`): the parent is bound by
    /// the subsequent `blockdev_snapshot`. `locking: "off"` is mandatory —
    /// the default OFD write-lock on the overlay file conflicts with the
    /// lock QEMU already holds on itself.
    pub async fn blockdev_add_overlay(
        &mut self,
        node_name: &str,
        file_path: &str,
    ) -> Result<(), QmpError> {
        let args = json!({
            "driver": "qcow2",
            "node-name": node_name,
            "discard": "unmap",
            "detect-zeroes": "on",
            "backing": null,
            "file": {
                "driver": "file",
                "filename": file_path,
                "aio": "threads",
                "locking": "off",
            },
        });
        self.execute_raw("blockdev-add", Some(args)).await?;
        Ok(())
    }

    /// Finds the name of the block graph's head node — the qcow2 node whose
    /// file is `active_filename` (the instance's `disk.qcow2`). After an
    /// external snapshot the head is the newest overlay node, not
    /// `drive-disk0` (busy as the overlay's backing), so a second snapshot
    /// must target the current head.
    pub async fn query_head_node_name(
        &mut self,
        active_filename: &str,
    ) -> Result<String, QmpError> {
        let value = self.execute_raw("query-named-block-nodes", None).await?;
        let nodes: Vec<NamedBlockNode> =
            serde_json::from_value(value).map_err(QmpError::ParseError)?;
        for node in nodes {
            if node.driver.as_deref() != Some("qcow2") {
                continue;
            }
            let Some(filename) = node.file.as_ref().and_then(node_file_name) else {
                continue;
            };
            if filename == active_filename {
                return Ok(node.node_name);
            }
        }
        Err(QmpError::CommandFailed {
            command: "query-named-block-nodes".to_string(),
            class: "GenericError".to_string(),
            desc: format!("no qcow2 node with file {active_filename}"),
        })
    }

    /// Switches the node `node` to write to the overlay `overlay`, making
    /// the overlay the new head of the chain.
    pub async fn blockdev_snapshot(&mut self, node: &str, overlay: &str) -> Result<(), QmpError> {
        let args = json!({
            "node": node,
            "overlay": overlay,
        });
        self.execute_raw("blockdev-snapshot", Some(args)).await?;
        Ok(())
    }

    /// Starts a block-commit job that merges everything between `top_node`
    /// and `base_node` into `base_node`. Auto-finalize but NOT auto-dismiss:
    /// the job stays visible in `query-jobs` until `wait_job_completion`
    /// dismisses it, so completion is observable instead of silently
    /// vanishing from the job list.
    pub async fn block_commit(
        &mut self,
        job_id: &str,
        top_node: &str,
        base_node: &str,
    ) -> Result<(), QmpError> {
        let args = json!({
            "job-id": job_id,
            "device": top_node,
            "top-node": top_node,
            "base-node": base_node,
            "auto-finalize": true,
            "auto-dismiss": false,
        });
        self.execute_raw("block-commit", Some(args)).await?;
        Ok(())
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

    pub async fn blockdev_add(
        &mut self,
        node_name: &str,
        file_path: &str,
        format: &str,
    ) -> Result<(), QmpError> {
        let args = json!({
            "driver": format,
            "node-name": node_name,
            "discard": "unmap",
            "detect-zeroes": "on",
            "file": {
                "driver": "file",
                "filename": file_path,
                "aio": "threads",
            },
        });
        self.execute_raw("blockdev-add", Some(args)).await?;
        Ok(())
    }

    pub async fn device_add_block(
        &mut self,
        id: &str,
        drive: &str,
        index: usize,
    ) -> Result<(), QmpError> {
        let args = json!({
            "driver": "virtio-blk-pci",
            "id": id,
            "drive": drive,
            "bus": format!("root-port-{}", index % 8),
        });
        self.execute_raw("device_add", Some(args)).await?;
        Ok(())
    }

    pub async fn device_add_net(
        &mut self,
        id: &str,
        netdev: &str,
        model: &str,
        index: usize,
    ) -> Result<(), QmpError> {
        let args = json!({
            "driver": model,
            "id": id,
            "netdev": netdev,
            "bus": format!("root-port-{}", 8 + index % 8),
        });
        self.execute_raw("device_add", Some(args)).await?;
        Ok(())
    }

    pub async fn device_del(&mut self, id: &str) -> Result<(), QmpError> {
        let args = json!({ "id": id });
        self.execute_raw("device_del", Some(args)).await?;
        Ok(())
    }

    pub async fn blockdev_del(&mut self, node_name: &str) -> Result<(), QmpError> {
        let args = json!({ "node-name": node_name });
        self.execute_raw("blockdev-del", Some(args)).await?;
        Ok(())
    }

    pub async fn netdev_add_user(&mut self, id: &str) -> Result<(), QmpError> {
        let args = json!({ "type": "user", "id": id });
        self.execute_raw("netdev_add", Some(args)).await?;
        Ok(())
    }

    pub async fn netdev_add_passt(&mut self, id: &str) -> Result<(), QmpError> {
        let args = json!({ "type": "passt", "id": id });
        self.execute_raw("netdev_add", Some(args)).await?;
        Ok(())
    }

    pub async fn netdev_add_tap(&mut self, id: &str, ifname: &str) -> Result<(), QmpError> {
        let args = json!({
            "type": "tap",
            "id": id,
            "ifname": ifname,
            "script": "no",
            "downscript": "no",
        });
        self.execute_raw("netdev_add", Some(args)).await?;
        Ok(())
    }

    pub async fn netdev_del(&mut self, id: &str) -> Result<(), QmpError> {
        let args = json!({ "id": id });
        self.execute_raw("netdev_del", Some(args)).await?;
        Ok(())
    }

    /// `device_del` is asynchronous: QEMU only detaches the PCI device after the
    /// guest acknowledges the unplug request, so `blockdev-del` keeps failing
    /// with "Device is in use" until then. Retries only that error until it
    /// succeeds or `timeout` elapses; any other failure is returned immediately.
    pub async fn detach_block_device(
        &mut self,
        device_id: &str,
        node_name: &str,
        timeout: std::time::Duration,
    ) -> Result<(), QmpError> {
        self.device_del(device_id).await?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.blockdev_del(node_name).await {
                Ok(()) => return Ok(()),
                Err(err)
                    if Self::is_device_in_use_error(&err)
                        && std::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
                Err(err) if Self::is_device_in_use_error(&err) => {
                    return Err(QmpError::GuestUnplugTimeout {
                        device_id: device_id.to_string(),
                    });
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// Same async-release retry as [`Self::detach_block_device`], for NICs:
    /// `netdev-del` fails with "is in use" until the guest releases the NIC.
    pub async fn detach_net_device(
        &mut self,
        device_id: &str,
        netdev_id: &str,
        timeout: std::time::Duration,
    ) -> Result<(), QmpError> {
        self.device_del(device_id).await?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.netdev_del(netdev_id).await {
                Ok(()) => return Ok(()),
                Err(err)
                    if Self::is_device_in_use_error(&err)
                        && std::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
                Err(err) if Self::is_device_in_use_error(&err) => {
                    return Err(QmpError::GuestUnplugTimeout {
                        device_id: device_id.to_string(),
                    });
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn is_device_in_use_error(err: &QmpError) -> bool {
        match err {
            QmpError::CommandFailed { class, desc, .. } => {
                class == "DeviceInUse" || desc.to_ascii_lowercase().contains("in use")
            }
            _ => false,
        }
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
    fn query_processes_return_parses_pid_list() {
        let json = r#"[{"pid": 4242, "cpu-time": 57163828, "cpu": 12.35}]"#;
        let parsed: Vec<ProcessInfo> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].pid, 4242);
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
    async fn snapshot_save_sends_internal_sync_device_and_name() {
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
            .snapshot_save("drive-disk0", "my-tag")
            .await
            .expect("snapshot_save should send the request and parse the reply");

        let req = request_fut.await.expect("server task did not panic");
        assert_eq!(req["execute"], "blockdev-snapshot-internal-sync");
        let args = &req["arguments"];
        assert_eq!(args["device"], "drive-disk0");
        assert_eq!(args["name"], "my-tag");
        assert!(
            args.get("vmstate").is_none(),
            "disk-only snapshots must not send vmstate"
        );
        assert!(
            args.get("devices").is_none(),
            "must not send the job-API `devices` array"
        );
        assert!(
            args.get("job-id").is_none(),
            "must not send the job-API `job-id`"
        );
    }

    #[tokio::test]
    async fn blockdev_add_overlay_sends_locking_off_and_null_backing() {
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
            .blockdev_add_overlay("snap-abc123", "/tmp/x/.tmp-abc123.qcow2")
            .await
            .expect("blockdev_add_overlay should succeed");

        let req = request_fut.await.expect("server task did not panic");
        assert_eq!(req["execute"], "blockdev-add");
        let args = &req["arguments"];
        assert_eq!(args["driver"], "qcow2");
        assert_eq!(args["node-name"], "snap-abc123");
        assert_eq!(args["file"]["filename"], "/tmp/x/.tmp-abc123.qcow2");
        assert_eq!(
            args["file"]["locking"], "off",
            "overlay must not take an OFD write-lock (conflicts with QEMU's own)"
        );
        assert!(
            args.get("backing").is_none() || args["backing"].is_null(),
            "overlay must attach with backing: null; the parent is bound by blockdev-snapshot"
        );
    }

    #[tokio::test]
    async fn query_head_node_name_finds_node_by_active_filename() {
        let (mut client, server) = fake_qmp_pair();
        let (mut server_read, mut server_write) = tokio::io::split(server);

        // QEMU >= 9 shape: `file` is a plain string, `drv` names the driver.
        let request_fut = tokio::spawn(async move {
            let mut buf = BufReader::new(&mut server_read);
            let mut line = String::new();
            buf.read_line(&mut line).await.expect("read request");
            let req: Value = serde_json::from_str(&line).expect("valid JSON");
            server_write
                .write_all(
                    b"{\"return\": [{\"node-name\": \"drive-disk0\", \"drv\": \"qcow2\", \"file\": \"/i/disk.qcow2\", \"children\": [{\"node-name\": \"file-disk0\", \"child\": \"file\"}]}, {\"node-name\": \"file-disk0\", \"drv\": \"file\", \"file\": \"/i/disk.qcow2\", \"children\": []}, {\"node-name\": \"snap-abc123\", \"drv\": \"qcow2\", \"file\": \"/i/disk.snapshots/.tmp-abc123.qcow2\", \"children\": []}]}\n",
                )
                .await
                .expect("write reply");
            req
        });

        let head = client
            .query_head_node_name("/i/disk.qcow2")
            .await
            .expect("head node must be found");
        assert_eq!(
            head, "drive-disk0",
            "the file driver node must not win over the qcow2 node"
        );

        let req = request_fut.await.expect("server task did not panic");
        assert_eq!(req["execute"], "query-named-block-nodes");
    }

    #[tokio::test]
    async fn query_head_node_name_accepts_nested_file_object() {
        let (mut client, server) = fake_qmp_pair();
        let (mut server_read, mut server_write) = tokio::io::split(server);

        // QEMU <= 8 shape: `file` nests `{"driver": "file", "filename": ...}`.
        let request_fut = tokio::spawn(async move {
            let mut buf = BufReader::new(&mut server_read);
            let mut line = String::new();
            buf.read_line(&mut line).await.expect("read request");
            server_write
                .write_all(
                    b"{\"return\": [{\"node-name\": \"drive-disk0\", \"drv\": \"qcow2\", \"file\": {\"driver\": \"file\", \"filename\": \"/i/disk.qcow2\"}}]}\n",
                )
                .await
                .expect("write reply");
        });

        let head = client
            .query_head_node_name("/i/disk.qcow2")
            .await
            .expect("head node must be found");
        assert_eq!(head, "drive-disk0");
        request_fut.await.expect("server task did not panic");
    }

    #[tokio::test]
    async fn blockdev_snapshot_switches_node_to_overlay() {
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
            .blockdev_snapshot("drive-disk0", "snap-abc123")
            .await
            .expect("blockdev_snapshot should succeed");

        let req = request_fut.await.expect("server task did not panic");
        assert_eq!(req["execute"], "blockdev-snapshot");
        let args = &req["arguments"];
        assert_eq!(args["node"], "drive-disk0");
        assert_eq!(args["overlay"], "snap-abc123");
    }

    #[tokio::test]
    async fn block_commit_sends_job_with_auto_finalize_only() {
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
            .block_commit("jc1", "snap-mid", "drive-disk0")
            .await
            .expect("block_commit should succeed");

        let req = request_fut.await.expect("server task did not panic");
        assert_eq!(req["execute"], "block-commit");
        let args = &req["arguments"];
        assert_eq!(args["job-id"], "jc1");
        assert_eq!(args["device"], "snap-mid");
        assert_eq!(args["top-node"], "snap-mid");
        assert_eq!(args["base-node"], "drive-disk0");
        assert_eq!(args["auto-finalize"], true);
        assert_eq!(
            args["auto-dismiss"], false,
            "job must stay visible so wait_job_completion can observe it"
        );
    }

    #[tokio::test]
    async fn snapshot_delete_sends_internal_delete_device_and_name() {
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
        assert_eq!(req["execute"], "blockdev-snapshot-delete-internal-sync");
        let args = &req["arguments"];
        assert_eq!(args["device"], "drive-disk0");
        assert_eq!(args["name"], "old-tag");
        assert!(
            args.get("vmstate").is_none(),
            "disk-only delete has no vmstate parameter"
        );
        assert!(
            args.get("devices").is_none(),
            "must not send the job-API `devices` array"
        );
        assert!(
            args.get("job-id").is_none(),
            "must not send the job-API `job-id`"
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
    #[ignore = "requires qemu-system-x86_64 binary with a live QMP socket, see docker/e2e/README.md integration-test target"]
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

    async fn run_with_server(
        server: UnixStream,
        replies: Vec<&'static [u8]>,
        op: impl std::future::Future<Output = Result<(), QmpError>>,
    ) -> Vec<Value> {
        let (mut server_read, mut server_write) = tokio::io::split(server);
        let server_task = tokio::spawn(async move {
            let mut buf = BufReader::new(&mut server_read);
            let mut requests = Vec::new();
            for reply in replies {
                let mut line = String::new();
                buf.read_line(&mut line).await.unwrap();
                requests.push(serde_json::from_str::<Value>(&line).unwrap());
                server_write.write_all(reply).await.unwrap();
            }
            requests
        });
        op.await.unwrap();
        server_task.await.unwrap()
    }

    #[tokio::test]
    async fn blockdev_add_sends_driver_node_and_file() {
        let (mut client, server) = fake_qmp_pair();
        let requests = run_with_server(server, vec![b"{\"return\": {}}\n"], async {
            client
                .blockdev_add("drive-extra0", "/data/extra.qcow2", "qcow2")
                .await
        })
        .await;
        let req = &requests[0];
        assert_eq!(req["execute"], "blockdev-add");
        let args = &req["arguments"];
        assert_eq!(args["driver"], "qcow2");
        assert_eq!(args["node-name"], "drive-extra0");
        assert_eq!(args["discard"], "unmap");
        assert_eq!(args["detect-zeroes"], "on");
        assert_eq!(args["file"]["driver"], "file");
        assert_eq!(args["file"]["filename"], "/data/extra.qcow2");
        assert_eq!(args["file"]["aio"], "threads");
    }

    #[tokio::test]
    async fn device_add_block_sends_virtio_blk_pci() {
        let (mut client, server) = fake_qmp_pair();
        let requests = run_with_server(server, vec![b"{\"return\": {}}\n"], async {
            client.device_add_block("extra0", "drive-extra0", 0).await
        })
        .await;
        let req = &requests[0];
        assert_eq!(req["execute"], "device_add");
        let args = &req["arguments"];
        assert_eq!(args["driver"], "virtio-blk-pci");
        assert_eq!(args["id"], "extra0");
        assert_eq!(args["drive"], "drive-extra0");
        assert_eq!(args["bus"], "root-port-0");
    }

    #[tokio::test]
    async fn device_add_net_sends_model_id_netdev() {
        let (mut client, server) = fake_qmp_pair();
        let requests = run_with_server(server, vec![b"{\"return\": {}}\n"], async {
            client
                .device_add_net("net-extra0", "net-extra0", "virtio-net-pci", 1)
                .await
        })
        .await;
        let req = &requests[0];
        assert_eq!(req["execute"], "device_add");
        let args = &req["arguments"];
        assert_eq!(args["driver"], "virtio-net-pci");
        assert_eq!(args["id"], "net-extra0");
        assert_eq!(args["netdev"], "net-extra0");
        assert_eq!(args["bus"], "root-port-9");
    }

    #[tokio::test]
    async fn netdev_add_sends_type_per_backend() {
        let (mut client, server) = fake_qmp_pair();
        let requests = run_with_server(
            server,
            vec![
                b"{\"return\": {}}\n",
                b"{\"return\": {}}\n",
                b"{\"return\": {}}\n",
            ],
            async {
                client.netdev_add_user("net-extra0").await?;
                client.netdev_add_passt("net-extra1").await?;
                client.netdev_add_tap("net-extra2", "andler-e2").await
            },
        )
        .await;
        assert_eq!(requests[0]["execute"], "netdev_add");
        assert_eq!(requests[0]["arguments"]["type"], "user");
        assert_eq!(requests[0]["arguments"]["id"], "net-extra0");
        assert_eq!(requests[1]["arguments"]["type"], "passt");
        assert_eq!(requests[2]["arguments"]["type"], "tap");
        assert_eq!(requests[2]["arguments"]["ifname"], "andler-e2");
        assert_eq!(requests[2]["arguments"]["script"], "no");
        assert_eq!(requests[2]["arguments"]["downscript"], "no");
    }

    #[tokio::test]
    async fn detach_block_device_sends_device_del_then_blockdev_del() {
        let (mut client, server) = fake_qmp_pair();
        let requests = run_with_server(
            server,
            vec![b"{\"return\": {}}\n", b"{\"return\": {}}\n"],
            async {
                client
                    .detach_block_device(
                        "extra0",
                        "drive-extra0",
                        std::time::Duration::from_secs(5),
                    )
                    .await
            },
        )
        .await;
        assert_eq!(requests[0]["execute"], "device_del");
        assert_eq!(requests[0]["arguments"]["id"], "extra0");
        assert_eq!(requests[1]["execute"], "blockdev-del");
        assert_eq!(requests[1]["arguments"]["node-name"], "drive-extra0");
    }

    #[tokio::test]
    async fn detach_block_device_retries_blockdev_del_while_device_in_use() {
        let (mut client, server) = fake_qmp_pair();
        let error = b"{\"error\": {\"class\": \"DeviceInUse\", \"desc\": \"Device 'drive-extra0' is in use\"}}\n";
        let requests = run_with_server(
            server,
            vec![b"{\"return\": {}}\n", error, b"{\"return\": {}}\n"],
            async {
                client
                    .detach_block_device(
                        "extra0",
                        "drive-extra0",
                        std::time::Duration::from_secs(5),
                    )
                    .await
            },
        )
        .await;
        assert_eq!(
            requests.len(),
            3,
            "device_del + failed blockdev-del + retried blockdev-del"
        );
        assert_eq!(requests[1]["execute"], "blockdev-del");
        assert_eq!(requests[2]["execute"], "blockdev-del");
    }

    #[tokio::test]
    async fn detach_block_device_fails_fast_on_non_in_use_errors() {
        let (mut client, server) = fake_qmp_pair();
        let error = b"{\"error\": {\"class\": \"GenericError\", \"desc\": \"Node not found\"}}\n";
        let (mut server_read, mut server_write) = tokio::io::split(server);
        let server_task = tokio::spawn(async move {
            let mut buf = BufReader::new(&mut server_read);
            for reply in [b"{\"return\": {}}\n".as_slice(), error] {
                let mut line = String::new();
                buf.read_line(&mut line).await.unwrap();
                server_write.write_all(reply).await.unwrap();
            }
            // must NOT get a third request: the retry only covers in-use errors
            let mut extra = String::new();
            let n = tokio::time::timeout(
                std::time::Duration::from_millis(400),
                buf.read_line(&mut extra),
            )
            .await;
            let no_retry = match n {
                Err(_) => true,
                Ok(Ok(0)) => true,
                _ => false,
            };
            assert!(no_retry, "no retry expected after non-in-use error");
        });
        let err = client
            .detach_block_device("extra0", "drive-extra0", std::time::Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(
            matches!(err, QmpError::CommandFailed { .. }),
            "expected CommandFailed, got: {err:?}"
        );
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn detach_block_device_guest_timeout_maps_to_guest_unplug_timeout() {
        let (mut client, server) = fake_qmp_pair();
        let error = b"{\"error\": {\"class\": \"DeviceInUse\", \"desc\": \"Node drive-extra0 is in use\"}}\n";
        let (mut server_read, mut server_write) = tokio::io::split(server);
        let server_task = tokio::spawn(async move {
            let mut buf = BufReader::new(&mut server_read);
            for i in 0.. {
                let mut line = String::new();
                buf.read_line(&mut line).await.unwrap();
                if i == 0 {
                    server_write.write_all(b"{\"return\": {}}\n").await.unwrap();
                } else {
                    server_write.write_all(error).await.unwrap();
                }
            }
        });
        let err = client
            .detach_block_device(
                "extra0",
                "drive-extra0",
                std::time::Duration::from_millis(200),
            )
            .await
            .unwrap_err();
        server_task.abort();
        assert!(
            matches!(&err, QmpError::GuestUnplugTimeout { device_id } if device_id.as_str() == "extra0"),
            "expected GuestUnplugTimeout, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn detach_net_device_sends_device_del_then_netdev_del() {
        let (mut client, server) = fake_qmp_pair();
        let requests = run_with_server(
            server,
            vec![b"{\"return\": {}}\n", b"{\"return\": {}}\n"],
            async {
                client
                    .detach_net_device(
                        "net-extra0",
                        "net-extra0",
                        std::time::Duration::from_secs(5),
                    )
                    .await
            },
        )
        .await;
        assert_eq!(requests[0]["execute"], "device_del");
        assert_eq!(requests[0]["arguments"]["id"], "net-extra0");
        assert_eq!(requests[1]["execute"], "netdev_del");
        assert_eq!(requests[1]["arguments"]["id"], "net-extra0");
    }
}
