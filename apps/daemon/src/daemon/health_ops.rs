use super::error::DaemonError;
use super::Daemon;
use andler_core::{
    BackendHandle, GuestExecOutput, GuestReadinessLevel, HypervisorBackend, InstanceConfig,
    InstanceEvent, InstanceId, InstanceState, ProbeOutcome, ReadinessSnapshot,
};
use std::sync::Arc;

/// Per-probe budget. A level that cannot answer inside this window is left
/// `Unobservable` for this pass instead of holding the caller's RPC (or the
/// next health cycle) open on an agent that is not answering.
const READINESS_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// What the two init-reporting probes print when the guest has no systemd to
/// ask. It is an `Unobservable` answer, not a failure: a guest whose init
/// keeps no readiness state can never report the level.
const NO_INIT_REPORTER: &str = "andler-no-init-reporter";

/// The guest command that observes `level`, or `None` for the levels the
/// daemon observes from the host (the VM process, then the agent handshake).
fn probe_argv(level: GuestReadinessLevel) -> Option<&'static [&'static str]> {
    match level {
        GuestReadinessLevel::SerialUp | GuestReadinessLevel::QgaUp => None,
        GuestReadinessLevel::DisplayApplied => {
            Some(&["/bin/sh", "-c", "cat /etc/andler/display.conf"])
        }
        GuestReadinessLevel::GuestOsUp => Some(&[
            "/bin/sh",
            "-c",
            "if command -v systemctl >/dev/null 2>&1; then systemctl is-system-running; \
             else echo andler-no-init-reporter; fi",
        ]),
        GuestReadinessLevel::WaydroidReady => Some(&[
            "/bin/sh",
            "-c",
            "if command -v systemctl >/dev/null 2>&1; then systemctl is-active \
             waydroid-container.service; else echo andler-no-init-reporter; fi",
        ]),
    }
}

/// Resolution the guest's display hook records in `/etc/andler/display.conf`
/// once it has applied one.
fn applied_resolution(display_conf: &str) -> Option<&str> {
    display_conf
        .lines()
        .find_map(|line| line.trim().strip_prefix("RESOLUTION="))
}

/// Reads one probe's answer. `Ready` only ever comes from the guest saying
/// so: a silent or failed probe is never rounded up to a reached level.
fn interpret_probe(
    level: GuestReadinessLevel,
    cfg: &InstanceConfig,
    output: &GuestExecOutput,
) -> ProbeOutcome {
    let stdout = output.stdout.trim();
    match level {
        GuestReadinessLevel::DisplayApplied => {
            let configured = format!(
                "{}x{}",
                cfg.display.resolution.width, cfg.display.resolution.height
            );
            match applied_resolution(&output.stdout) {
                Some(applied) if applied == configured => ProbeOutcome::Ready,
                _ => ProbeOutcome::NotReady,
            }
        }
        GuestReadinessLevel::GuestOsUp => match stdout {
            // "degraded" is systemd's own wording for "booted, with a failed
            // unit": the OS is up, which is what this level states.
            "running" | "degraded" => ProbeOutcome::Ready,
            NO_INIT_REPORTER => ProbeOutcome::Unobservable,
            _ => ProbeOutcome::NotReady,
        },
        GuestReadinessLevel::WaydroidReady => match stdout {
            "active" => ProbeOutcome::Ready,
            NO_INIT_REPORTER => ProbeOutcome::Unobservable,
            _ => ProbeOutcome::NotReady,
        },
        GuestReadinessLevel::SerialUp | GuestReadinessLevel::QgaUp => ProbeOutcome::Unobservable,
    }
}

/// Probe of the VM process itself: the serial chardev cannot be asked
/// directly (it serves one client and the daemon must not take it away from
/// an attach session), so the level is read from the backend's own live
/// status.
async fn probe_serial_up(
    backend: &Arc<dyn HypervisorBackend>,
    handle: &BackendHandle,
) -> ProbeOutcome {
    match tokio::time::timeout(READINESS_PROBE_TIMEOUT, backend.status(handle)).await {
        Ok(Ok(status)) => match status.state {
            InstanceState::Running | InstanceState::Paused => ProbeOutcome::Ready,
            _ => ProbeOutcome::NotReady,
        },
        Ok(Err(_)) | Err(_) => ProbeOutcome::Unobservable,
    }
}

