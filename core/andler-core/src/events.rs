use serde::{Deserialize, Serialize};

use crate::android_profile::AndroidBootMode;
use crate::backend::BackendHandle;
use crate::config::InstanceKind;
use crate::fsm::InstanceState;
use crate::InstanceId;

/// Machine-readable event emitted by the daemon, forming the per-instance
/// audit trail. `ts_ms` is milliseconds since the UNIX epoch (wall clock).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonEvent {
    pub ts_ms: u64,
    pub instance_id: Option<InstanceId>,
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventKind {
    /// FSM transition, with the reason on failure-driven transitions.
    Lifecycle {
        from: InstanceState,
        to: InstanceState,
        reason: Option<String>,
    },

    /// State change of a long-running operation.
    Operation { op: Operation },

    /// Raw QEMU QMP event the daemon reacts to.
    Qmp { event: QmpEvent, data: String },

    /// Guest readiness contract reached.
    Readiness { level: GuestReadinessLevel },

    /// User-facing diagnostic line for the audit trail.
    Log {
        level: EventLogLevel,
        message: String,
    },
}

/// A raw QMP event forwarded from a backend, tagged with the handle of the
/// instance it belongs to. The daemon relay maps the handle back to an
/// instance id and republishes the event on the daemon bus.
#[derive(Debug, Clone)]
pub struct QmpEventRecord {
    pub handle: BackendHandle,
    pub event: QmpEvent,
    pub data: String,
}

/// Raw QEMU QMP events the daemon can react to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QmpEvent {
    VserportChanged,
    Shutdown,
    Reset,
    Powerdown,
    DeviceDeleted,
    BlockIoError,
    BlockJob,
    GuestPanicked,
    Watchdog,
    Stopped,
    Resumed,
    Other,
}

/// Steps of the guest readiness contract; ordered from weakest to strongest
/// guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GuestReadinessLevel {
    SerialUp,
    QgaUp,
    DisplayApplied,
    GuestOsUp,
    WaydroidReady,
}

/// Effective guest profile a run's readiness ladder is derived from. The
/// profile is the `(kind, boot_mode)` pair, never `kind` alone: an
/// `AndroidVm` switched to Linux boot mode never brings up Waydroid, so it
/// ends the ladder exactly where an `LinuxVm` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadinessProfile {
    /// `InstanceKind::LinuxVm` — a distro the user booted from an ISO.
    LinuxGuest,
    /// `InstanceKind::AndroidVm` booted into `boot_mode = linux`.
    AndroidLinuxMode,
    /// `InstanceKind::AndroidVm` booted into `boot_mode = android`, where the
    /// Android session runs inside the Waydroid container.
    AndroidWaydroid,
}

const LINUX_LEVELS: &[GuestReadinessLevel] = &[
    GuestReadinessLevel::SerialUp,
    GuestReadinessLevel::QgaUp,
    GuestReadinessLevel::DisplayApplied,
    GuestReadinessLevel::GuestOsUp,
];
const WAYDROID_LEVELS: &[GuestReadinessLevel] = &[
    GuestReadinessLevel::SerialUp,
    GuestReadinessLevel::QgaUp,
    GuestReadinessLevel::DisplayApplied,
    GuestReadinessLevel::GuestOsUp,
    GuestReadinessLevel::WaydroidReady,
];

impl ReadinessProfile {
    /// Derives the effective profile from the instance kind and boot mode.
    pub fn of(kind: &InstanceKind) -> Self {
        match kind {
            InstanceKind::LinuxVm { .. } => ReadinessProfile::LinuxGuest,
            InstanceKind::AndroidVm { android_profile } => match android_profile.boot_mode {
                AndroidBootMode::Linux => ReadinessProfile::AndroidLinuxMode,
                AndroidBootMode::Android => ReadinessProfile::AndroidWaydroid,
            },
        }
    }