/// Probe of the guest agent handshake: the only level the daemon observes
/// without asking the guest to run anything.
async fn probe_qga_up(
    backend: &Arc<dyn HypervisorBackend>,
    handle: &BackendHandle,
) -> ProbeOutcome {
    match tokio::time::timeout(
        READINESS_PROBE_TIMEOUT,
        backend.is_guest_agent_available(handle),
    )
    .await
    {
        Ok(Ok(true)) => ProbeOutcome::Ready,
        Ok(Ok(false)) => ProbeOutcome::NotReady,
        Ok(Err(_)) | Err(_) => ProbeOutcome::Unobservable,
    }
}

async fn probe_via_guest_agent(
    backend: &Arc<dyn HypervisorBackend>,
    handle: &BackendHandle,
    cfg: &InstanceConfig,
    level: GuestReadinessLevel,
) -> ProbeOutcome {
    let Some(argv) = probe_argv(level) else {
        return ProbeOutcome::Unobservable;
    };
    let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_string()).collect();
    match tokio::time::timeout(
        READINESS_PROBE_TIMEOUT,
        backend.guest_exec_command(handle, &argv, Some(READINESS_PROBE_TIMEOUT)),
    )
    .await
    {
        Ok(Ok(output)) => interpret_probe(level, cfg, &output),
        Ok(Err(backend_error)) => {
            tracing::debug!(
                level = ?level,
                error = %backend_error,
                "readiness probe did not reach the guest agent"
            );
            ProbeOutcome::Unobservable
        }
        Err(_) => ProbeOutcome::Unobservable,
    }
}

impl Daemon {
    /// Advances one instance's readiness ladder by probing every level above
    /// the reached one, weakest first, and returns the position afterwards.
    ///
    /// The pass stops at the first level that does not answer `Ready`: the
    /// levels above `QgaUp` are observed through the agent, so a guest
    /// without one reports `QgaUp` and stops instead of guessing. Nothing is
    /// probed while another operation owns the agent (the QGA chardev serves
    /// one client), and a run already at its terminal level costs nothing.
    pub(crate) async fn probe_readiness(
        &self,
        id: InstanceId,
    ) -> Result<ReadinessSnapshot, DaemonError> {
        let handle = self.handle_for(id).await?;
        // A paused guest cannot answer and cannot progress: its ladder stays
        // where the run left it.
        if handle.state() != InstanceState::Running {
            return Ok(handle.readiness());
        }
        if handle.active_operation().await?.is_some() {
            return Ok(handle.readiness());
        }
        let Some(backend_handle) = handle.backend_handle() else {
            return Ok(handle.readiness());
        };
        let cfg = handle.config();
        let backend = self.backend_for(cfg.backend)?;

        let mut snapshot = handle.readiness();
        for level in snapshot.pending().collect::<Vec<_>>() {
            let outcome = match level {
                GuestReadinessLevel::SerialUp => probe_serial_up(backend, &backend_handle).await,
                GuestReadinessLevel::QgaUp => probe_qga_up(backend, &backend_handle).await,
                _ => probe_via_guest_agent(backend, &backend_handle, &cfg, level).await,
            };
            snapshot = handle.observe_readiness(level, outcome).await?;
            if outcome != ProbeOutcome::Ready {
                break;
            }
        }
        Ok(snapshot)
    }

    pub async fn run_health_check_once(&self) {
        let running: Vec<_> = {
            let supervisors = self.supervisors.read().await;
            supervisors
                .iter()
                .filter_map(|(id, handle)| {
                    if handle.state() == InstanceState::Running {
                        let config = handle.config();
                        handle
                            .backend_handle()
                            .map(|backend_handle| (*id, config.backend, backend_handle))
                    } else {
                        None
                    }
                })
                .collect()
        };

        for (id, backend_kind, backend_handle) in running {
            let backend = match self.backend_for(backend_kind) {
                Ok(backend) => backend.clone(),
                Err(err) => {
                    tracing::warn!(
                        instance_id = %id,
                        error = %err,
                        "health check: no backend registered, skipping"
                    );
                    continue;
                }
            };

            let status = match backend.status(&backend_handle).await {
                Ok(status) => status,
                Err(err) => {
                    tracing::warn!(
                        instance_id = %id,
                        error = %err,
                        "health check: status query failed, will retry next cycle"
                    );
                    continue;
                }
            };

            let still_active = matches!(
                status.state,
                InstanceState::Running | InstanceState::Paused | InstanceState::Starting
            );
            if still_active {
                if let Err(err) = self.probe_readiness(id).await {
                    tracing::debug!(
                        instance_id = %id,
                        error = %err,
                        "readiness probe pass skipped"
                    );
                }
                continue;
            }

            let reason = status.detail.clone().unwrap_or_else(|| {
                format!("backend now reports state {:?}, was Running", status.state)
            });

            if status.clean_shutdown {
                tracing::info!(
                    instance_id = %id,
                    reason = %reason,
                    "instance health check: guest shut down cleanly — marking Stopped"
                );
                if let Err(err) = self.mark_instance_stopped_cleanly(id).await {
                    tracing::error!(
                        instance_id = %id,
                        error = %err,
                        "health check: failed to record clean shutdown in FSM"
                    );
                }
                continue;
            }

            tracing::error!(
                instance_id = %id,
                reason = %reason,
                "instance health check: process is no longer running (was Running) \
                 — marking Error. Restart it manually with `andler start`."
            );

            if let Err(err) = self.mark_instance_crashed(id, reason).await {
                tracing::error!(
                    instance_id = %id,
                    error = %err,
                    "health check: failed to record crash in FSM"
                );
            }
        }
    }

    pub(crate) async fn mark_instance_crashed(
        &self,
        id: InstanceId,
        reason: String,
    ) -> Result<(), DaemonError> {
        let handle = self.handle_for(id).await?;
        handle.set_handle(None).await?;
        handle.transition(InstanceEvent::Fail(reason)).await?;
        Ok(())
    }

    /// Like `mark_instance_crashed`, but for a backend-observed status that reflects
    /// a genuine guest-initiated shutdown (see `BackendStatus::clean_shutdown`) rather
    /// than the process disappearing unexpectedly — reaches `Stopped` through the
    /// normal Stop+StopCompleted transitions instead of `Error`.
    pub(crate) async fn mark_instance_stopped_cleanly(
        &self,
        id: InstanceId,
    ) -> Result<(), DaemonError> {
        let handle = self.handle_for(id).await?;
        handle.set_handle(None).await?;
        handle.transition(InstanceEvent::Stop).await?;
        handle.transition(InstanceEvent::StopCompleted).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use andler_core::{
        AudioConfig, BackendError, BackendKind, CdromBus, CpuConfig, DaemonEvent, DiskConfig,
        DisplayConfig, EventKind, FirmwareConfig, GpuConfig, InputConfig, InstanceKind,
        MemoryConfig, NetworkConfig, Operation, OperationKind, RenderBackend,
    };
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use futures_util::StreamExt;

    /// Guest-side answers the mock gives the probe pass, one field per level
    /// the daemon asks the guest about.
    struct GuestAnswers {
        /// `/etc/andler/display.conf` as the guest has it.
        display_conf: &'static str,
        /// `systemctl is-system-running` output.
        init_state: &'static str,
        /// `systemctl is-active waydroid-container.service` output.
        waydroid_state: &'static str,
    }

    /// Guest agent whose answers are set per level, so a test can put the
    /// probe pass anywhere on the ladder.
    struct ReadinessBackend {
        answers: Mutex<GuestAnswers>,
        agent: Result<bool, ()>,
        agent_probes: AtomicUsize,
        exec_calls: AtomicUsize,
        process_alive: bool,
    }

    impl ReadinessBackend {
        fn new(answers: GuestAnswers, agent: Result<bool, ()>) -> Self {
            ReadinessBackend {
                answers: Mutex::new(answers),
                agent,
                agent_probes: AtomicUsize::new(0),
                exec_calls: AtomicUsize::new(0),
                process_alive: true,
            }
        }

        fn answer_with(&self, answers: GuestAnswers) {
            *self.answers.lock().expect("answers lock") = answers;
        }

        fn output_for(&self, argv: &[String]) -> GuestExecOutput {
            let command = argv.last().map(String::as_str).unwrap_or_default();
            let answers = self.answers.lock().expect("answers lock");
            let stdout = if command.contains("display.conf") {
                answers.display_conf
            } else if command.contains("is-system-running") {
                answers.init_state
            } else if command.contains("is-active") {
                answers.waydroid_state
            } else {
                ""
            };
            GuestExecOutput {
                exit_code: 0,
                stdout: stdout.to_string(),
                stderr: String::new(),
            }
        }
    }

    #[async_trait::async_trait]
    impl HypervisorBackend for ReadinessBackend {
        fn name(&self) -> &'static str {
            "readiness-mock"
        }

        fn supported_render_backends(&self) -> &[RenderBackend] {
            &[]
        }

        async fn spawn(&self, _cfg: &InstanceConfig) -> Result<BackendHandle, BackendError> {
            Ok(BackendHandle("readiness-mock:vm".to_string()))
        }

        async fn pause(&self, _handle: &BackendHandle) -> Result<(), BackendError> {
            Ok(())
        }

        async fn resume(&self, _handle: &BackendHandle) -> Result<(), BackendError> {
            Ok(())
        }

        async fn stop(&self, _handle: &BackendHandle, _graceful: bool) -> Result<(), BackendError> {
            Ok(())
        }

        async fn status(
            &self,
            _handle: &BackendHandle,
        ) -> Result<andler_core::BackendStatus, BackendError> {
            Ok(andler_core::BackendStatus {
                state: if self.process_alive {
                    InstanceState::Running
                } else {
                    InstanceState::Stopped
                },
                detail: None,
                clean_shutdown: false,
            })
        }

        async fn is_guest_agent_available(
            &self,
            _handle: &BackendHandle,
        ) -> Result<bool, BackendError> {
            self.agent_probes.fetch_add(1, Ordering::SeqCst);
            self.agent.map_err(|()| BackendError::ProcessNotRunning)
        }

        async fn guest_exec_command(
            &self,
            _handle: &BackendHandle,
            argv: &[String],
            _timeout: Option<std::time::Duration>,
        ) -> Result<GuestExecOutput, BackendError> {
            self.exec_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.output_for(argv))
        }