    /// Every level this profile can reach, weakest first. A level outside
    /// this list cannot be observed for the profile and is never reported
    /// for it — it is uncountable, not reached and not failed.
    pub fn levels(self) -> &'static [GuestReadinessLevel] {
        match self {
            ReadinessProfile::LinuxGuest | ReadinessProfile::AndroidLinuxMode => LINUX_LEVELS,
            ReadinessProfile::AndroidWaydroid => WAYDROID_LEVELS,
        }
    }

    pub fn accepts(self, level: GuestReadinessLevel) -> bool {
        self.levels().contains(&level)
    }

    /// Strongest level this profile ever reaches.
    pub fn terminal_level(self) -> GuestReadinessLevel {
        match self {
            ReadinessProfile::LinuxGuest | ReadinessProfile::AndroidLinuxMode => {
                GuestReadinessLevel::GuestOsUp
            }
            ReadinessProfile::AndroidWaydroid => GuestReadinessLevel::WaydroidReady,
        }
    }
}

/// Answer of one readiness probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeOutcome {
    /// The probe measured the level and the guest reports it reached it.
    Ready,
    /// The probe measured the level and the guest reports it has not reached
    /// it (yet).
    NotReady,
    /// The level could not be measured at all — no probe answered, the guest
    /// carries no reporter for it, or the profile never reaches it. Distinct
    /// from `NotReady`: it is never recorded as reached and never as failed.
    Unobservable,
}

/// A run's position on the readiness ladder.
///
/// Monotonic by construction: only a `Ready` probe moves the position, only
/// upwards in the profile's own level order, and never onto a level the
/// profile cannot reach. A probe that flips back to `NotReady`, a level
/// re-observed, or an out-of-order observation of a stronger level can
/// therefore never move a run backwards — and a stronger level observed
/// implies the weaker ones, exactly as the contract's ladder states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadinessLadder {
    profile: ReadinessProfile,
    current: Option<GuestReadinessLevel>,
}

impl ReadinessLadder {
    pub fn new(profile: ReadinessProfile) -> Self {
        ReadinessLadder {
            profile,
            current: None,
        }
    }

    /// Applies one probe answer; returns the level when this observation
    /// advanced the ladder.
    pub fn observe(
        &mut self,
        level: GuestReadinessLevel,
        outcome: ProbeOutcome,
    ) -> Option<GuestReadinessLevel> {
        if outcome != ProbeOutcome::Ready || !self.profile.accepts(level) {
            return None;
        }
        let reached = self.snapshot().reached_slot();
        let observed = self
            .profile
            .levels()
            .iter()
            .position(|candidate| *candidate == level);
        if observed > reached {
            self.current = Some(level);
            return Some(level);
        }
        None
    }

    pub fn snapshot(&self) -> ReadinessSnapshot {
        ReadinessSnapshot {
            profile: self.profile,
            current: self.current,
        }
    }
}

/// What a consumer reads from a run's ladder: the profile it is derived
/// from, the level reached so far, and (through the profile) the strongest
/// level that profile can reach at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadinessSnapshot {
    pub profile: ReadinessProfile,
    pub current: Option<GuestReadinessLevel>,
}

impl ReadinessSnapshot {
    /// Strongest level this snapshot's profile can reach.
    pub fn terminal(&self) -> GuestReadinessLevel {
        self.profile.terminal_level()
    }

    pub fn at_terminal(&self) -> bool {
        self.current == Some(self.terminal())
    }

    /// Levels above the reached one, weakest first — the levels a probe pass
    /// still has to observe. Empty once the profile's terminal level is
    /// reached, and empty for a level the profile cannot reach.
    pub fn pending(&self) -> impl Iterator<Item = GuestReadinessLevel> {
        self.profile
            .levels()
            .iter()
            .copied()
            .skip(self.reached_slot().map_or(0, |slot| slot + 1))
    }