        fn metrics_stream(
            &self,
            _handle: &BackendHandle,
        ) -> futures_core::stream::BoxStream<'_, andler_core::ResourceMetrics> {
            Box::pin(futures_util::stream::empty())
        }

        fn log_stream(
            &self,
            _handle: &BackendHandle,
        ) -> futures_core::stream::BoxStream<'_, andler_core::LogLine> {
            Box::pin(futures_util::stream::empty())
        }
    }

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "andler-readiness-probe-{}-{}",
                std::process::id(),
                InstanceId::new()
            ));
            std::fs::create_dir_all(&path).expect("create test dir");
            TestDir(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn linux_config(dir: &std::path::Path) -> InstanceConfig {
        InstanceConfig {
            id: InstanceId::new(),
            name: "readiness-vm".to_string(),
            kind: InstanceKind::LinuxVm {
                iso_path: PathBuf::from("/tmp/readiness.iso"),
                cdrom_bus: CdromBus::Ide,
            },
            backend: BackendKind::Qemu,
            schema_version: andler_core::CURRENT_SCHEMA_VERSION,
            cpu: CpuConfig::reference_default(),
            memory: MemoryConfig::reference_default(),
            disk: DiskConfig::reference_default(dir.join("disk.qcow2")),
            display: DisplayConfig::reference_default(),
            gpu: GpuConfig::reference_default(),
            network: NetworkConfig::reference_default(),
            extra_disks: Vec::new(),
            extra_networks: Vec::new(),
            firmware: FirmwareConfig::reference_default(dir.join("VARS.fd")),
            audio: AudioConfig::reference_default(),
            input: InputConfig::reference_default(),
            autostart: false,
        }
    }

    fn android_config(
        dir: &std::path::Path,
        boot_mode: andler_core::AndroidBootMode,
    ) -> InstanceConfig {
        let mut cfg = linux_config(dir);
        cfg.kind = InstanceKind::AndroidVm {
            android_profile: andler_core::AndroidProfile {
                android_version: andler_core::AndroidVersion::Android13,
                gapps: false,
                microg: false,
                arm_translator: andler_core::ArmTranslator::None,
                boot_mode,
                base_image_pin: None,
            },
        };
        cfg
    }

    async fn running_instance(
        cfg: InstanceConfig,
        answers: GuestAnswers,
        agent: Result<bool, ()>,
    ) -> (Daemon, Arc<ReadinessBackend>, InstanceId, TestDir) {
        let dir = TestDir::new();
        let mut daemon = Daemon::new();
        let id = cfg.id;
        let mock = Arc::new(ReadinessBackend::new(answers, agent));
        daemon.backends.insert(BackendKind::Qemu, mock.clone());
        let handle = super::super::spawn_supervisor(
            id,
            dir.0.clone(),
            cfg,
            InstanceState::Running,
            Some(BackendHandle("readiness-mock:vm".to_string())),
            daemon.event_sender(),
        );
        daemon.supervisors.write().await.insert(id, handle);
        (daemon, mock, id, dir)
    }

    fn answers(
        display_conf: &'static str,
        init_state: &'static str,
        waydroid_state: &'static str,
    ) -> GuestAnswers {
        GuestAnswers {
            display_conf,
            init_state,
            waydroid_state,
        }
    }

    /// The resolution the reference display config carries, as the guest's
    /// own record writes it.
    fn applied_default() -> &'static str {
        "RESOLUTION=1920x1080"
    }

    #[test]
    fn display_level_comes_from_the_guests_applied_resolution() {
        let cfg = linux_config(std::path::Path::new("/tmp"));
        let ready = GuestExecOutput {
            exit_code: 0,
            stdout: format!("{}\n", applied_default()),
            stderr: String::new(),
        };
        assert_eq!(
            interpret_probe(GuestReadinessLevel::DisplayApplied, &cfg, &ready),
            ProbeOutcome::Ready
        );

        let other_mode = GuestExecOutput {
            exit_code: 0,
            stdout: "RESOLUTION=800x600\n".to_string(),
            stderr: String::new(),
        };
        assert_eq!(
            interpret_probe(GuestReadinessLevel::DisplayApplied, &cfg, &other_mode),
            ProbeOutcome::NotReady,
            "a guest that applied a different mode has not applied the configured one"
        );

        let never_applied = GuestExecOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "cat: /etc/andler/display.conf: No such file or directory".to_string(),
        };
        assert_eq!(
            interpret_probe(GuestReadinessLevel::DisplayApplied, &cfg, &never_applied),
            ProbeOutcome::NotReady,
            "a guest with no applied-resolution record is measured, not assumed"
        );
    }

    #[test]
    fn init_probe_separates_booting_ready_and_unobservable() {
        let cfg = linux_config(std::path::Path::new("/tmp"));
        let with_stdout = |stdout: &str| GuestExecOutput {
            exit_code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        };
        for (stdout, expected) in [
            ("starting\n", ProbeOutcome::NotReady),
            ("maintenance\n", ProbeOutcome::NotReady),
            ("running\n", ProbeOutcome::Ready),
            ("degraded\n", ProbeOutcome::Ready),
            ("andler-no-init-reporter\n", ProbeOutcome::Unobservable),
            ("", ProbeOutcome::NotReady),
        ] {
            assert_eq!(
                interpret_probe(GuestReadinessLevel::GuestOsUp, &cfg, &with_stdout(stdout)),
                expected,
                "unexpected answer for init state {stdout:?}"
            );
        }
    }

    #[test]
    fn waydroid_probe_counts_only_an_active_container() {
        let cfg = linux_config(std::path::Path::new("/tmp"));
        let with_stdout = |stdout: &str| GuestExecOutput {
            exit_code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        };
        for (stdout, expected) in [
            ("active\n", ProbeOutcome::Ready),
            ("activating\n", ProbeOutcome::NotReady),
            ("inactive\n", ProbeOutcome::NotReady),
            ("failed\n", ProbeOutcome::NotReady),
            ("andler-no-init-reporter\n", ProbeOutcome::Unobservable),
        ] {
            assert_eq!(
                interpret_probe(
                    GuestReadinessLevel::WaydroidReady,
                    &cfg,
                    &with_stdout(stdout)
                ),
                expected,
                "unexpected answer for container state {stdout:?}"
            );
        }
    }

    #[tokio::test]
    async fn probe_pass_walks_the_ladder_and_publishes_every_level_reached() {
        let dir = TestDir::new();
        let cfg = android_config(&dir.0, andler_core::AndroidBootMode::Android);
        let (daemon, mock, id, _dir) =
            running_instance(cfg, answers("", "running\n", "active\n"), Ok(true)).await;
        // Subscribe before the probes: the bus carries what is published after
        // the subscription and the ladder is published as it advances, so
        // collecting afterwards would wait for events that already happened.
        let mut readiness = daemon.stream_events(Some(id));
        let snapshot = daemon.probe_readiness(id).await.expect("probe pass");
        assert_eq!(
            snapshot.current,
            Some(GuestReadinessLevel::QgaUp),
            "the agent answers, the display has not been applied: the ladder \
             stops at the level the probes actually measured"
        );
        assert_eq!(snapshot.terminal(), GuestReadinessLevel::WaydroidReady);
        assert!(!snapshot.at_terminal());

        mock.answer_with(answers(applied_default(), "running\n", "active\n"));
        let snapshot = daemon.probe_readiness(id).await.expect("probe pass");
        assert_eq!(
            snapshot.current,
            Some(GuestReadinessLevel::WaydroidReady),
            "the display was applied, the OS reported ready and the container \
             is up, so the profile's terminal level is reached"
        );
        assert!(snapshot.at_terminal());
        assert_eq!(snapshot.pending().count(), 0);

        let mut published: Vec<GuestReadinessLevel> = Vec::new();
        let deadline = std::time::Duration::from_secs(5);
        tokio::time::timeout(deadline, async {
            while published.len() < 5 {
                match readiness.next().await {
                    Some(event) => {
                        if let EventKind::Readiness { level } = event.kind {
                            published.push(level);
                        }
                    }
                    None => break,
                }
            }
        })
        .await
        .expect("the ladder's five levels must be published");
        assert_eq!(
            published,
            vec![
                GuestReadinessLevel::SerialUp,
                GuestReadinessLevel::QgaUp,
                GuestReadinessLevel::DisplayApplied,
                GuestReadinessLevel::GuestOsUp,
                GuestReadinessLevel::WaydroidReady,
            ],
            "each reached level is published once, in ladder order"
        );
    }

    #[tokio::test]
    async fn an_answering_agent_is_not_an_os_ready_report() {
        let dir = TestDir::new();
        let cfg = linux_config(&dir.0);
        let (daemon, mock, id, _dir) =
            running_instance(cfg, answers("", "starting\n", "inactive\n"), Ok(true)).await;

        let snapshot = daemon.probe_readiness(id).await.expect("probe pass");
        assert_eq!(
            snapshot.current,
            Some(GuestReadinessLevel::QgaUp),
            "QgaUp must not stand in for GuestOsUp: the guest reported that it \
             is still booting"
        );
        assert!(snapshot
            .pending()
            .any(|level| level == GuestReadinessLevel::GuestOsUp));

        mock.answer_with(answers(applied_default(), "running\n", "inactive\n"));
        let snapshot = daemon.probe_readiness(id).await.expect("probe pass");
        assert_eq!(snapshot.current, Some(GuestReadinessLevel::GuestOsUp));
        assert!(
            snapshot.at_terminal(),
            "GuestOsUp is the terminal level of a Linux guest"
        );
    }

    #[tokio::test]
    async fn a_guest_without_a_ready_reporter_cannot_reach_the_level() {
        let dir = TestDir::new();
        let cfg = linux_config(&dir.0);
        let (daemon, _mock, id, _dir) = running_instance(
            cfg,
            answers("", "andler-no-init-reporter\n", "andler-no-init-reporter\n"),
            Ok(true),
        )
        .await;

        let snapshot = daemon.probe_readiness(id).await.expect("probe pass");
        assert_eq!(
            snapshot.current,
            Some(GuestReadinessLevel::QgaUp),
            "the guest answered that it has no init readiness reporter: the \
             level is uncountable, not reached"
        );
        assert!(snapshot
            .pending()
            .any(|level| level == GuestReadinessLevel::GuestOsUp));
    }

    #[tokio::test]
    async fn a_guest_that_only_says_a_level_in_log_text_does_not_reach_it() {
        let dir = TestDir::new();
        let cfg = android_config(&dir.0, andler_core::AndroidBootMode::Android);
        let (daemon, _mock, id, _dir) = running_instance(
            cfg,
            answers(
                applied_default(),
                "running\n",
                "Waydroid session is up (see /var/lib/waydroid/waydroid.log)\n",
            ),
            Ok(true),
        )
        .await;

        let snapshot = daemon.probe_readiness(id).await.expect("probe pass");
        assert_eq!(
            snapshot.current,
            Some(GuestReadinessLevel::GuestOsUp),
            "text that mentions Waydroid is not the container reporting itself \
             as running"
        );
        assert_eq!(
            snapshot.pending().collect::<Vec<_>>(),
            vec![GuestReadinessLevel::WaydroidReady]
        );
    }

    #[tokio::test]
    async fn waydroid_ready_is_uncountable_for_a_linux_guest() {
        let dir = TestDir::new();
        let cfg = linux_config(&dir.0);
        let (daemon, _mock, id, _dir) = running_instance(
            cfg,
            answers(applied_default(), "running\n", "active\n"),
            Ok(true),
        )
        .await;

        let snapshot = daemon.probe_readiness(id).await.expect("probe pass");
        assert_eq!(
            snapshot.current,
            Some(GuestReadinessLevel::GuestOsUp),
            "a Linux guest ends the ladder at GuestOsUp even if its guest says \
             a Waydroid container is active"
        );
        assert_eq!(snapshot.terminal(), GuestReadinessLevel::GuestOsUp);
    }

    #[tokio::test]
    async fn an_android_vm_in_linux_boot_mode_ends_at_guest_os_up() {
        let dir = TestDir::new();
        let cfg = android_config(&dir.0, andler_core::AndroidBootMode::Linux);
        let (daemon, _mock, id, _dir) = running_instance(
            cfg,
            answers(applied_default(), "running\n", "active\n"),
            Ok(true),
        )
        .await;

        let snapshot = daemon.probe_readiness(id).await.expect("probe pass");
        assert_eq!(snapshot.current, Some(GuestReadinessLevel::GuestOsUp));
        assert_eq!(
            snapshot.terminal(),
            GuestReadinessLevel::GuestOsUp,
            "the terminal level comes from the effective (kind, boot_mode) \
             profile, not from the kind"
        );
    }

    #[tokio::test]
    async fn a_level_already_reached_never_goes_backwards_when_the_guest_flaps() {
        let dir = TestDir::new();
        let cfg = linux_config(&dir.0);
        let (daemon, mock, id, _dir) = running_instance(
            cfg,
            answers(applied_default(), "running\n", "inactive\n"),
            Ok(true),
        )
        .await;
        assert_eq!(
            daemon
                .probe_readiness(id)
                .await
                .expect("probe pass")
                .current,
            Some(GuestReadinessLevel::GuestOsUp)
        );

        mock.answer_with(answers("RESOLUTION=800x600", "starting\n", "inactive\n"));
        let snapshot = daemon.probe_readiness(id).await.expect("probe pass");
        assert_eq!(
            snapshot.current,
            Some(GuestReadinessLevel::GuestOsUp),
            "the level a run reached is a fact about that run"
        );
    }

    #[tokio::test]
    async fn readiness_is_not_probed_while_an_operation_owns_the_agent() {
        let dir = TestDir::new();
        let cfg = linux_config(&dir.0);
        let (daemon, mock, id, _dir) = running_instance(
            cfg,
            answers(applied_default(), "running\n", "inactive\n"),
            Ok(true),
        )
        .await;
        let handle = daemon.handle_for(id).await.expect("handle");
        let run: super::super::supervisor::OpRunner = Box::new(|mut progress| {
            Box::pin(async move {
                progress.enter_phase("installing");
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                progress.finish(Ok(()));
                Ok(())
            })
        });
        handle
            .run_operation(
                Operation::new(OperationKind::GuestInstall, id, "op-readiness".to_string()),
                None,
                run,
            )
            .await
            .expect("operation accepted");

        let snapshot = daemon.probe_readiness(id).await.expect("probe pass");
        assert_eq!(
            snapshot.current, None,
            "the probe pass leaves the ladder alone while another operation \
             holds the guest agent"
        );
        assert_eq!(
            mock.exec_calls.load(Ordering::SeqCst),
            0,
            "no second QGA client is opened behind a running operation"
        );
        assert_eq!(
            mock.agent_probes.load(Ordering::SeqCst),
            0,
            "even the agent handshake probe is skipped"
        );
    }

    #[tokio::test]
    async fn switching_android_boot_mode_re_derives_the_ladder() {
        let dir = TestDir::new();
        let cfg = android_config(&dir.0, andler_core::AndroidBootMode::Linux);
        let (daemon, _mock, id, _dir) = running_instance(
            cfg.clone(),
            answers(applied_default(), "running\n", "active\n"),
            Ok(true),
        )
        .await;
        let handle = daemon.handle_for(id).await.expect("handle");
        assert_eq!(
            daemon
                .probe_readiness(id)
                .await
                .expect("probe pass")
                .current,
            Some(GuestReadinessLevel::GuestOsUp)
        );
        assert!(handle.readiness().at_terminal());

        let mut switched = cfg;
        switched.kind = android_config(&_dir.0, andler_core::AndroidBootMode::Android).kind;
        handle
            .set_config(switched, None)
            .await
            .expect("config accepted");

        let after = handle.readiness();
        assert_eq!(
            after.terminal(),
            GuestReadinessLevel::WaydroidReady,
            "the terminal level follows the effective (kind, boot_mode) profile"
        );
        assert_eq!(
            after.current, None,
            "a profile change starts a new ladder instead of carrying the old profile's levels"
        );
    }

    #[tokio::test]
    async fn a_new_run_starts_from_the_bottom_of_the_ladder() {
        let dir = TestDir::new();
        let cfg = linux_config(&dir.0);
        let (daemon, _mock, id, _dir) = running_instance(
            cfg,
            answers(applied_default(), "running\n", "inactive\n"),
            Ok(true),
        )
        .await;
        let handle = daemon.handle_for(id).await.expect("handle");
        assert_eq!(
            daemon
                .probe_readiness(id)
                .await
                .expect("probe pass")
                .current,
            Some(GuestReadinessLevel::GuestOsUp)
        );

        handle
            .transition(InstanceEvent::Stop)
            .await
            .expect("stop accepted");
        assert_eq!(
            handle.readiness().current,
            None,
            "a run that ended keeps no level to report"
        );

        handle
            .transition(InstanceEvent::StopCompleted)
            .await
            .expect("stop completes");
        handle
            .transition(InstanceEvent::Start)
            .await
            .expect("restart accepted");
        assert_eq!(
            handle.readiness().current,
            None,
            "the next run starts from the bottom of the ladder"
        );
        assert_eq!(
            handle.readiness().terminal(),
            GuestReadinessLevel::GuestOsUp
        );
    }

    #[tokio::test]
    async fn readiness_events_are_appended_to_the_audit_trail() {
        let dir = TestDir::new();
        let cfg = linux_config(&dir.0);
        let (daemon, _mock, id, _dir) = running_instance(
            cfg,
            answers(applied_default(), "running\n", "inactive\n"),
            Ok(true),
        )
        .await;
        daemon.probe_readiness(id).await.expect("probe pass");

        // The audit append is a spawned task, so the file fills a moment after
        // the probe returns: poll it with a bound instead of racing it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let levels = loop {
            let levels = audit_levels(&_dir.0.join("events.jsonl"));
            if levels.len() >= 4 || std::time::Instant::now() > deadline {
                break levels;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        assert_eq!(
            levels,
            vec!["SerialUp", "QgaUp", "DisplayApplied", "GuestOsUp"]
        );
    }

    fn audit_levels(path: &Path) -> Vec<&'static str> {
        let Ok(trail) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        trail
            .lines()
            .filter_map(|line| serde_json::from_str::<DaemonEvent>(line).ok())
            .filter_map(|event| match event.kind {
                EventKind::Readiness { level } => Some(match level {
                    GuestReadinessLevel::SerialUp => "SerialUp",
                    GuestReadinessLevel::QgaUp => "QgaUp",
                    GuestReadinessLevel::DisplayApplied => "DisplayApplied",
                    GuestReadinessLevel::GuestOsUp => "GuestOsUp",
                    GuestReadinessLevel::WaydroidReady => "WaydroidReady",
                }),
                _ => None,
            })
            .collect()
    }
}