    fn reached_slot(&self) -> Option<usize> {
        let current = self.current?;
        self.profile
            .levels()
            .iter()
            .position(|candidate| *candidate == current)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventLogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// Identifier of a long-running operation (uuid string, generated by the
/// daemon at enqueue time).
pub type OpId = String;

/// A long-running operation with phased progress. The daemon re-publishes the
/// same `Operation` under `EventKind::Operation` as it advances; phase/
/// progress bookkeeping lives in the operation runner (supervisor, phases
/// 2-3), this is the wire shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub op_id: OpId,
    pub instance_id: InstanceId,
    pub kind: OperationKind,
    /// (phase name, weight) pairs; weights sum to <= 1.0.
    pub phases: Vec<(String, f32)>,
    /// Overall progress, 0.0..=1.0.
    pub progress: f32,
    /// Name of the phase the runner entered; `None` before the first phase.
    /// Absent in events stored by an older daemon.
    #[serde(default)]
    pub current_phase: Option<String>,
    pub state: OperationState,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationKind {
    Create,
    Clone,
    Export,
    Start,
    Stop,
    SnapshotCreate,
    SnapshotRestore,
    SnapshotDelete,
    Compact,
    GuestInstall,
    GuestRemove,
    DiskCreate,
    DiskResize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl DaemonEvent {
    /// Serializes to a single-line JSON record for the per-instance
    /// `events.jsonl` audit trail (append-only, rotated by size).
    pub fn to_jsonl_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

impl Operation {
    pub fn new(kind: OperationKind, instance_id: InstanceId, op_id: OpId) -> Self {
        Operation {
            op_id,
            instance_id,
            kind,
            phases: Vec::new(),
            progress: 0.0,
            current_phase: None,
            state: OperationState::Queued,
            error: None,
        }
    }

    pub fn finished(&self) -> bool {
        matches!(
            self.state,
            OperationState::Done | OperationState::Failed | OperationState::Cancelled
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::android_profile::{AndroidProfile, AndroidVersion, ArmTranslator};
    use crate::config::CdromBus;
    use std::path::PathBuf;

    fn sample_event() -> DaemonEvent {
        DaemonEvent {
            ts_ms: 1_700_000_000_000,
            instance_id: Some(InstanceId::new()),
            kind: EventKind::Lifecycle {
                from: InstanceState::Starting,
                to: InstanceState::Running,
                reason: None,
            },
        }
    }

    #[test]
    fn event_round_trips_through_json() {
        let event = sample_event();
        let json = serde_json::to_string(&event).expect("serialize");
        let restored: DaemonEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, event);
    }

    #[test]
    fn operation_round_trips_through_json() {
        let op = Operation {
            op_id: "op-1".to_string(),
            instance_id: InstanceId::new(),
            kind: OperationKind::SnapshotCreate,
            phases: vec![("freeze".to_string(), 0.3), ("commit".to_string(), 0.7)],
            progress: 0.3,
            current_phase: Some("commit".to_string()),
            state: OperationState::Running,
            error: None,
        };
        let json = serde_json::to_string(&op).expect("serialize");
        let restored: Operation = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, op);
        assert!(!op.finished());
    }

    #[test]
    fn operation_without_a_current_phase_deserializes() {
        let json = r#"{
            "op_id": "op-1",
            "instance_id": "0000000000000000000000000000000000000000000000000000000000000000",
            "kind": "SnapshotCreate",
            "phases": [["freeze", 0.3], ["commit", 0.7]],
            "progress": 0.3,
            "state": "Running",
            "error": null
        }"#;
        let restored: Operation = serde_json::from_str(json).expect("deserialize");
        assert_eq!(restored.current_phase, None);
    }

    #[test]
    fn operation_states_are_serializable_and_ordered() {
        let json = serde_json::to_string(&GuestReadinessLevel::WaydroidReady).unwrap();
        assert_eq!(json, "\"WaydroidReady\"");
        assert!(GuestReadinessLevel::SerialUp < GuestReadinessLevel::WaydroidReady);
    }

    fn android_kind(boot_mode: AndroidBootMode) -> InstanceKind {
        InstanceKind::AndroidVm {
            android_profile: AndroidProfile {
                android_version: AndroidVersion::Android13,
                gapps: false,
                microg: false,
                arm_translator: ArmTranslator::None,
                boot_mode,
                base_image_pin: None,
            },
        }
    }

    #[test]
    fn profile_is_derived_from_kind_and_boot_mode() {
        let linux = InstanceKind::LinuxVm {
            iso_path: PathBuf::from("/isos/cachyos.iso"),
            cdrom_bus: CdromBus::Ide,
        };
        assert_eq!(ReadinessProfile::of(&linux), ReadinessProfile::LinuxGuest);
        assert_eq!(
            ReadinessProfile::of(&android_kind(AndroidBootMode::Linux)),
            ReadinessProfile::AndroidLinuxMode
        );
        assert_eq!(
            ReadinessProfile::of(&android_kind(AndroidBootMode::Android)),
            ReadinessProfile::AndroidWaydroid
        );
    }

    #[test]
    fn terminal_level_follows_the_effective_profile_not_the_kind() {
        let android = android_kind(AndroidBootMode::Linux);
        assert!(matches!(android, InstanceKind::AndroidVm { .. }));
        assert_eq!(
            ReadinessProfile::of(&android).terminal_level(),
            GuestReadinessLevel::GuestOsUp,
            "an Android VM in Linux boot mode never brings up Waydroid"
        );
        assert_eq!(
            ReadinessProfile::of(&android_kind(AndroidBootMode::Android)).terminal_level(),
            GuestReadinessLevel::WaydroidReady
        );
    }

    #[test]
    fn a_profiles_levels_end_at_its_terminal_level() {
        for profile in [
            ReadinessProfile::LinuxGuest,
            ReadinessProfile::AndroidLinuxMode,
            ReadinessProfile::AndroidWaydroid,
        ] {
            let levels = profile.levels();
            assert_eq!(
                levels.last().copied(),
                Some(profile.terminal_level()),
                "{profile:?} levels must end at its terminal level"
            );
            assert!(profile.accepts(profile.terminal_level()));
            assert!(profile.accepts(GuestReadinessLevel::SerialUp));
            assert_eq!(
                profile.accepts(GuestReadinessLevel::WaydroidReady),
                profile == ReadinessProfile::AndroidWaydroid,
                "only the waydroid profile can ever report WaydroidReady"
            );
        }
    }

    #[test]
    fn ladder_only_advances_on_a_ready_probe() {
        let mut ladder = ReadinessLadder::new(ReadinessProfile::LinuxGuest);
        assert_eq!(ladder.snapshot().current, None);

        assert_eq!(
            ladder.observe(GuestReadinessLevel::QgaUp, ProbeOutcome::NotReady),
            None
        );
        assert_eq!(
            ladder.observe(GuestReadinessLevel::QgaUp, ProbeOutcome::Unobservable),
            None
        );
        assert_eq!(ladder.snapshot().current, None);

        assert_eq!(
            ladder.observe(GuestReadinessLevel::QgaUp, ProbeOutcome::Ready),
            Some(GuestReadinessLevel::QgaUp)
        );
        assert_eq!(ladder.snapshot().current, Some(GuestReadinessLevel::QgaUp));
    }

    #[test]
    fn ladder_never_goes_backwards() {
        let mut ladder = ReadinessLadder::new(ReadinessProfile::AndroidWaydroid);
        ladder.observe(GuestReadinessLevel::QgaUp, ProbeOutcome::Ready);
        ladder.observe(GuestReadinessLevel::DisplayApplied, ProbeOutcome::Ready);
        ladder.observe(GuestReadinessLevel::GuestOsUp, ProbeOutcome::Ready);
        assert_eq!(
            ladder.snapshot().current,
            Some(GuestReadinessLevel::GuestOsUp)
        );

        // A probe that answered Ready and now answers NotReady is a flap, not
        // a regression: the level a run reached is a fact about that run.
        assert_eq!(
            ladder.observe(GuestReadinessLevel::GuestOsUp, ProbeOutcome::NotReady),
            None
        );
        assert_eq!(
            ladder.observe(GuestReadinessLevel::QgaUp, ProbeOutcome::NotReady),
            None
        );
        assert_eq!(
            ladder.snapshot().current,
            Some(GuestReadinessLevel::GuestOsUp)
        );
        // Re-observing a reached level is not an advance either.
        assert_eq!(
            ladder.observe(GuestReadinessLevel::QgaUp, ProbeOutcome::Ready),
            None
        );
    }

    #[test]
    fn a_stronger_level_observed_implies_the_weaker_ones() {
        let mut ladder = ReadinessLadder::new(ReadinessProfile::LinuxGuest);
        assert_eq!(
            ladder.observe(GuestReadinessLevel::GuestOsUp, ProbeOutcome::Ready),
            Some(GuestReadinessLevel::GuestOsUp),
            "the ladder's levels imply each other, so the OS being up is a \
             stronger statement than the display being applied"
        );
        assert!(ladder.snapshot().at_terminal());
        assert_eq!(ladder.snapshot().pending().count(), 0);
    }

    #[test]
    fn a_level_the_profile_cannot_reach_is_never_recorded() {
        let mut ladder = ReadinessLadder::new(ReadinessProfile::LinuxGuest);
        assert_eq!(
            ladder.observe(GuestReadinessLevel::WaydroidReady, ProbeOutcome::Ready),
            None,
            "WaydroidReady is uncountable for a Linux guest"
        );
        assert_eq!(ladder.snapshot().current, None);

        let mut ladder = ReadinessLadder::new(ReadinessProfile::AndroidLinuxMode);
        ladder.observe(GuestReadinessLevel::GuestOsUp, ProbeOutcome::Ready);
        assert_eq!(
            ladder.observe(GuestReadinessLevel::WaydroidReady, ProbeOutcome::Ready),
            None
        );
        assert_eq!(ladder.snapshot().terminal(), GuestReadinessLevel::GuestOsUp);
        assert!(ladder.snapshot().at_terminal());
    }

    #[test]
    fn pending_levels_are_above_the_reached_one_weakest_first() {
        let mut ladder = ReadinessLadder::new(ReadinessProfile::AndroidWaydroid);
        assert_eq!(
            ladder.snapshot().pending().collect::<Vec<_>>(),
            vec![
                GuestReadinessLevel::SerialUp,
                GuestReadinessLevel::QgaUp,
                GuestReadinessLevel::DisplayApplied,
                GuestReadinessLevel::GuestOsUp,
                GuestReadinessLevel::WaydroidReady,
            ]
        );
        ladder.observe(GuestReadinessLevel::DisplayApplied, ProbeOutcome::Ready);
        assert_eq!(
            ladder.snapshot().pending().collect::<Vec<_>>(),
            vec![
                GuestReadinessLevel::GuestOsUp,
                GuestReadinessLevel::WaydroidReady,
            ]
        );
    }

    #[test]
    fn readiness_event_round_trips_through_json() {
        let event = DaemonEvent {
            ts_ms: 1_700_000_000_000,
            instance_id: Some(InstanceId::new()),
            kind: EventKind::Readiness {
                level: GuestReadinessLevel::DisplayApplied,
            },
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert_eq!(
            json,
            format!(
                "{{\"ts_ms\":1700000000000,\"instance_id\":\"{}\",\
                 \"kind\":{{\"Readiness\":{{\"level\":\"DisplayApplied\"}}}}}}",
                event.instance_id.expect("instance id")
            )
        );
        let restored: DaemonEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, event);
    }
}
